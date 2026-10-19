//! Synchronous HTTP response transitions; the transport owns socket flushing.
use ivy_contracts::{IvyJson, IvyResult, json::stringify};

pub trait Response {
    fn writable_ended(&self) -> bool;
    fn destroyed(&self) -> bool;
    fn headers_sent(&self) -> bool;
    fn write_head(&mut self, status: u16, headers: IvyJson) -> IvyResult<()>;
    fn end(&mut self, text: String) -> IvyResult<()>;
    fn destroy(&mut self) -> IvyResult<()>;
}
pub fn json(response: &mut impl Response, status: u16, value: &IvyJson) -> IvyResult<()> {
    if response.writable_ended() || response.destroyed() {
        return Ok(());
    }
    if response.headers_sent() {
        return response.destroy();
    }
    response.write_head(
        status,
        IvyJson::object([("Content-Type", "application/json; charset=utf-8".into())]),
    )?;
    response.end(stringify(value, usize::MAX)?)
}
