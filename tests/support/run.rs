//! Tunnel run scaffolding for the live tests: start a tunnel, poll the
//! public hostname, and shut down with a bounded wait.
//!
//! Every run holds the exclusive live-state lock, so concurrent test
//! processes or threads never register the same tunnel twice. A failed run
//! against a cached quick tunnel invalidates the cache and retries once
//! with a freshly created tunnel; every path shuts the tunnel down before
//! returning.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(feature = "named-tunnel")]
use libcfd::NamedTunnel;
use libcfd::{EdgeConnector, EdgeOptions, Origin, RemoteConfiguration, Transport, Tunnel};
use tokio::sync::oneshot;

use super::origins::{PathEchoOrigin, WebSocketEchoOrigin};
#[cfg(feature = "named-tunnel")]
use super::state::StateLock;
#[cfg(feature = "quick-tunnel")]
use super::state::quick_tunnel;
use super::{CONFIG_WAIT, POLL_INTERVAL, POLL_TIMEOUT, SHUTDOWN_TIMEOUT};

/// A running tunnel task plus its shutdown signal.
pub struct TunnelRun {
    task: tokio::task::JoinHandle<Result<(), libcfd::Error>>,
    shutdown: oneshot::Sender<()>,
}

/// Starts a tunnel run on a new task. The caller must shut it down with
/// [`shutdown_bounded`] even on failure.
pub fn start(
    tunnel: Tunnel,
    origin: Origin,
    transport: Transport,
    on_remote_configuration: Option<Arc<dyn Fn(RemoteConfiguration) + Send + Sync>>,
) -> TunnelRun {
    let options = EdgeOptions {
        transport,
        connect_timeout: Duration::from_secs(30),
        grace_period: Duration::from_secs(60),
        backoff: Duration::from_secs(1),
        on_remote_configuration,
        ..EdgeOptions::default()
    };
    let connector = EdgeConnector::new(options);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let shutdown = async move {
        let _ = shutdown_rx.await;
    };
    let task = tokio::spawn(async move { connector.run(tunnel, origin, shutdown).await });
    TunnelRun {
        task,
        shutdown: shutdown_tx,
    }
}

/// What a live request observed on the public hostname.
#[derive(Debug)]
pub struct Observed {
    /// The last HTTP status observed.
    pub status: u16,
    /// The last response body observed.
    pub body: String,
}

/// Polls the public hostname until it answers `expected` with status 200 or
/// `POLL_TIMEOUT` elapses. Detects an early tunnel-run failure.
pub async fn poll_public(
    run: &TunnelRun,
    hostname: &str,
    path: &str,
    expected: &str,
) -> Result<Observed, String> {
    let url = format!("https://{hostname}{path}");
    let deadline = Instant::now() + POLL_TIMEOUT;
    let mut last = Observed {
        status: 0,
        body: String::new(),
    };
    while Instant::now() < deadline {
        if run.task.is_finished() {
            return Err(format!(
                "tunnel run ended before serving the request (hostname {hostname})"
            ));
        }
        match super::http::https_get(&url).await {
            Ok(response) => {
                last.status = response.status;
                last.body = String::from_utf8_lossy(&response.body).into_owned();
                if response.status == 200 && last.body.contains(expected) {
                    return Ok(last);
                }
                tracing::debug!(status = last.status, body = %last.body, "polling tunnel hostname");
            }
            Err(e) => {
                last.status = 0;
                last.body = format!("https get error: {e}");
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Err(format!(
        "public hostname never served the expected response within {POLL_TIMEOUT:?} \
         (last status {}, body {:?})",
        last.status, last.body
    ))
}

/// Sends the shutdown signal and waits for the run to stop within
/// `SHUTDOWN_TIMEOUT`, reporting the run's final error if it had one.
pub async fn shutdown_bounded(run: TunnelRun) -> Result<(), String> {
    let _ = run.shutdown.send(());
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, run.task).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(e))) => Err(format!("tunnel run failed during shutdown: {e}")),
        Ok(Err(e)) => Err(format!("tunnel run task panicked: {e}")),
        Err(_) => Err(format!(
            "tunnel did not shut down within {SHUTDOWN_TIMEOUT:?}"
        )),
    }
}

/// The outcome of one live attempt, distinguishing stale cached state from
/// a real failure.
pub enum QuickAttempt<T = ()> {
    /// The attempt succeeded.
    Success(T),
    /// A cached quick tunnel failed; its state was invalidated so a fresh
    /// tunnel will be created on the next attempt.
    StaleRetry(String),
}

/// Runs `attempt`, retrying once with a freshly created tunnel when a
/// cached quick tunnel fails. Quick tunnels are replaced at most once.
pub async fn retry_on_stale_cached<T, F, Fut>(mut attempt: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<QuickAttempt<T>, String>>,
{
    match attempt().await {
        Ok(QuickAttempt::Success(value)) => Ok(value),
        Ok(QuickAttempt::StaleRetry(first)) => match attempt().await {
            Ok(QuickAttempt::Success(value)) => Ok(value),
            Ok(QuickAttempt::StaleRetry(second)) => Err(format!(
                "cached quick tunnel failed ({first}); replacement tunnel failed too ({second})"
            )),
            Err(second) => Err(format!(
                "cached quick tunnel failed ({first}); replacement tunnel failed ({second})"
            )),
        },
        Err(e) => Err(e),
    }
}

/// Runs one quick-tunnel HTTP live attempt and always shuts the run down.
#[cfg(feature = "quick-tunnel")]
async fn attempt_quick_http(
    transport: Transport,
    label: &'static str,
    path: &'static str,
) -> Result<QuickAttempt, String> {
    let session = quick_tunnel().await;
    let hostname = session.tunnel.hostname.clone();
    let handler = PathEchoOrigin::new(label);
    let run = start(
        Tunnel::quick(session.tunnel.clone()),
        Origin::http(handler.clone()),
        transport,
        None,
    );
    let expected = format!("{label}:{path}");
    let polled = poll_public(&run, &hostname, path, &expected).await;
    let served = handler.served();
    let shutdown = shutdown_bounded(run).await;
    match (polled, shutdown) {
        (Ok(observed), Ok(())) => {
            assert_eq!(
                observed.body, expected,
                "origin response should echo the request path"
            );
            assert!(served >= 1, "the origin handler was never invoked");
            tracing::info!(%hostname, status = observed.status, "quick tunnel served the origin response");
            Ok(QuickAttempt::Success(()))
        }
        (Err(e), Ok(())) if session.cached => {
            tracing::warn!(%hostname, "cached quick tunnel failed: {e}; one fresh tunnel will be created");
            super::state::invalidate_quick();
            Ok(QuickAttempt::StaleRetry(e))
        }
        (Err(e), _) => Err(e),
        (Ok(_), Err(e)) => Err(e),
    }
}

/// Quick tunnel over the given transport with an HTTP origin: verifies the
/// public HTTPS status, response body, request path, origin invocation, and
/// bounded shutdown. Retries once with a fresh tunnel when the cached one
/// proves stale.
#[cfg(feature = "quick-tunnel")]
pub async fn quick_http_live_test(
    transport: Transport,
    label: &'static str,
    path: &'static str,
) -> Result<(), String> {
    retry_on_stale_cached(|| attempt_quick_http(transport, label, path)).await
}

/// Runs one quick-tunnel websocket live attempt and always shuts the run
/// down.
#[cfg(feature = "quick-tunnel")]
async fn attempt_quick_ws(
    transport: Transport,
    label: &'static str,
) -> Result<QuickAttempt, String> {
    let session = quick_tunnel().await;
    let hostname = session.tunnel.hostname.clone();
    let run = start(
        Tunnel::quick(session.tunnel.clone()),
        Origin::http(PathEchoOrigin::new(label)).with_websocket(WebSocketEchoOrigin),
        transport,
        None,
    );
    let ws_result = super::ws::websocket_echo_round_trip(&hostname, "/ws").await;
    let shutdown = shutdown_bounded(run).await;
    match (ws_result, shutdown) {
        (Ok(()), Ok(())) => {
            tracing::info!(%hostname, "quick tunnel served a websocket echo");
            Ok(QuickAttempt::Success(()))
        }
        (Err(e), Ok(())) if session.cached => {
            tracing::warn!(%hostname, "cached quick tunnel websocket failed: {e}; one fresh tunnel will be created");
            super::state::invalidate_quick();
            Ok(QuickAttempt::StaleRetry(e))
        }
        (Err(e), _) => Err(e),
        (Ok(_), Err(e)) => Err(e),
    }
}

/// Quick tunnel over the given transport with a websocket origin.
#[cfg(feature = "quick-tunnel")]
pub async fn quick_ws_live_test(transport: Transport, label: &'static str) -> Result<(), String> {
    retry_on_stale_cached(|| attempt_quick_ws(transport, label)).await
}

/// Collects the routed hostnames and services the edge pushes for a
/// remotely-managed tunnel.
pub struct RemoteHostnames {
    names: Arc<Mutex<Vec<String>>>,
    services: Arc<Mutex<Vec<String>>>,
}

impl RemoteHostnames {
    /// Creates an empty collector.
    pub fn new() -> Self {
        Self {
            names: Arc::new(Mutex::new(Vec::new())),
            services: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The hostnames collected so far.
    pub fn names(&self) -> Vec<String> {
        self.names.lock().expect("hostname collector lock").clone()
    }

    /// The services collected so far, parallel to [`Self::names`].
    pub fn services(&self) -> Vec<String> {
        self.services
            .lock()
            .expect("service collector lock")
            .clone()
    }

    /// The first hostname whose ingress service is a `tcp://` route.
    pub fn tcp_route(&self) -> Option<(String, String)> {
        let names = self.names.lock().expect("hostname collector lock");
        let services = self.services.lock().expect("service collector lock");
        names
            .iter()
            .zip(services.iter())
            .find(|(_, service)| service.starts_with("tcp://"))
            .map(|(name, service)| (name.clone(), service.clone()))
    }

    /// The callback handed to `EdgeOptions::on_remote_configuration`.
    pub fn callback(&self) -> Arc<dyn Fn(RemoteConfiguration) + Send + Sync> {
        let names = self.names.clone();
        let services = self.services.clone();
        Arc::new(move |configuration: RemoteConfiguration| {
            let mut names = names.lock().expect("hostname collector lock");
            names.extend(configuration.hostnames);
            let mut services = services.lock().expect("service collector lock");
            services.extend(configuration.services);
        })
    }

    /// Waits for the edge to push at least one routed hostname. Fails with
    /// a clear prerequisite message when the tunnel is not remotely managed
    /// or has no ingress routes.
    pub async fn first_hostname(&self, run: &TunnelRun) -> Result<String, String> {
        let deadline = Instant::now() + CONFIG_WAIT;
        loop {
            {
                let names = self.names.lock().expect("hostname collector lock");
                if let Some(hostname) = names.iter().find(|name| !name.is_empty()) {
                    return Ok(hostname.clone());
                }
            }
            if run.task.is_finished() {
                return Err(
                    "tunnel run ended before the edge pushed a remote configuration".to_string(),
                );
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "no routed hostname arrived within {CONFIG_WAIT:?}; the tunnel must be \
                     remotely managed with an ingress route, or the edge rejected the \
                     registration (verify that NAMED_TUNNEL_TOKEN belongs to a live tunnel \
                     the account owns)"
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// Loads the named tunnel from the token, writes the normalized credentials,
/// and returns the file-loader result.
#[cfg(feature = "named-tunnel")]
fn named_tunnel_loaded() -> Result<NamedTunnel, String> {
    let token_tunnel = super::state::named_tunnel_from_token();
    let path = super::state::write_named_credentials(&token_tunnel);
    let loaded = NamedTunnel::from_credentials_file(&path)
        .map_err(|e| format!("normalized credentials file failed to load: {e}"))?;
    assert_eq!(
        loaded.tunnel_identifier_bytes().unwrap(),
        token_tunnel.tunnel_identifier_bytes().unwrap(),
        "file-loader tunnel must match the token tunnel"
    );
    Ok(loaded)
}

/// Runs one named-tunnel HTTP live attempt over the given transport with a
/// remotely-managed configuration.
#[cfg(feature = "named-tunnel")]
async fn attempt_named_http(transport: Transport, label: &'static str) -> Result<(), String> {
    let _lock = named_session().await?;
    let hostnames = RemoteHostnames::new();
    let handler = PathEchoOrigin::new(label);
    let run = start(
        Tunnel::named(named_tunnel_loaded()?),
        Origin::http(handler.clone()),
        transport,
        Some(hostnames.callback()),
    );
    let hostname = hostnames.first_hostname(&run).await?;
    let path = "/named";
    let expected = format!("{label}:{path}");
    let polled = poll_public(&run, &hostname, path, &expected).await;
    let served = handler.served();
    let shutdown = shutdown_bounded(run).await;
    match (polled, shutdown) {
        (Ok(observed), Ok(())) => {
            assert_eq!(
                observed.body, expected,
                "origin response should echo the request path"
            );
            assert!(served >= 1, "the origin handler was never invoked");
            tracing::info!(%hostname, status = observed.status, "named tunnel served the origin response");
            Ok(())
        }
        (Err(e), _) => Err(e),
        (Ok(_), Err(e)) => Err(e),
    }
}

/// Acquires the live lock for a named tunnel run.
#[cfg(feature = "named-tunnel")]
async fn named_session() -> Result<StateLock, String> {
    let lock = StateLock::acquire().expect("acquire live state lock");
    Ok(lock)
}

/// Named tunnel over the given transport: verifies the remote configuration
/// callback supplies routed hostnames, a public request to the first
/// hostname reaches the origin, and the run shuts down cleanly.
#[cfg(feature = "named-tunnel")]
pub async fn named_http_live_test(transport: Transport, label: &'static str) -> Result<(), String> {
    attempt_named_http(transport, label).await
}

/// Named tunnel websocket round trip over the given transport. Fails with a
/// clear prerequisite message when the routed hostname does not serve a
/// websocket echo.
#[cfg(feature = "named-tunnel")]
pub async fn named_ws_live_test(transport: Transport, label: &'static str) -> Result<(), String> {
    let _lock = named_session().await?;
    let hostnames = RemoteHostnames::new();
    let run = start(
        Tunnel::named(named_tunnel_loaded()?),
        Origin::http(PathEchoOrigin::new(label)).with_websocket(WebSocketEchoOrigin),
        transport,
        Some(hostnames.callback()),
    );
    let hostname = hostnames.first_hostname(&run).await?;
    let ws_result = super::ws::websocket_echo_round_trip(&hostname, "/ws").await;
    let shutdown = shutdown_bounded(run).await;
    match (ws_result, shutdown) {
        (Ok(()), Ok(())) => {
            tracing::info!(%hostname, "named tunnel served a websocket echo");
            Ok(())
        }
        (Err(e), _) => Err(format!(
            "{e} (the routed hostname {hostname} must serve websockets; configure a \
             websocket-capable route for this tunnel)"
        )),
        (Ok(_), Err(e)) => Err(e),
    }
}

/// Named tunnel TCP route round trip over the given transport.
///
/// Cloudflare exposes a tunnel's `tcp://` ingress route as a websocket
/// connection, so the run carries a websocket origin handler and the client
/// speaks websocket frames that carry raw bytes. The route hostname is
/// discovered from the remote configuration's `tcp://` service; a tunnel
/// without one produces a clear prerequisite failure.
#[cfg(feature = "named-tunnel")]
pub async fn named_tcp_live_test(transport: Transport, label: &'static str) -> Result<(), String> {
    let _lock = named_session().await?;
    let hostnames = RemoteHostnames::new();
    let run = start(
        Tunnel::named(named_tunnel_loaded()?),
        Origin::http(PathEchoOrigin::new(label)).with_websocket(WebSocketEchoOrigin),
        transport,
        Some(hostnames.callback()),
    );
    let _ = hostnames.first_hostname(&run).await?;
    let Some((hostname, service)) = hostnames.tcp_route() else {
        let error = format!(
            "the remote configuration exposes no tcp:// ingress route (services {:?}); \
             configure a tcp route for this tunnel",
            hostnames.services()
        );
        shutdown_bounded(run).await?;
        return Err(error);
    };
    tracing::info!(%hostname, %service, "tcp route discovered from remote configuration");
    let tcp_result = super::ws::websocket_echo_round_trip(&hostname, "/").await;
    let shutdown = shutdown_bounded(run).await;
    match (tcp_result, shutdown) {
        (Ok(()), Ok(())) => {
            tracing::info!(%hostname, "named tunnel served bytes through the tcp route");
            Ok(())
        }
        (Err(e), _) => Err(format!(
            "{e} (the tcp route {hostname} must accept websocket connections carrying raw \
             tcp bytes)"
        )),
        (Ok(_), Err(e)) => Err(e),
    }
}
