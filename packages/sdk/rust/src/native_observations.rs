//! Interpret observation layouts from an immutable retained native catalog.
use crate::{IvyError, IvyJson, IvyResult, JsString};
use ivy_contracts::native_contract::NativeContract;
use std::{collections::HashSet, sync::OnceLock};

fn shape(message: &str) -> IvyError {
    IvyError::new("native_observation_shape_unsupported", message)
}
fn record(value: Option<&IvyJson>) -> IvyResult<&IvyJson> {
    value.filter(|v| v.as_object().is_some()).ok_or_else(|| {
        shape("Expected the selected native object shape.")
    })
}
fn resolve<'a>(root: &'a IvyJson, value: Option<&'a IvyJson>) -> IvyResult<&'a IvyJson> {
    let mut node = record(value)?;
    let mut seen = HashSet::new();
    while let Some(reference) = node.get("$ref") {
        let name = reference.as_js_string().and_then(|s| s.to_utf8().ok())
            .and_then(|s| s.strip_prefix("#/$defs/").map(str::to_owned))
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()));
        if seen.len() >= 64 || !seen.insert(node as *const IvyJson) || name.is_none() {
            return Err(shape("Native observation shape must resolve within its original finite schema."));
        }
        node = record(record(root.get("$defs"))?.get(&name.unwrap()))?;
    }
    Ok(node)
}
fn text_is(value: Option<&IvyJson>, text: &str) -> bool {
    value.and_then(IvyJson::as_js_string) == Some(&JsString::from(text))
}
fn properties<'a>(root: &'a IvyJson, value: Option<&'a IvyJson>) -> IvyResult<&'a IvyJson> {
    let node = resolve(root, value)?;
    if !text_is(node.get("type"), "object") {
        return Err(shape("Expected the selected native object schema."));
    }
    record(node.get("properties"))
}
fn required(node: &IvyJson, fields: &[&str]) -> bool {
    node.get("required").and_then(IvyJson::as_array).is_some_and(|values| {
        fields.iter().all(|name| values.iter().any(|v| text_is(Some(v), name)))
    })
}

pub struct NativeThreadState {
    pub thread: IvyJson,
    pub can_start_turn: bool,
    pub state: JsString,
}
#[derive(Clone, Copy)]
enum ItemLayout { Direct, TurnWrapper }

/// Keep one interpreter per retained contract; successful layout discovery is cached.
/// These observations do not replace native result validation at the caller boundary.
pub struct NativeObservations<'a> {
    contract: &'a NativeContract,
    direct_input_flag: OnceLock<bool>,
    items: OnceLock<ItemLayout>,
}
impl<'a> NativeObservations<'a> {
    pub fn new(contract: &'a NativeContract) -> Self {
        Self { contract, direct_input_flag: OnceLock::new(), items: OnceLock::new() }
    }
    pub fn native_thread_state(&self, result: &IvyJson) -> IvyResult<NativeThreadState> {
        let direct = if let Some(value) = self.direct_input_flag.get() { *value } else {
            let definition = self.contract.definition(&"thread/read".into())?;
            let schema = record(definition.get("outputSchema"))?;
            let thread = properties(schema, properties(schema, Some(schema))?.get("thread"))?;
            let flag = thread.get("canAcceptDirectInput").is_some();
            let _ = self.direct_input_flag.set(flag);
            flag
        };
        let thread = record(record(Some(result))?.get("thread"))?;
        let state = record(thread.get("status"))?.get("type")
            .and_then(IvyJson::as_js_string)
            .ok_or_else(|| shape("Native thread status requires its actual discriminator."))?;
        let can_start_turn = (state == &JsString::from("idle") || state == &JsString::from("systemError"))
            && (!direct || matches!(thread.get("canAcceptDirectInput"), Some(IvyJson::Bool(true))));
        Ok(NativeThreadState { thread: thread.clone(), can_start_turn, state: state.clone() })
    }
    fn item_layout(&self) -> IvyResult<ItemLayout> {
        if let Some(value) = self.items.get() { return Ok(*value); }
        let definition = self.contract.definition(&"thread/items/list".into())?;
        let schema = record(definition.get("outputSchema"))?;
        let data = resolve(schema, properties(schema, Some(schema))?.get("data"))?;
        if !text_is(data.get("type"), "array") {
            return Err(shape("Native item pages require the selected data array."));
        }
        let item = resolve(schema, data.get("items"))?;
        let selected = if text_is(item.get("type"), "object") {
            let fields = properties(schema, Some(item))?;
            if fields.get("item").is_none() || fields.get("turnId").is_none() || !required(item, &["item", "turnId"]) {
                return Err(shape("Unknown native item envelope."));
            }
            ItemLayout::TurnWrapper
        } else {
            let variants = item.get("oneOf").filter(|v| !v.is_null()).or_else(|| item.get("anyOf"));
            let variants = variants.and_then(IvyJson::as_array)
                .filter(|v| !v.is_empty()).ok_or_else(|| shape("Unknown native item union."))?;
            for variant in variants {
                let variant = resolve(schema, Some(variant))?;
                let fields = properties(schema, Some(variant))?;
                if fields.get("type").is_none() || fields.get("id").is_none() || !required(variant, &["type", "id"]) {
                    return Err(shape("Unknown native item union."));
                }
            }
            ItemLayout::Direct
        };
        let _ = self.items.set(selected);
        Ok(selected)
    }
    pub fn native_turn_items(&self, result: &IvyJson, turn_id: &JsString) -> IvyResult<Vec<IvyJson>> {
        if turn_id.0.is_empty() {
            return Err(IvyError::new("native_turn_mismatch", "Native item interpretation requires the original turn ID."));
        }
        let layout = self.item_layout()?;
        let data = record(Some(result))?.get("data").and_then(IvyJson::as_array)
            .ok_or_else(|| shape("Native page lacks its complete data array."))?;
        data.iter().map(|value| {
            let entry = record(Some(value))?;
            match layout {
                ItemLayout::Direct => Ok(entry.clone()),
                ItemLayout::TurnWrapper => {
                    if entry.get("turnId").and_then(IvyJson::as_js_string) != Some(turn_id) {
                        return Err(IvyError::new("native_turn_mismatch", "Native item belongs to another turn."));
                    }
                    Ok(record(entry.get("item"))?.clone())
                }
            }
        }).collect()
    }
}
