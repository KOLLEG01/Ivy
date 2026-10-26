use ivy_contracts::{
    IvyJson, IvyResult, JsString,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::sync::LazyLock;

static TEMPLATE: LazyLock<Vec<JsString>> = LazyLock::new(|| {
    parse(
        include_str!("../../../../packages/ui-client/src/dashboard/document-template.json"),
        MANAGEMENT_FRAME_BYTES,
    )
    .expect("embedded document template is JSON")
    .as_array()
    .expect("document template is an array")
    .iter()
    .map(|part| {
        part.as_js_string()
            .expect("document template parts are strings")
            .clone()
    })
    .collect()
});

/// Compose UTF-16 code units without normalizing authored HTML. Only the channel
/// is JSON quoted and escaped, exactly like the browser viewer's wrapper.
pub fn dashboard_document(html: &JsString, channel: &JsString) -> IvyResult<JsString> {
    let token = stringify(&IvyJson::String(channel.clone()), MANAGEMENT_FRAME_BYTES)?
        .replace('<', "\\u003c");
    let mut units = Vec::with_capacity(
        TEMPLATE.iter().map(|p| p.0.len()).sum::<usize>() + html.0.len() + token.len(),
    );
    units.extend_from_slice(&TEMPLATE[0].0);
    units.extend(token.encode_utf16());
    units.extend_from_slice(&TEMPLATE[1].0);
    units.extend_from_slice(&html.0);
    units.extend_from_slice(&TEMPLATE[2].0);
    Ok(JsString(units))
}
