//! Retain original native item pages without synthesizing a full-history reply.
use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    native_observations::{ItemLayout, NativeObservations},
    rpc::RequestFuture,
};
use ivy_contracts::{
    checked_native_contract::CheckedNativeContracts, json::canonical_size,
    native_contract::NativeContract,
};
use std::{collections::HashSet, future::Future, pin::Pin, sync::Arc};

pub type ContractFuture<'a> =
    Pin<Box<dyn Future<Output = IvyResult<Arc<NativeContract>>> + Send + 'a>>;

/// Reads and checks must validate the original caller, selected owner, catalog,
/// request hash and epoch. Domain authority remains with the caller.
pub trait NativeTurnOwner: Send + Sync {
    fn check_read(
        &self,
        value: &IvyJson,
        method: &str,
        params: &IvyJson,
        epoch: &JsString,
    ) -> IvyResult<()>;
    fn read<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        epoch: &'a JsString,
        allow_error: bool,
    ) -> RequestFuture<'a>;
    fn selected_contract(&self) -> ContractFuture<'_>;
}

/// Own the exact admitted observations once. Immutable views borrow their
/// original items; callers cannot mutate evidence after complete verification.
#[derive(Debug)]
pub struct VerifiedNativeTurnContents {
    original: IvyJson,
    item_layout: Option<ItemLayout>,
}
impl VerifiedNativeTurnContents {
    pub fn original(&self) -> &IvyJson {
        &self.original
    }
    pub fn into_original(self) -> IvyJson {
        self.original
    }
    pub fn snapshot(&self) -> &IvyJson {
        native_turn_snapshot(&self.original)
    }
    pub fn turn(&self) -> &IvyJson {
        &field(field(self.snapshot(), "reply"), "result")
            .get("data")
            .and_then(IvyJson::as_array)
            .expect("verified native turn")[0]
    }
    pub fn items(&self) -> impl Iterator<Item = &IvyJson> {
        let direct = if self.item_layout.is_none() {
            field(self.turn(), "items")
                .as_array()
                .expect("verified full items")
        } else {
            &[]
        };
        let pages = if self.item_layout.is_some() {
            field(&self.original, "pages")
                .as_array()
                .expect("verified native pages")
        } else {
            &[]
        };
        let layout = self.item_layout.unwrap_or(ItemLayout::Direct);
        direct.iter().chain(pages.iter().flat_map(move |page| {
            field(field(page, "reply"), "result")
                .get("data")
                .and_then(IvyJson::as_array)
                .expect("verified native page items")
                .iter()
                .map(move |entry| layout.original(entry))
        }))
    }
    pub fn final_answer(&self) -> Option<&JsString> {
        single_final_answer(self.items())
    }
}
/// Preserve the domain rule: exactly one final agent message with string text.
pub fn single_final_answer<'a>(
    items: impl IntoIterator<Item = &'a IvyJson>,
) -> Option<&'a JsString> {
    let mut answers = items
        .into_iter()
        .filter(|item| text(item, "type", "agentMessage") && text(item, "phase", "final_answer"))
        .filter_map(|item| item.get("text").and_then(IvyJson::as_js_string));
    let answer = answers.next()?;
    answers.next().is_none().then_some(answer)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    static NULL: IvyJson = IvyJson::Null;
    value.get(key).unwrap_or(&NULL)
}
fn text(value: &IvyJson, key: &str, expected: &str) -> bool {
    value.get(key).is_some_and(|value| value.is_str(expected))
}
fn empty_record() -> IvyJson {
    IvyJson::object(Vec::<(String, IvyJson)>::new())
}
fn record(value: &IvyJson) -> IvyJson {
    if value.as_object().is_some() {
        value.clone()
    } else {
        empty_record()
    }
}
pub fn native_turn_snapshot(value: &IvyJson) -> &IvyJson {
    if value.get("format").is_some() {
        field(value, "snapshot")
    } else {
        value
    }
}
fn item_params(thread: &JsString, turn: &JsString, cursor: &IvyJson) -> IvyJson {
    IvyJson::object([
        ("threadId", IvyJson::String(thread.clone())),
        ("turnId", IvyJson::String(turn.clone())),
        ("cursor", cursor.clone()),
        ("limit", IvyJson::Number(16.0)),
        ("sortDirection", "asc".into()),
    ])
}
fn turn_of<'a>(
    read: &'a IvyJson,
    thread: &JsString,
    turn: &JsString,
    view: &str,
) -> IvyResult<&'a IvyJson> {
    let params = field(read, "params");
    let reply = field(read, "reply");
    require(
        text(read, "method", "thread/turns/list")
            && reply.get("result").is_some()
            && params.get("threadId").and_then(IvyJson::as_js_string) == Some(thread)
            && text(params, "itemsView", view)
            && field(params, "limit").as_f64() == Some(1.0)
            && text(params, "sortDirection", "desc"),
        "native_turn_mismatch",
        "Turn contents require the exact one-turn query.",
    )?;
    let data = field(field(reply, "result"), "data").as_array();
    require(
        data.is_some_and(|v| v.len() == 1),
        "native_turn_mismatch",
        "Turn contents require one original turn.",
    )?;
    let value = &data.unwrap()[0];
    require(
        value.get("id").and_then(IvyJson::as_js_string) == Some(turn)
            && ["inProgress", "completed", "failed", "interrupted"]
                .iter()
                .any(|status| text(value, "status", status))
            && text(value, "itemsView", view)
            && field(value, "items")
                .as_array()
                .is_some_and(|v| view != "notLoaded" || v.is_empty()),
        "native_turn_mismatch",
        "Turn contents must retain their original turn identity and representation.",
    )?;
    Ok(value)
}
fn item_identity(item: &IvyJson, seen: &mut HashSet<JsString>, message: &str) -> IvyResult<()> {
    let id = field(item, "id").as_js_string();
    require(
        id.is_some_and(|id| !id.0.is_empty() && seen.insert(id.clone())),
        "native_turn_incomplete",
        message,
    )
}
fn epoch(snapshot: &IvyJson) -> IvyResult<&JsString> {
    field(snapshot, "epoch").as_js_string().ok_or_else(|| {
        IvyError::new(
            "native_turn_mismatch",
            "Turn contents require their original epoch.",
        )
    })
}

pub async fn verify_native_turn_contents(
    owner: &dyn NativeTurnOwner,
    retained: &CheckedNativeContracts,
    value: IvyJson,
    thread_id: &JsString,
    turn_id: &JsString,
) -> IvyResult<VerifiedNativeTurnContents> {
    let snapshot = native_turn_snapshot(&value);
    let paged = value.get("format").is_some();
    require(
        !paged
            || text(&value, "format", "native-turn-item-pages-1")
                && field(&value, "pages").as_array().is_some(),
        "native_turn_mismatch",
        "Unknown native contents representation.",
    )?;
    let epoch = epoch(snapshot)?;
    owner.check_read(
        snapshot,
        "thread/turns/list",
        field(snapshot, "params"),
        epoch,
    )?;
    let turn = turn_of(
        snapshot,
        thread_id,
        turn_id,
        if paged { "notLoaded" } else { "full" },
    )?;
    let mut seen = HashSet::new();
    if !paged {
        let items = field(turn, "items").as_array().unwrap();
        for item in items {
            item_identity(
                item,
                &mut seen,
                "Complete turn items need distinct original identities.",
            )?;
        }
        return Ok(VerifiedNativeTurnContents {
            original: value,
            item_layout: None,
        });
    }
    let identity = IvyJson::object([
        ("catalogHash", field(snapshot, "catalogHash").clone()),
        (
            "nativeExecutableHash",
            field(snapshot, "nativeExecutableHash").clone(),
        ),
    ]);
    let version = field(snapshot, "nativeVersion")
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    let contract = match retained.checked_native_contract(version, Some(&identity)) {
        Ok(contract) => contract,
        Err(error)
            if ["native_catalog_mismatch", "native_version_unsupported"]
                .iter()
                .any(|c| error.code == *c) =>
        {
            owner.selected_contract().await?
        }
        Err(error) => return Err(error),
    };
    let interpreter = NativeObservations::new(&contract);
    let pages = field(&value, "pages").as_array().unwrap();
    require(
        !pages.is_empty(),
        "native_turn_incomplete",
        "A paginated turn needs its original item observations.",
    )?;
    let mut cursors = HashSet::new();
    let mut observations = HashSet::from([field(snapshot, "observationId")
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)?]);
    let mut cursor = IvyJson::Null;
    for (index, page) in pages.iter().enumerate() {
        owner.check_read(
            page,
            "thread/items/list",
            &item_params(thread_id, turn_id, &cursor),
            epoch,
        )?;
        let observation = field(page, "observationId")
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        require(
            observations.insert(observation.clone()),
            "native_turn_incomplete",
            "Original item observations must have distinct identities.",
        )?;
        let result = field(page, "reply").get("result");
        require(
            result.is_some(),
            "native_turn_incomplete",
            "Complete turn contents cannot omit a failed item page.",
        )?;
        let result = result.unwrap();
        let page_items = interpreter.native_turn_item_refs(result, turn_id)?;
        let next = result.get("nextCursor");
        let final_cursor = next == Some(&IvyJson::Null);
        let advancing = next
            .and_then(IvyJson::as_js_string)
            .is_some_and(|s| !s.0.is_empty() && next != Some(&cursor) && !cursors.contains(s));
        require(
            page_items.len() <= 16
                && (!page_items.is_empty() || final_cursor)
                && (final_cursor || advancing)
                && (if index == pages.len() - 1 {
                    final_cursor
                } else {
                    !final_cursor
                }),
            "native_turn_incomplete",
            "Original item pages must advance to their final cursor.",
        )?;
        for item in page_items {
            item_identity(
                item,
                &mut seen,
                "Turn items need distinct original identities.",
            )?;
        }
        if let Some(previous) = cursor.as_js_string() {
            cursors.insert(previous.clone());
        }
        cursor = next.unwrap().clone();
    }
    Ok(VerifiedNativeTurnContents {
        original: value,
        item_layout: Some(interpreter.item_layout()?),
    })
}

/// Only an exact unsupported first item page permits the original full-turn fallback.
pub async fn read_native_turn_contents(
    owner: &dyn NativeTurnOwner,
    retained: &CheckedNativeContracts,
    snapshot: IvyJson,
    thread_id: &JsString,
    turn_id: &JsString,
    maximum_bytes: Option<usize>,
) -> IvyResult<VerifiedNativeTurnContents> {
    let maximum = maximum_bytes.unwrap_or(64 * 1024 * 1024);
    let epoch = epoch(&snapshot)?;
    owner.check_read(
        &snapshot,
        "thread/turns/list",
        field(&snapshot, "params"),
        epoch,
    )?;
    let turn = turn_of(&snapshot, thread_id, turn_id, "notLoaded")?;
    let mut pages = Vec::new();
    let mut visited = HashSet::new();
    let mut cursor = IvyJson::Null;
    let mut bytes = canonical_size(&snapshot, maximum)?;
    loop {
        let observed = owner
            .read(
                "thread/items/list",
                item_params(thread_id, turn_id, &cursor),
                epoch,
                true,
            )
            .await?;
        let reply = field(&observed, "reply");
        if let Some(error) = reply.get("error") {
            require(
                pages.is_empty()
                    && cursor.is_null()
                    && field(error, "code").as_f64() == Some(-32601.0),
                "native_read_failed",
                "Only an unsupported first item page permits the full-turn fallback.",
            )?;
            let mut params = record(field(&snapshot, "params"));
            params.insert("itemsView", "full".into());
            let full = owner
                .read("thread/turns/list", params, epoch, false)
                .await?;
            let status = field(turn_of(&full, thread_id, turn_id, "full")?, "status");
            require(
                status == field(turn, "status"),
                "native_turn_changed",
                "The native turn changed while reading its contents.",
            )?;
            canonical_size(&full, maximum)?;
            return verify_native_turn_contents(owner, retained, full, thread_id, turn_id).await;
        }
        let size = canonical_size(&observed, maximum)?;
        require(
            size <= maximum.saturating_sub(bytes),
            "native_turn_contents_limit",
            "Complete native contents exceed their original byte budget.",
        )?;
        bytes += size;
        let next = field(reply, "result").get("nextCursor");
        require(
            next == Some(&IvyJson::Null)
                || next.and_then(IvyJson::as_js_string).is_some_and(|s| {
                    !s.0.is_empty() && next != Some(&cursor) && !visited.contains(s)
                }),
            "native_turn_incomplete",
            "Native item cursors must advance.",
        )?;
        cursor = next.unwrap().clone();
        pages.push(observed);
        if cursor.is_null() {
            break;
        }
        visited.insert(cursor.as_js_string().unwrap().clone());
    }
    let value = IvyJson::object([
        ("format", "native-turn-item-pages-1".into()),
        ("snapshot", snapshot),
        ("pages", IvyJson::Array(pages)),
    ]);
    verify_native_turn_contents(owner, retained, value, thread_id, turn_id).await
}
