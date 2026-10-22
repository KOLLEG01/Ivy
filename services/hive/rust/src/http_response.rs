//! Synchronous HTTP response transitions; the transport owns socket flushing.
use ivy_contracts::{IvyJson, IvyResult, JsString, json::stringify};

pub trait Response {
    fn writable_ended(&self) -> bool;
    fn destroyed(&self) -> bool;
    fn headers_sent(&self) -> bool;
    fn set_header(&mut self, name: &str, value: &JsString) -> IvyResult<()>;
    fn write_head(&mut self, status: u16, headers: IvyJson) -> IvyResult<()>;
    fn end(&mut self, text: String) -> IvyResult<()>;
    fn end_empty(&mut self) -> IvyResult<()>;
    fn destroy(&mut self) -> IvyResult<()>;
}
/// Common HTTP prefix, before request admission and route selection. Preserve
/// the original order and any partial header effects when the owner throws.
pub fn security_headers(response: &mut impl Response) -> IvyResult<()> {
    let policy = crate::ui_security::content_policy(&JsString::default());
    for (name, value) in [
        ("Content-Security-Policy", policy.as_str()),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "same-origin"),
        ("Cache-Control", "no-store"),
    ] {
        response.set_header(name, &value.into())?;
    }
    Ok(())
}
pub fn redirect(response: &mut impl Response, target: &JsString) -> IvyResult<()> {
    response.write_head(
        303,
        IvyJson::object([("Location", IvyJson::String(target.clone()))]),
    )?;
    response.end_empty()
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
