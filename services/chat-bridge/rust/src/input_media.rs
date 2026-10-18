//! Original pinned image reads; no files, URLs or replacement revisions.
use crate::admission::{AdmittedSend, ChatAdmission};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::digest};
use ivy_sdk::rpc::{RequestOptions, RpcClient};

fn header(bytes: &[u8], media_type: &str) -> bool {
    match media_type {
        "image/png" => bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]),
        "image/jpeg" => bytes.starts_with(&[255, 216, 255]),
        _ => {
            if bytes.len() < 20
                || &bytes[..4] != b"RIFF"
                || &bytes[8..12] != b"WEBP"
                || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as u64 + 8
                    != bytes.len() as u64
                || ![b"VP8 ", b"VP8L", b"VP8X"].contains(&bytes[12..16].try_into().unwrap())
            {
                return false;
            }
            let mut offset = 12_u64;
            while offset < bytes.len() as u64 {
                if bytes.len() as u64 - offset < 8 {
                    return false;
                }
                let start = offset as usize;
                let size =
                    u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap()) as u64;
                offset += 8 + size + size % 2;
                if offset > bytes.len() as u64 {
                    return false;
                }
            }
            offset == bytes.len() as u64
        }
    }
}
pub struct AdmittedNativeInput {
    pub admission: AdmittedSend,
    pub input: Vec<Json>,
}
pub async fn admitted_native_input(
    admission: &ChatAdmission,
    client: &dyn RpcClient,
    caller: &JsString,
    request: &Json,
) -> IvyResult<AdmittedNativeInput> {
    let original = request.clone();
    let admitted = admission.send(caller, &original)?;
    let payload = original.required("payload")?;
    let mut input = Vec::new();
    if !payload
        .required("text")?
        .as_js_string()
        .unwrap()
        .0
        .is_empty()
    {
        input.push(Json::object([
            ("type", "text".into()),
            ("text", payload.required("text")?.clone()),
        ]));
    }
    for image in payload.required("images")?.as_array().unwrap() {
        let pin = image.required("object")?;
        let value = client
            .request("objects.read", pin.clone(), RequestOptions::default())
            .await?;
        let object = value.required("object")?;
        let revision = value.required("revision")?;
        let content = value.required("content")?;
        let media_type = image.required("mediaType")?.as_str().unwrap();
        let key = match media_type.as_str() {
            "image/png" => "chat-bridge/image-png",
            "image/jpeg" => "chat-bridge/image-jpeg",
            _ => "chat-bridge/image-webp",
        };
        if object.get("id") != pin.get("objectId")
            || revision.get("revision") != pin.get("revision")
            || object.get("contractKey") != Some(&key.into())
            || revision.get("contractVersion") != Some(&"1.0.0".into())
            || revision.get("mediaType") != image.get("mediaType")
            || content.get("encoding") != Some(&"base64".into())
        {
            return Err(IvyError::new(
                "chat_image_mismatch",
                "The selected immutable image revision does not have its exact supported media contract.",
            ));
        }
        let mismatch = || {
            IvyError::new(
                "chat_image_mismatch",
                "The image bytes differ from their declared immutable hash or length.",
            )
        };
        let encoded = content.required("value")?.as_str().map_err(|_| mismatch())?;
        // Buffer.from is permissive, but the original immediately requires exact
        // canonical re-encoding. Strict decoding rejects the same wire inputs.
        let bytes = STANDARD.decode(&encoded).map_err(|_| mismatch())?;
        if STANDARD.encode(&bytes) != encoded
            || image.get("byteLength") != Some(&Json::Number(bytes.len() as f64))
            || revision.get("byteLength") != image.get("byteLength")
            || image.get("contentHash") != Some(&digest(&bytes).into())
            || revision.get("contentHash") != image.get("contentHash")
        {
            return Err(mismatch());
        }
        if !header(&bytes, &media_type) {
            return Err(IvyError::new(
                "chat_image_invalid",
                "The image signature or complete container header is invalid.",
            ));
        }
        input.push(Json::object([
            ("type", "image".into()),
            ("url", format!("data:{media_type};base64,{encoded}").into()),
        ]));
    }
    Ok(AdmittedNativeInput {
        admission: admitted,
        input,
    })
}
