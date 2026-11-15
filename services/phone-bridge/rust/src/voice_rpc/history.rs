//! Bounded raw history reads on one original Voice connection. These are not
//! authenticated Hive observations; their authority stays with the Voice owner.
use super::*;
use ivy_contracts::{JsString, json::canonical_size};
use ivy_sdk::native_observations::ItemLayout;
use std::collections::HashSet;

const PAGE_ITEMS: usize = 16;
const PAGE_BYTES: usize = 1024 * 1024;
const READ_BYTES: usize = 8 * 1024 * 1024;
const READ_PAGES: usize = 512;

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_voice_history.rs"]
mod tests;

fn require(ok: bool) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(
            "phone_voice_history_invalid",
            "Native Voice history must retain bounded original identities and advancing pages.",
        ))
    }
}
fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .filter(|v| !v.0.is_empty())
        .ok_or_else(|| {
            IvyError::new(
                "phone_voice_history_invalid",
                "Native Voice history lacks its original identity.",
            )
        })
}
struct Budget {
    bytes: usize,
    pages: usize,
}
impl Budget {
    fn new() -> Self {
        Self { bytes: 0, pages: 0 }
    }
    fn charge(&mut self, value: &IvyJson) -> IvyResult<()> {
        let maximum = PAGE_BYTES.min(READ_BYTES.saturating_sub(self.bytes));
        let bytes = canonical_size(value, maximum).map_err(|_| Self::limit())?;
        if self.pages >= READ_PAGES {
            return Err(Self::limit());
        }
        self.bytes += bytes;
        self.pages += 1;
        Ok(())
    }
    fn limit() -> IvyError {
        IvyError::new(
            "phone_voice_history_limit",
            "The original Voice history remains unresolved within its read budget; it cannot be repeated.",
        )
    }
}
struct Cursor {
    current: IvyJson,
    seen: HashSet<JsString>,
}
impl Default for Cursor {
    fn default() -> Self {
        Self {
            current: IvyJson::Null,
            seen: HashSet::new(),
        }
    }
}
impl Cursor {
    fn advance(&mut self, result: &IvyJson, count: usize) -> IvyResult<bool> {
        require(count <= PAGE_ITEMS)?;
        match result.get("nextCursor") {
            Some(IvyJson::Null) => Ok(false),
            Some(IvyJson::String(next))
                if count > 0 && !next.0.is_empty() && next.0.len() <= 8192 =>
            {
                require(self.seen.insert(next.clone()))?;
                self.current = next.clone().into();
                Ok(true)
            }
            _ => {
                require(false)?;
                unreachable!()
            }
        }
    }
}
pub(super) enum TurnSearch<'a> {
    Id(&'a JsString),
    Operation(&'a JsString),
    // Descending native turns place any current work before the retained turn.
    ActiveOrId(Option<&'a JsString>),
}
impl VoiceRpc {
    fn history_schema(&self, method: &str) -> IvyResult<&IvyJson> {
        self.rpc
            .catalog()
            .required("clientRequests")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|d| d.get("method").is_some_and(|m| m.is_str(method)))
            .ok_or_else(IvyError::internal)?
            .required("outputSchema")
    }
    async fn history_page(
        &self,
        method: &str,
        params: IvyJson,
        budget: &mut Budget,
    ) -> IvyResult<IvyJson> {
        if budget.pages >= READ_PAGES || budget.bytes >= READ_BYTES {
            return Err(Budget::limit());
        }
        let value = self
            .call(
                method,
                params,
                RequestOptions {
                    detach_on_timeout: true,
                    ..Default::default()
                },
                Some(5000),
            )
            .await?;
        budget.charge(&value)?;
        self.schemas
            .validate(self.history_schema(method)?, &value, PAGE_BYTES)?;
        Ok(value)
    }
    fn history_items<'a>(
        &self,
        value: &'a IvyJson,
        turn: &JsString,
    ) -> IvyResult<Vec<&'a IvyJson>> {
        let layout = match self.item_layout.get() {
            Some(layout) => *layout,
            None => {
                let layout = ItemLayout::from_schema(self.history_schema("thread/items/list")?)?;
                let _ = self.item_layout.set(layout);
                layout
            }
        };
        layout.item_refs(value, turn)
    }
    async fn items(
        &self,
        thread: &JsString,
        turn: &JsString,
        operation: Option<&JsString>,
        budget: &mut Budget,
    ) -> IvyResult<Option<Vec<IvyJson>>> {
        let mut cursor = Cursor::default();
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        loop {
            let value = self
                .history_page(
                    "thread/items/list",
                    IvyJson::object([
                        ("threadId", thread.clone().into()),
                        ("turnId", turn.clone().into()),
                        ("cursor", cursor.current.clone()),
                        ("limit", (PAGE_ITEMS as f64).into()),
                        ("sortDirection", "asc".into()),
                    ]),
                    budget,
                )
                .await?;
            let page = self.history_items(&value, turn)?;
            let more = cursor.advance(&value, page.len())?;
            for item in page {
                require(seen.insert(text(item, "id")?.clone()))?;
                if let Some(operation) = operation {
                    if item.get("type").is_some_and(|v| v.is_str("userMessage"))
                        && item.get("clientId").and_then(IvyJson::as_js_string) == Some(operation)
                    {
                        return Ok(Some(Vec::new()));
                    }
                } else {
                    items.push(item.clone());
                }
            }
            if !more {
                return Ok(operation.is_none().then_some(items));
            }
        }
    }
    async fn find_turn(
        &self,
        thread: &JsString,
        search: TurnSearch<'_>,
        budget: &mut Budget,
    ) -> IvyResult<Option<IvyJson>> {
        let mut cursor = Cursor::default();
        let mut seen = HashSet::new();
        loop {
            let value = self
                .history_page(
                    "thread/turns/list",
                    IvyJson::object([
                        ("threadId", thread.clone().into()),
                        ("cursor", cursor.current.clone()),
                        ("limit", (PAGE_ITEMS as f64).into()),
                        ("itemsView", "notLoaded".into()),
                        ("sortDirection", "desc".into()),
                    ]),
                    budget,
                )
                .await?;
            let turns = value
                .required("data")?
                .as_array()
                .ok_or_else(IvyError::internal)?;
            let more = cursor.advance(&value, turns.len())?;
            for turn in turns {
                let id = text(turn, "id")?;
                require(
                    seen.insert(id.clone())
                        && turn.get("itemsView").is_some_and(|v| v.is_str("notLoaded"))
                        && turn
                            .get("items")
                            .and_then(IvyJson::as_array)
                            .is_some_and(|items| items.is_empty()),
                )?;
                let matched = match &search {
                    TurnSearch::Id(wanted) => id == *wanted,
                    TurnSearch::ActiveOrId(wanted) => {
                        wanted.is_some_and(|wanted| id == wanted)
                            || turn.get("status").is_some_and(|s| s.is_str("inProgress"))
                    }
                    TurnSearch::Operation(operation) => self
                        .items(thread, id, Some(operation), budget)
                        .await?
                        .is_some(),
                };
                if matched {
                    return Ok(Some(turn.clone()));
                }
            }
            if !more {
                return Ok(None);
            }
        }
    }
    pub(super) async fn work_turn(
        &self,
        thread: &JsString,
        search: TurnSearch<'_>,
    ) -> IvyResult<Option<IvyJson>> {
        self.find_turn(thread, search, &mut Budget::new()).await
    }
    pub(super) async fn preparation_turn(
        &self,
        thread: &JsString,
        turn: Option<&JsString>,
        operation: &JsString,
    ) -> IvyResult<Option<IvyJson>> {
        let mut budget = Budget::new();
        let search = turn
            .map(TurnSearch::Id)
            .unwrap_or(TurnSearch::Operation(operation));
        let Some(mut turn) = self.find_turn(thread, search, &mut budget).await? else {
            return Ok(None);
        };
        if turn.get("status").is_some_and(|s| s.is_str("completed")) {
            let items = self
                .items(thread, text(&turn, "id")?, None, &mut budget)
                .await?
                .unwrap();
            turn.insert("items", IvyJson::Array(items));
            turn.insert("itemsView", "full".into());
        }
        Ok(Some(turn))
    }
}
