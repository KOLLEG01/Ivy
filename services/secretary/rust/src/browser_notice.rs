//! Browser publication follows the caller's contact policy and retains a local receipt.
use crate::store::{Record, Store};
use ivy_contracts::{IvyJson, IvyResult, JsString, json::encode_uri_component};
use ivy_sdk::{browser_notice::publish_browser_notice, clock::Clock};

pub async fn secretary_browser_notice(
    store: &Store<'_>,
    service_node: &JsString,
    tag: &JsString,
    text: &JsString,
    clock: &dyn Clock,
) -> IvyResult<()> {
    let key = "secretary/browser-notice";
    let name = String::from_utf16_lossy(&tag.0);
    if store.journal.named(key, &name, true)?.is_some() {
        return Ok(());
    }
    let configuration = store
        .named(Record::Configuration, &"configuration".into())
        .await?;
    if configuration
        .as_ref()
        .and_then(|d| d.get("value"))
        .and_then(|v| v.get("browserNotifications"))
        == Some(&IvyJson::Bool(false))
    {
        return Ok(());
    }
    // URLSearchParams uses USVString and form encoding, unlike encodeURIComponent.
    let node = encode_uri_component(&String::from_utf16_lossy(&service_node.0).into())?
        .replace("%20", "+")
        .replace('!', "%21")
        .replace('~', "%7E")
        .replace('\'', "%27")
        .replace('(', "%28")
        .replace(')', "%29");
    publish_browser_notice(
        store.client.clone(),
        &"secretary".into(),
        &IvyJson::object([
            ("title", "Ivy · Secretary".into()),
            ("body", IvyJson::String(text.clone())),
            ("tag", IvyJson::String(tag.clone())),
            (
                "target",
                IvyJson::object([
                    ("uiId", "secretary-ui".into()),
                    ("fragment", format!("#/journal?node={node}").into()),
                ]),
            ),
        ]),
    )
    .await?;
    store.journal.create_technical(
        key,
        &name,
        &IvyJson::object([(
            "expiresAt",
            IvyJson::Number(clock.utc_millis() as f64 + 8.0 * 86_400_000.0),
        )]),
    )?;
    Ok(())
}
