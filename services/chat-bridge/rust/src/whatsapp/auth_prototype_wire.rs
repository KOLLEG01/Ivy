//! Private synthetic multiplexing. No generic Host process/pipe implementation.
use super::store::{encode, text, unknown};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyJson as Json, IvyResult, json::parse};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStdin, ChildStdout},
};
const FRAME: usize = 65536;
const CHUNK: usize = 45 * 1024;
#[derive(Clone, Copy, Default)]
pub struct Statistics {
    pub sent: u64,
    pub received: u64,
    pub maximum: usize,
}
pub struct Reader {
    input: ChildStdout,
    epoch: String,
    next: u64,
    stats: Arc<Mutex<Statistics>>,
}
pub struct Writer {
    output: ChildStdin,
    epoch: String,
    next: u64,
    stats: Arc<Mutex<Statistics>>,
}
pub fn pipes(
    input: ChildStdout,
    output: ChildStdin,
    epoch: &str,
) -> (Reader, Writer, Arc<Mutex<Statistics>>) {
    let stats = Arc::new(Mutex::new(Statistics::default()));
    (
        Reader {
            input,
            epoch: epoch.into(),
            next: 0,
            stats: stats.clone(),
        },
        Writer {
            output,
            epoch: epoch.into(),
            next: 0,
            stats: stats.clone(),
        },
        stats,
    )
}
impl Writer {
    pub async fn send(&mut self, correlation: &str, value: Json) -> IvyResult<()> {
        self.next = self.next.checked_add(1).ok_or_else(unknown)?;
        let bytes = encode(&value).into_bytes();
        for (part, chunk) in bytes.chunks(CHUNK).enumerate() {
            let frame = Json::object([
                ("v", Json::Number(1.0)),
                ("g", self.epoch.as_str().into()),
                ("r", correlation.into()),
                ("m", self.next.to_string().into()),
                ("p", Json::Number(part as f64)),
                ("e", ((part + 1) * CHUNK >= bytes.len()).into()),
                ("b", STANDARD.encode(chunk).into()),
            ]);
            let frame = encode(&frame).into_bytes();
            if frame.len() + 4 > FRAME {
                return Err(unknown());
            }
            self.output
                .write_all(&(frame.len() as u32).to_be_bytes())
                .await
                .map_err(|_| unknown())?;
            self.output.write_all(&frame).await.map_err(|_| unknown())?;
            let mut stats = self.stats.lock().unwrap();
            stats.sent += 1;
            stats.maximum = stats.maximum.max(frame.len() + 4);
        }
        self.output.flush().await.map_err(|_| unknown())
    }
    pub async fn close(&mut self) -> IvyResult<()> {
        self.output.shutdown().await.map_err(|_| unknown())
    }
}
impl Reader {
    pub async fn receive(&mut self) -> IvyResult<Option<(String, Json)>> {
        let expected = self.next.checked_add(1).ok_or_else(unknown)?;
        let mut bytes = Vec::new();
        let mut part = 0_u64;
        let mut correlation = None;
        loop {
            let mut header = [0; 4];
            match self.input.read(&mut header[..1]).await {
                Ok(0) if part == 0 => return Ok(None),
                Ok(1) => {}
                _ => return Err(unknown()),
            };
            self.input
                .read_exact(&mut header[1..])
                .await
                .map_err(|_| unknown())?;
            let size = u32::from_be_bytes(header) as usize;
            if size == 0 || size + 4 > FRAME {
                return Err(unknown());
            }
            let mut frame = vec![0; size];
            self.input
                .read_exact(&mut frame)
                .await
                .map_err(|_| unknown())?;
            let frame = parse(
                std::str::from_utf8(&frame).map_err(|_| unknown())?,
                usize::MAX,
            )
            .map_err(|_| unknown())?;
            if frame.get("v").and_then(Json::as_f64) != Some(1.0)
                || text(&frame, "g")? != self.epoch
                || text(&frame, "m")? != expected.to_string()
                || frame.get("p").and_then(Json::as_f64) != Some(part as f64)
            {
                return Err(unknown());
            }
            let r = text(&frame, "r")?;
            if let Some(original) = &correlation {
                if original != &r {
                    return Err(unknown());
                }
            } else {
                correlation = Some(r);
            }
            let b = text(&frame, "b")?;
            let chunk = STANDARD.decode(&b).map_err(|_| unknown())?;
            if chunk.len() > CHUNK || STANDARD.encode(&chunk) != b {
                return Err(unknown());
            }
            bytes.extend(chunk);
            {
                let mut stats = self.stats.lock().unwrap();
                stats.received += 1;
                stats.maximum = stats.maximum.max(size + 4);
            }
            let end = frame.get("e").and_then(Json::as_bool).ok_or_else(unknown)?;
            if end {
                self.next = expected;
                return Ok(Some((
                    correlation.unwrap(),
                    parse(
                        std::str::from_utf8(&bytes).map_err(|_| unknown())?,
                        usize::MAX,
                    )
                    .map_err(|_| unknown())?,
                )));
            }
            part = part.checked_add(1).ok_or_else(unknown)?;
        }
    }
}
