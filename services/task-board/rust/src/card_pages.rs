//! Bounded card pages over authoritative immutable Task pins. Bulk current
//! headers avoid per-card RPCs; changed or oversized headers keep exact reads.
use super::{
    document_reads::{DocumentReads, TreeReads},
    store::ObjectReads,
};
use crate::{
    engine_read::comment_reads::{self, CommentReadPolicies},
    journal_store::Contracts,
    operations::{ObjectPin, StoredRecord},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use std::{collections::HashSet, future::Future};

fn mismatch() -> IvyError {
    IvyError::new(
        "task_board_evidence_mismatch",
        "The card page does not retain its selected immutable Task pins.",
    )
}

impl<R, C, Parse, SqlFailure, M> DocumentReads<'_, R, C, Parse, SqlFailure, M> {
    fn query_task_header<E: From<IvyError>>(&self, mut row: Json) -> Result<Json, E>
    where
        C: Contracts,
    {
        let pin = Json::object([
            ("objectId", row.required("objectId")?.clone()),
            ("revision", row.required("revision")?.clone()),
        ]);
        let mut document = row.remove("document").ok_or_else(mismatch)?;
        self.check_remote_record::<E>("task-board/task", &pin, true, true, &document)?;
        if document.required("object")?.get("id") != pin.get("objectId")
            || document.required("revision")?.get("objectId") != pin.get("objectId")
            || document.required("revision")?.get("contractVersion") != row.get("contractVersion")
        {
            return Err(mismatch().into());
        }
        document.required("content")?.required("value")?;
        let value = document
            .get_mut("content")
            .unwrap()
            .remove("value")
            .unwrap();
        Ok(StoredRecord {
            key: "task-board/task".into(),
            pin: ObjectPin::from_json(pin)?,
            value,
            metadata: document.remove("object").unwrap(),
            revision: document.remove("revision").unwrap(),
        }
        .into_json())
    }

    pub(crate) async fn card_page<E, P, F, Fut>(
        &self,
        mut query: Json,
        policies: &P,
        check_cancelled: &impl Fn() -> IvyResult<()>,
        mut page: F,
    ) -> Result<Json, E>
    where
        R: ObjectReads<E>,
        C: Contracts,
        M: TreeReads<E>,
        E: Clone + From<IvyError>,
        P: CommentReadPolicies<E>,
        Parse: Fn(&str) -> Result<Json, E>,
        SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
        F: FnMut(Json) -> Fut,
        Fut: Future<Output = IvyResult<Json>>,
    {
        self.local
            .contracts
            .validate_definition("TaskListQuery", &query)?;
        check_cancelled()?;
        query.remove("view");
        let limit = query.get("limit").and_then(Json::as_f64).unwrap_or(20.0);
        if !(1.0..=20.0).contains(&limit) || limit.fract() != 0.0 {
            return Err(
                IvyError::new("invalid_arguments", "Card pages contain at most 20 tasks.").into(),
            );
        }
        query.insert("limit", Json::Number(limit));
        query.insert(
            "select",
            Json::Array(vec!["object.effectivelyArchived".into()]),
        );
        let mut selected = page(query).await?;
        let next = selected.required("nextCursor")?.clone();
        let rows = match selected.remove("items").and_then(Json::into_array) {
            Some(rows) if rows.len() <= limit as usize => rows,
            _ => return Err(mismatch().into()),
        };
        let mut ids = Vec::with_capacity(rows.len());
        let mut pins = Vec::with_capacity(rows.len());
        for row in &rows {
            let id = row
                .required("objectId")?
                .as_js_string()
                .ok_or_else(mismatch)?
                .clone();
            if ids.contains(&id) {
                return Err(mismatch().into());
            }
            pins.push(Json::object([
                ("objectId", Json::String(id.clone())),
                ("revision", row.required("revision")?.clone()),
            ]));
            ids.push(id);
        }
        let mut cards: Vec<Option<Json>> = vec![None; rows.len()];
        if !rows.is_empty() {
            let mut options = Json::object([
                (
                    "where",
                    Json::object([
                        ("op", "in".into()),
                        ("field", "object.id".into()),
                        (
                            "value",
                            Json::Array(ids.iter().cloned().map(Json::String).collect()),
                        ),
                    ]),
                ),
                (
                    "select",
                    Json::Array(vec!["object.effectivelyArchived".into()]),
                ),
                ("includeArchived", true.into()),
                ("includeContent", true.into()),
                ("observe", true.into()),
                ("limit", Json::Number(rows.len() as f64)),
            ]);
            let mut cursors = HashSet::new();
            let mut seen = vec![false; rows.len()];
            // Each byte-capped bulk page must select at least one new ID.
            for _ in 0..rows.len() {
                check_cancelled()?;
                let mut result = match page(options.clone()).await {
                    Ok(result) => result,
                    Err(error) if error.code_js().eq_str("result_too_large") => break,
                    Err(error) => return Err(error.into()),
                };
                check_cancelled()?;
                let cursor = result.required("nextCursor")?.clone();
                let batch = match result.remove("items").and_then(Json::into_array) {
                    Some(items) if !items.is_empty() && items.len() <= rows.len() => items,
                    Some(items) if items.is_empty() && cursor.is_null() => break,
                    _ => return Err(mismatch().into()),
                };
                for row in batch {
                    check_cancelled()?;
                    let id = row
                        .required("objectId")?
                        .as_js_string()
                        .ok_or_else(mismatch)?;
                    let index = ids
                        .iter()
                        .position(|expected| expected == id)
                        .ok_or_else(mismatch)?;
                    if seen[index] {
                        return Err(mismatch().into());
                    }
                    seen[index] = true;
                    let header = self.query_task_header::<E>(row)?;
                    if header.get("pin") == Some(&pins[index]) {
                        if header.required("revision")?.get("contractVersion")
                            != rows[index].get("contractVersion")
                        {
                            return Err(mismatch().into());
                        }
                        let card_query = Json::object([
                            ("task", Json::String(ids[index].clone())),
                            ("view", "card".into()),
                        ]);
                        cards[index] = Some(
                            comment_reads::read_task(self, policies, &header, &card_query).await?,
                        );
                    }
                }
                if cursor.is_null() || seen.iter().all(|selected| *selected) {
                    break;
                }
                let value = cursor.as_js_string().ok_or_else(mismatch)?.clone();
                if !cursors.insert(value) {
                    return Err(mismatch().into());
                }
                options.insert("cursor", cursor);
            }
        }
        let mut items = Vec::with_capacity(rows.len());
        for (index, card) in cards.into_iter().enumerate() {
            check_cancelled()?;
            let card = match card {
                Some(card) => card,
                None => {
                    let header = self.observed_task_header(&pins[index]).await?;
                    if header.required("revision")?.get("contractVersion")
                        != rows[index].get("contractVersion")
                    {
                        return Err(mismatch().into());
                    }
                    let card_query = Json::object([
                        ("task", Json::String(ids[index].clone())),
                        ("view", "card".into()),
                    ]);
                    comment_reads::read_task(self, policies, &header, &card_query).await?
                }
            };
            items.push(card);
        }
        Ok(Json::object([
            ("view", "cards".into()),
            ("items", Json::Array(items)),
            ("nextCursor", next),
        ]))
    }
}

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-card-pages.rs"]
mod tests;
