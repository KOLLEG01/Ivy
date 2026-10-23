//! An original dialog's outbound offer. IPC timeouts never cancel its driver.
use super::{InviteExchange, SipEndpoint, SipIoError, endpoint::DialogFlow};
use rsipstack::sip::Request;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub(super) async fn run(
    endpoint: Arc<SipEndpoint>,
    request: Request,
    flow: &DialogFlow,
    media: Arc<dyn super::HostedSipMedia>,
    stopped: CancellationToken,
    renegotiating: Arc<Mutex<bool>>,
) -> Result<(), SipIoError> {
    let mut invite =
        InviteExchange::new(request.clone()).map_err(|_| SipIoError::InvalidRequest)?;
    let mut client = endpoint.dialog_client(request, flow, None)?;
    let mut selected = false;
    loop {
        let response = tokio::select! {
            response = client.next_response() => response?,
            _ = stopped.cancelled(), if selected => return client.retire().await,
        };
        let status = response.status_code.code();
        if status < 200 {
            continue;
        }
        if status >= 300 {
            // The stack owns the non-success ACK and original transaction.
            return client.retire().await;
        }
        if let Some(ack) = invite.ack_for_retransmission(&response) {
            client.send_ack(ack).await?;
            continue;
        }
        if selected {
            continue;
        }
        invite
            .select_final(&response)
            .map_err(|_| SipIoError::InvalidRequest)?;
        let body = String::from_utf8_lossy(&response.body);
        // Only an accepted SDP consumes the original negotiation window. The
        // original response path reads its body independently of Content-Type.
        // Serialize this synchronous choice so an invalid answer cannot hide a
        // concurrent valid answer while temporarily claiming the shared flag.
        {
            let mut pending = renegotiating.lock().unwrap_or_else(|p| p.into_inner());
            if *pending && !media.is_closed() && media.accept_answer(&body) {
                *pending = false;
            }
        }
        let ack = invite
            .prepare_ack(&response, None, endpoint.inner())
            .map_err(|_| SipIoError::InvalidRequest)?;
        // Never abandon the stack's original ACK send at a select boundary.
        client.send_ack(&ack).await?;
        selected = true;
    }
}
