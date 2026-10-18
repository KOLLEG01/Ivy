//! Private length-prefixed pipes. There is no listener or public wire change.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    json::{parse, stringify},
};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// An accepted public input plus its private context fits within the existing
// 64MiB storage budget. Encode only one frame per pipe, never a JSON backlog.
pub const MAX_FRAME_BYTES: usize = crate::worker_client::MAX_QUEUED_BYTES as usize;
fn malformed() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid storage worker frame.")
}
fn ended() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "Incomplete storage worker frame.",
    )
}

/// State survives cancellation of next(), including a split header/body. The
/// actor may select a backup/progress event between any two incoming bytes.
pub struct FrameReader<R> {
    reader: R,
    header: [u8; 4],
    header_read: usize,
    body: Vec<u8>,
    body_read: usize,
    length: Option<usize>,
}
impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            header: [0; 4],
            header_read: 0,
            body: vec![],
            body_read: 0,
            length: None,
        }
    }
    pub async fn next(&mut self) -> io::Result<Option<IvyJson>> {
        while self.header_read < 4 {
            let count = self
                .reader
                .read(&mut self.header[self.header_read..])
                .await?;
            if count == 0 {
                return if self.header_read == 0 {
                    Ok(None)
                } else {
                    Err(ended())
                };
            }
            self.header_read += count;
        }
        if self.length.is_none() {
            let length = u32::from_be_bytes(self.header) as usize;
            if length == 0 || length > MAX_FRAME_BYTES {
                return Err(malformed());
            }
            self.body.resize(length, 0);
            self.length = Some(length);
        }
        while self.body_read < self.body.len() {
            let count = self.reader.read(&mut self.body[self.body_read..]).await?;
            if count == 0 {
                return Err(ended());
            }
            self.body_read += count;
        }
        let value = parse(
            std::str::from_utf8(&self.body).map_err(|_| malformed())?,
            MAX_FRAME_BYTES,
        )
        .map_err(|_| malformed())?;
        self.header_read = 0;
        self.body_read = 0;
        self.length = None;
        self.body.clear();
        Ok(Some(value))
    }
}
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, value: &IvyJson) -> io::Result<()> {
    let bytes = stringify(value, MAX_FRAME_BYTES).map_err(|_| malformed())?;
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    writer.write_all(bytes.as_bytes()).await?;
    writer.flush().await
}

// Decimal private IDs preserve every u64 sequence without JavaScript's numeric
// rounding. The child is bound to a generation by its owned pipes, not input.
fn sequence(value: &IvyJson) -> io::Result<u64> {
    let text = value.as_str().map_err(|_| malformed())?;
    let id = text.parse::<u64>().map_err(|_| malformed())?;
    if id == 0 || id.to_string() != text {
        return Err(malformed());
    }
    Ok(id)
}
pub struct Request {
    pub id: u64,
    pub payload: IvyJson,
}
impl Request {
    pub fn to_json(self) -> IvyJson {
        IvyJson::object([
            ("id", self.id.to_string().into()),
            ("payload", self.payload),
        ])
    }
    pub fn from_json(mut value: IvyJson) -> io::Result<Self> {
        let id = sequence(value.get("id").ok_or_else(malformed)?)?;
        let payload = value.remove("payload").ok_or_else(malformed)?;
        Ok(Self { id, payload })
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Notification {
    Routing(IvyJson),
    EventsAvailable(IvyJson),
    Changes(Vec<JsString>),
}
impl Notification {
    pub fn to_json(self) -> IvyJson {
        match self {
            Self::Routing(value) => IvyJson::object([("routing", value)]),
            Self::EventsAvailable(value) => IvyJson::object([("eventsAvailable", value)]),
            Self::Changes(value) => IvyJson::object([(
                "changes",
                IvyJson::Array(value.into_iter().map(IvyJson::String).collect()),
            )]),
        }
    }
}
pub enum Message {
    Ready,
    Fatal(IvyError),
    Started(u64),
    Progress(u64),
    Reply { id: u64, result: IvyResult<IvyJson> },
    Notification(Notification),
}
fn from_wire(value: &IvyJson) -> io::Result<IvyError> {
    let data = value.get("data").ok_or_else(malformed)?;
    let text = |value: Option<&IvyJson>| {
        value
            .and_then(IvyJson::as_js_string)
            .cloned()
            .ok_or_else(malformed)
    };
    let outcome = match data.get("outcome").and_then(|v| v.as_str().ok()).as_deref() {
        Some("not_executed") => Outcome::NotExecuted,
        Some("unknown") => Outcome::Unknown,
        Some("completed") => Outcome::Completed,
        _ => return Err(malformed()),
    };
    let mut error = IvyError::new_js(text(data.get("code"))?, text(value.get("message"))?)
        .with_outcome(outcome);
    if let Some(details) = data.get("details") {
        error = error.with_details(details.clone());
    }
    Ok(error)
}
impl Message {
    pub fn to_json(self) -> IvyJson {
        match self {
            Self::Ready => IvyJson::object([("ready", true.into())]),
            Self::Fatal(error) => IvyJson::object([("fatal", error.to_wire())]),
            Self::Started(id) => {
                IvyJson::object([("id", id.to_string().into()), ("started", true.into())])
            }
            Self::Progress(id) => {
                IvyJson::object([("id", id.to_string().into()), ("progress", true.into())])
            }
            Self::Reply { id, result } => match result {
                Ok(value) => IvyJson::object([("id", id.to_string().into()), ("value", value)]),
                Err(error) => {
                    IvyJson::object([("id", id.to_string().into()), ("error", error.to_wire())])
                }
            },
            Self::Notification(value) => value.to_json(),
        }
    }
    pub fn from_json(value: IvyJson) -> io::Result<Option<Self>> {
        if value.get("ready") == Some(&IvyJson::Bool(true)) {
            return Ok(Some(Self::Ready));
        }
        if let Some(error) = value.get("fatal") {
            return Ok(Some(Self::Fatal(from_wire(error)?)));
        }
        if let Some(value) = value.get("routing") {
            return Ok(Some(Self::Notification(Notification::Routing(
                value.clone(),
            ))));
        }
        if let Some(value) = value.get("eventsAvailable") {
            return Ok(Some(Self::Notification(Notification::EventsAvailable(
                value.clone(),
            ))));
        }
        if let Some(values) = value.get("changes") {
            let changes = values
                .as_array()
                .ok_or_else(malformed)?
                .iter()
                .map(|v| v.as_js_string().cloned().ok_or_else(malformed))
                .collect::<io::Result<_>>()?;
            return Ok(Some(Self::Notification(Notification::Changes(changes))));
        }
        let Some(id) = value.get("id") else {
            return Ok(None);
        };
        let id = sequence(id)?;
        if value.get("progress") == Some(&IvyJson::Bool(true)) {
            return Ok(Some(Self::Progress(id)));
        }
        if value.get("started") == Some(&IvyJson::Bool(true)) {
            return Ok(Some(Self::Started(id)));
        }
        let result = match value.get("error") {
            Some(error) => Err(from_wire(error)?),
            None => Ok(value.get("value").cloned().unwrap_or(IvyJson::Null)),
        };
        Ok(Some(Self::Reply { id, result }))
    }
}
