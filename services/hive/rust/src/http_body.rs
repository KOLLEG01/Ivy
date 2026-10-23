//! Incoming HTTP body accounting. Admission owns and releases the request lease.
use crate::admission::RequestLease;
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};

const MAXIMUM_BUFFERED_BYTES: usize = 64 * 1024 * 1024;

pub fn require_form(value: Option<&IvyJson>, message: &str) -> IvyResult<()> {
    let valid = match value {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::String(value)) => {
            value.0.split(|v| *v == 59).next()
                == Some(
                    "application/x-www-form-urlencoded"
                        .encode_utf16()
                        .collect::<Vec<_>>()
                        .as_slice(),
                )
        }
        Some(_) => return Err(IvyError::internal()),
    };
    if valid {
        Ok(())
    } else {
        Err(IvyError::new("invalid_arguments", message))
    }
}

/// Preserve transport chunk boundaries. Closing an iterator after a rejected
/// chunk belongs to that transport; an exhausted or failed iterator is not closed
/// a second time. Dropping a pending request is the outer connection owner's job.
pub trait BodyStream {
    fn next_chunk(&mut self) -> impl Future<Output = IvyResult<Option<Vec<u8>>>>;
    fn cancel(&mut self) -> impl Future<Output = IvyResult<()>>;
}

#[derive(Clone, Default)]
pub struct BodyBuffers(Arc<Mutex<usize>>);
impl BodyBuffers {
    pub fn buffered_bytes(&self) -> usize {
        *self.0.lock().unwrap()
    }
    pub async fn read(
        &self,
        stream: &mut impl BodyStream,
        declared: Option<&IvyJson>,
        limit: usize,
        mut lease: Option<&mut RequestLease>,
    ) -> IvyResult<Vec<u8>> {
        if let Some(declared) = declared {
            let length = declared
                .as_str()
                .ok()
                .filter(|value| !value.is_empty() && value.bytes().all(|v| v.is_ascii_digit()))
                .and_then(|value| value.parse::<usize>().ok());
            if !length.is_some_and(|length| length <= limit) {
                return Err(too_large());
            }
        }
        let mut held = Buffered {
            owner: self,
            size: 0,
        };
        let mut chunks = Vec::new();
        while let Some(chunk) = stream.next_chunk().await? {
            let admitted = (|| {
                if let Some(lease) = &mut lease {
                    lease.grow(chunk.len())?;
                }
                held.size += chunk.len();
                let buffered = {
                    let mut buffered = self.0.lock().unwrap();
                    *buffered += chunk.len();
                    *buffered
                };
                if held.size > limit {
                    return Err(too_large());
                }
                if buffered > MAXIMUM_BUFFERED_BYTES {
                    return Err(IvyError::new(
                        "limit_exceeded",
                        "Hive request buffers are full.",
                    ));
                }
                Ok(())
            })();
            if let Err(error) = admitted {
                // Cancel transport reads while retaining the admission error.
                let _ = stream.cancel().await;
                return Err(error);
            }
            chunks.push(chunk);
        }
        let mut bytes = Vec::with_capacity(held.size);
        for chunk in chunks {
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}
struct Buffered<'a> {
    owner: &'a BodyBuffers,
    size: usize,
}
impl Drop for Buffered<'_> {
    fn drop(&mut self) {
        *self.owner.0.lock().unwrap() -= self.size;
    }
}
fn too_large() -> IvyError {
    IvyError::new("frame_too_large", "Request body exceeds its limit.")
}
