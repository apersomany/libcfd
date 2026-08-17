//! Tunnel state for the live tests.
//!
//! Quick-tunnel credentials are cached in `tests/state/quick_tunnel.json`
//! and reused until they stop resolving. Named-tunnel tokens are normalized
//! into `tests/state/named_tunnel.json` so the file-loader path is
//! exercised. Every live run holds an exclusive lock over
//! `tests/state/.live.lock` so concurrent test processes cannot create or
//! register duplicate tunnels.
//!
//! Secrets are never printed or logged: only hostnames and account tags are
//! traced.

use std::io;
use std::time::{Duration, Instant};

#[cfg(feature = "named-tunnel")]
use std::path::PathBuf;

#[cfg(feature = "quick-tunnel")]
use libcfd::{QuickTunnel, QuickTunnelOptions, create_quick_tunnel};

use super::{DNS_WAIT, POLL_INTERVAL};

/// The state directory, relative to the crate root.
pub const STATE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/state");
const QUICK_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/state/quick_tunnel.json");
const NAMED_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/state/named_tunnel.json");
const LOCK_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/state/.live.lock");

/// An exclusive advisory lock over the live state, held for the duration of
/// a tunnel run so concurrent test processes (or threads) cannot race to
/// create or register tunnels.
pub struct StateLock {
    file: std::fs::File,
}

impl StateLock {
    /// Acquires the lock, blocking until it is free.
    pub fn acquire() -> io::Result<Self> {
        std::fs::create_dir_all(STATE_DIR)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(LOCK_FILE)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// Whether the hostname currently resolves to at least one address.
pub async fn hostname_resolves(hostname: &str) -> bool {
    resolve_host(hostname)
        .await
        .is_ok_and(|ips| !ips.is_empty())
}

/// Resolves a hostname to addresses, trying the system resolver first and
/// falling back to Cloudflare's public DNS-over-HTTPS when the local
/// resolver fails (e.g. a broken VPN proxy). Test-only: the library always
/// uses the system resolver.
pub async fn resolve_host(hostname: &str) -> Result<Vec<std::net::IpAddr>, String> {
    if let Ok(addrs) = tokio::net::lookup_host((hostname, 0)).await {
        let ips: Vec<std::net::IpAddr> = addrs.map(|addr| addr.ip()).collect();
        if !ips.is_empty() {
            return Ok(ips);
        }
    }
    doh_lookup(hostname).await
}

/// Queries Cloudflare's public DNS-over-HTTPS endpoint directly (the IP is
/// literal, so no resolution is involved and there is no recursion).
async fn doh_lookup(hostname: &str) -> Result<Vec<std::net::IpAddr>, String> {
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let server_name = rustls_pki_types::ServerName::try_from("1.1.1.1".to_string())
        .map_err(|e| format!("invalid doh server name: {e}"))?;
    let mut store = rustls::RootCertStore::empty();
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let configuration = rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(configuration));
    let tcp = tokio::net::TcpStream::connect((std::net::IpAddr::from([1, 1, 1, 1]), 443))
        .await
        .map_err(|e| format!("doh connect: {e}"))?;
    let mut stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("doh tls: {e}"))?;
    let request = format!(
        "GET /dns-query?name={hostname}&type=A HTTP/1.1\r\nHost: 1.1.1.1\r\n\
         Accept: application/dns-json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("doh write: {e}"))?;
    let mut body = Vec::new();
    stream
        .read_to_end(&mut body)
        .await
        .map_err(|e| format!("doh read: {e}"))?;
    let header_end = body
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "doh response has no headers".to_string())?;
    let parsed: serde_json::Value = serde_json::from_slice(&body[header_end + 4..])
        .map_err(|e| format!("doh response is not json: {e}"))?;
    let mut ips = Vec::new();
    if let Some(answer) = parsed.get("Answer").and_then(|a| a.as_array()) {
        for entry in answer {
            if let Some(data) = entry.get("data").and_then(|d| d.as_str())
                && let Ok(ip) = data.parse::<std::net::IpAddr>()
            {
                ips.push(ip);
            }
        }
    }
    if ips.is_empty() {
        return Err(format!("doh found no addresses for {hostname}"));
    }
    Ok(ips)
}

/// Polls hostname resolution until it succeeds or `DNS_WAIT` elapses.
/// Returns whether the hostname resolves.
pub async fn wait_for_resolution(hostname: &str) -> bool {
    let deadline = Instant::now() + DNS_WAIT;
    while Instant::now() < deadline {
        if hostname_resolves(hostname).await {
            return true;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    tracing::warn!(%hostname, "hostname still not resolving after {DNS_WAIT:?}");
    false
}

/// Writes bytes through a temporary file followed by an atomic rename, with
/// owner-only permissions.
pub(crate) fn atomic_write(path: &str, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(STATE_DIR)?;
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)
}

#[cfg(feature = "quick-tunnel")]
/// A quick tunnel ready for a live run, holding the state lock.
pub struct QuickTunnelSession {
    /// The tunnel to run.
    pub tunnel: QuickTunnel,
    /// Whether the tunnel came from the cached credentials file.
    pub cached: bool,
    _lock: StateLock,
}

#[cfg(feature = "quick-tunnel")]
fn load_quick_tunnel() -> Option<QuickTunnel> {
    let bytes = std::fs::read(QUICK_FILE).ok()?;
    let tunnel: QuickTunnel = serde_json::from_slice(&bytes).ok()?;
    if tunnel.tunnel_identifier.is_empty()
        || tunnel.hostname.is_empty()
        || tunnel.account_tag.is_empty()
        || tunnel.secret.is_empty()
        || tunnel.tunnel_identifier_bytes().is_err()
    {
        tracing::warn!("cached quick tunnel credentials are incomplete; discarding");
        return None;
    }
    Some(tunnel)
}

#[cfg(feature = "quick-tunnel")]
fn save_quick_tunnel(tunnel: &QuickTunnel) {
    let json = serde_json::to_vec_pretty(tunnel).expect("serialize quick tunnel credentials");
    atomic_write(QUICK_FILE, &json).expect("write quick tunnel credentials");
}

/// Loads the cached quick tunnel if it is still usable, otherwise creates a
/// new one. Only called from the live suite.
#[cfg(feature = "quick-tunnel")]
pub async fn quick_tunnel() -> QuickTunnelSession {
    let lock = StateLock::acquire().expect("acquire live state lock");
    if let Some(tunnel) = load_quick_tunnel() {
        tracing::info!(hostname = %tunnel.hostname, "checking cached quick tunnel");
        if wait_for_resolution(&tunnel.hostname).await {
            tracing::info!(hostname = %tunnel.hostname, "reusing cached quick tunnel credentials");
            return QuickTunnelSession {
                tunnel,
                cached: true,
                _lock: lock,
            };
        }
        // Expired quick tunnels stop resolving; the saved credentials are
        // stale and cannot be reused.
        tracing::warn!(hostname = %tunnel.hostname, "cached quick tunnel no longer resolves; discarding");
        std::fs::remove_file(QUICK_FILE).ok();
    }
    tracing::info!("no usable quick tunnel credentials; creating one");
    let tunnel = create_quick_tunnel(&QuickTunnelOptions::default())
        .await
        .expect("quick tunnel API should create a tunnel");
    save_quick_tunnel(&tunnel);
    let resolves = wait_for_resolution(&tunnel.hostname).await;
    tracing::info!(hostname = %tunnel.hostname, resolves, "created and saved quick tunnel credentials");
    QuickTunnelSession {
        tunnel,
        cached: false,
        _lock: lock,
    }
}

/// Removes the cached quick tunnel credentials so the next run creates a
/// fresh one. Called after a cached tunnel provably fails to serve.
#[cfg(feature = "quick-tunnel")]
pub fn invalidate_quick() {
    std::fs::remove_file(QUICK_FILE).ok();
}

/// Parses the named tunnel from `NAMED_TUNNEL_TOKEN`. The live-test runner
/// sets this variable when eligible state exists and omits the named suite
/// otherwise, so a missing variable here is a loud failure, never a silent
/// pass.
#[cfg(feature = "named-tunnel")]
pub fn named_tunnel_from_token() -> libcfd::NamedTunnel {
    let token = std::env::var("NAMED_TUNNEL_TOKEN").unwrap_or_else(|_| {
        panic!(
            "NAMED_TUNNEL_TOKEN is not set; run scripts/live-test.sh (which reads \
             tests/state/named-token.txt) or export the dashboard connector token"
        )
    });
    libcfd::NamedTunnel::from_token(&token).expect("named tunnel token should parse")
}

/// Writes the normalized credentials (never the raw token) and returns the
/// file path, so the file-loader path is exercised.
#[cfg(feature = "named-tunnel")]
pub fn write_named_credentials(tunnel: &libcfd::NamedTunnel) -> PathBuf {
    let json = serde_json::to_vec_pretty(tunnel).expect("serialize named tunnel credentials");
    atomic_write(NAMED_FILE, &json).expect("write named tunnel credentials");
    PathBuf::from(NAMED_FILE)
}

/// A short bounded wait used before starting a tunnel when the state file
/// was just rewritten.
pub async fn settle(duration: Duration) {
    tokio::time::sleep(duration).await;
}
