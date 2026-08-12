//! The registration RPC on the HTTP/2 control-stream request.

use std::sync::Arc;

use bytes::Bytes;
use h2::RecvStream;
use h2::server::SendResponse;

use super::control;
use crate::error::{Error, Result};

use super::H2Shared;
use super::stream::H2Bidi;

/// Runs the registration RPC on the control-stream request, then blocks
/// until shutdown and unregisters.
pub(crate) async fn handle_control_stream(
    request: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    shared: Arc<H2Shared>,
    reg_tx: tokio::sync::oneshot::Sender<Result<()>>,
) -> Result<()> {
    let send = match respond.send_response(http::Response::new(()), false) {
        Ok(send) => send,
        Err(e) => {
            let error = Error::H2(format!("control stream response failed: {e}"));
            let _ = reg_tx.send(Err(error));
            return Ok(());
        }
    };
    let body = request.into_body();
    let bidi = H2Bidi::new(body, send);
    let result =
        control::register_on_stream(bidi, &shared.tunnel, &shared.reg_opts, &shared.config_json)
            .await;
    let client = match result {
        Ok((_details, client)) => client,
        Err(e) => {
            let _ = reg_tx.send(Err(e));
            return Ok(());
        }
    };
    shared.registered.fire();
    let _ = reg_tx.send(Ok(()));
    shared.control_shutdown.notified().await;
    let _ = control::unregister(client, shared.grace_period).await;
    Ok(())
}
