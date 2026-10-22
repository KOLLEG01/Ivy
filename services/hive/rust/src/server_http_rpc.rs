//! JSON-RPC route after the shared HTTP authentication/cookie prefix.
use crate::{
    http_body::BodyStream,
    http_common::json_body,
    http_response::{self, Response},
    rpc::valid_request_id,
    server_http::Request,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, limits::MANAGEMENT_FRAME_BYTES};

pub struct Headers<'a> {
    pub content_type: Option<&'a IvyJson>,
    pub content_length: Option<&'a IvyJson>,
}
/// An unmatched route has no effects. The caller applies the public-access and
/// authentication/cookie prefix first, then continues other authenticated routes
/// on false. It keeps the request alive through success/error response effects.
/// rpc_id is the outer catch's correlation value; batches leave it unchanged.
pub async fn handle(
    request: &mut Request<'_>,
    method: Option<&JsString>,
    path: &JsString,
    headers: Headers<'_>,
    context: &IvyJson,
    rpc_id: &mut IvyJson,
    stream: &mut impl BodyStream,
    response: &mut impl Response,
) -> IvyResult<bool> {
    if method != Some(&JsString::from("POST")) || path != &JsString::from("/api/v1/rpc") {
        return Ok(false);
    }
    let (frame, received_bytes) = read(request, headers, stream).await?;
    if frame.as_array().is_some() {
        let body = request
            .batch(
                context.clone(),
                frame.into_array().unwrap(),
                received_bytes as f64,
            )
            .await?;
        response.write_head(
            200,
            IvyJson::object([("Content-Type", "application/json".into())]),
        )?;
        response.end(body)?;
        return Ok(true);
    }
    single(&frame)?;
    *rpc_id = valid_request_id(frame.get("id"));
    let result = request
        .invoke(context.clone(), frame, received_bytes as f64)
        .await?;
    http_response::json(
        response,
        200,
        &IvyJson::object([
            ("jsonrpc", "2.0".into()),
            ("id", rpc_id.clone()),
            ("result", result),
        ]),
    )?;
    Ok(true)
}
/// Shared source branch for RPC and MCP content admission and fatal JSON decode.
pub(crate) async fn read(
    request: &mut Request<'_>,
    headers: Headers<'_>,
    stream: &mut impl BodyStream,
) -> IvyResult<(IvyJson, usize)> {
    let json = match headers.content_type {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::String(value)) => {
            value.0.split(|v| *v == 59).next()
                == Some(
                    "application/json"
                        .encode_utf16()
                        .collect::<Vec<_>>()
                        .as_slice(),
                )
        }
        // Unsupported header carriers fail before body iteration in the source.
        // Native transport exception identity remains a separate boundary.
        Some(_) => return Err(IvyError::internal()),
    };
    if !json {
        return Err(IvyError::new(
            "invalid_arguments",
            "RPC requires JSON content type.",
        ));
    }
    let bytes = request
        .body(stream, headers.content_length, MANAGEMENT_FRAME_BYTES)
        .await?;
    Ok((json_body(&bytes)?, bytes.len()))
}
pub(crate) fn single(frame: &IvyJson) -> IvyResult<()> {
    if !matches!(frame, IvyJson::Object(_)) {
        return Err(IvyError::new(
            "invalid_frame",
            "Expected a single request envelope.",
        ));
    }
    Ok(())
}
