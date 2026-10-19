//! Incoming HTTP body accounting. Admission owns and releases the request lease.
use crate::admission::RequestLease;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{number_from_string, try_js_string},
};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};

const MAXIMUM_BUFFERED_BYTES: f64 = 64.0 * 1024.0 * 1024.0;

/// Preserve transport chunk boundaries. Closing an iterator after a rejected
/// chunk belongs to that transport; an exhausted or failed iterator is not closed
/// a second time. Dropping a pending request is the outer connection owner's job.
pub trait BodyStream {
    fn next_chunk(&mut self) -> impl Future<Output = IvyResult<Option<Vec<u8>>>>;
    fn cancel(&mut self) -> impl Future<Output = IvyResult<()>>;
}

#[derive(Clone, Default)]
pub struct BodyBuffers(Arc<Mutex<f64>>);
impl BodyBuffers {
    pub fn buffered_bytes(&self) -> f64 {
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
            // IncomingMessage normally supplies a string. Keep the original
            // RegExp/Number coercion for an explicitly supplied JSON carrier too.
            let text = try_js_string(declared)?;
            if text.0.is_empty()
                || !text.0.iter().all(|v| (48..=57).contains(v))
                || !(number_from_string(&text) <= limit as f64)
            {
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
                    lease.grow(chunk.len() as f64)?;
                }
                held.size += chunk.len();
                let buffered = {
                    let mut buffered = self.0.lock().unwrap();
                    *buffered += chunk.len() as f64;
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
                // AsyncIteratorClose preserves the original thrown completion.
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
        *self.owner.0.lock().unwrap() -= self.size as f64;
    }
}
fn too_large() -> IvyError {
    IvyError::new("frame_too_large", "Request body exceeds its limit.")
}
