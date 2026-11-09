//! The local recovery lane pages only current unfinished operations. Public
//! Task/Run/Delivery queries remain with their authoritative remote store.
use crate::local_journal::{LocalJournal, count_column, require};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult};
use ivy_store::{rusqlite::params, sqlite::error};

const CURSOR: &str = "pending-operations:";

struct PendingOperations {
    after: String,
    limit: i64,
}

fn cursor_error() -> IvyError {
    IvyError::new("invalid_cursor", "The local workflow cursor is invalid.")
}
fn eq(value: Option<&Json>, expected: &str) -> bool {
    value
        .and_then(Json::as_js_string)
        .is_some_and(|value| value.eq_str(expected))
}

impl PendingOperations {
    fn read(key: &str, options: &Json) -> IvyResult<Self> {
        let predicate = options.get("where");
        let phases = predicate
            .and_then(|value| value.get("value"))
            .and_then(Json::as_array);
        let order = options.get("orderBy").filter(|value| !value.is_null());
        require(
            key == "task-board/operation"
                && eq(predicate.and_then(|v| v.get("op")), "in")
                && eq(predicate.and_then(|v| v.get("field")), "data:/phase")
                && phases.is_some_and(|values| {
                    values.len() == 4
                        && values
                            .iter()
                            .zip(["accepted", "preparing", "publishing", "needs_attention"])
                            .all(|(value, phase)| eq(Some(value), phase))
                })
                && order.is_none_or(|order| {
                    order.as_array().is_some_and(|values| {
                        values.len() == 1
                            && eq(values[0].get("field"), "object.id")
                            && eq(values[0].get("direction"), "asc")
                    })
                }),
            "invalid_arguments",
            "The local journal only pages unfinished operations in identity order.",
        )?;
        let limit = match options.get("limit").filter(|value| !value.is_null()) {
            None => 50.0,
            Some(value) => value.as_f64().unwrap_or(f64::NAN),
        };
        require(
            limit.is_finite() && limit > 0.0 && limit.fract() == 0.0 && limit < i64::MAX as f64,
            "invalid_arguments",
            "The internal workflow page limit must be a positive integer.",
        )?;
        let after = match options.get("cursor").filter(|value| !value.is_null()) {
            None => String::new(),
            Some(value) => {
                let value = value.as_js_string().ok_or_else(cursor_error)?;
                if value.0.is_empty() {
                    String::new()
                } else {
                    let bytes = URL_SAFE_NO_PAD
                        .decode(value.to_utf8().map_err(|_| cursor_error())?)
                        .map_err(|_| cursor_error())?;
                    let text = std::str::from_utf8(&bytes).map_err(|_| cursor_error())?;
                    text.strip_prefix(CURSOR)
                        .filter(|id| !id.is_empty())
                        .ok_or_else(cursor_error)?
                        .to_owned()
                }
            }
        };
        Ok(Self {
            after,
            limit: limit as i64,
        })
    }
}

impl LocalJournal<'_> {
    /// Decode the owner's fixed internal query once, then read at most one page
    /// plus its continuation from the transactional current-operation index.
    /// No record body, historical revision, filter interpreter or offset scan.
    pub fn page(&self, key: &str, options: &Json) -> IvyResult<Json> {
        let query = PendingOperations::read(key, options)?;
        let mut statement = self
            .connection
            .prepare("SELECT id,revision FROM pending_operations WHERE id>? ORDER BY id LIMIT ?")
            .map_err(error)?;
        let mut rows = statement
            .query(params![query.after, query.limit + 1])
            .map_err(error)?;
        let mut items = Vec::new();
        let mut last = String::new();
        let mut more = false;
        while let Some(row) = rows.next().map_err(error)? {
            if items.len() as i64 == query.limit {
                more = true;
                break;
            }
            last = row.get::<_, String>(0).map_err(error)?;
            items.push(Json::object([
                ("objectId", last.clone().into()),
                (
                    "revision",
                    Json::Number(count_column(row, 1).map_err(error)? as f64),
                ),
                ("values", Json::Object(vec![])),
            ]));
        }
        Ok(Json::object([
            ("items", Json::Array(items)),
            (
                "nextCursor",
                if more {
                    URL_SAFE_NO_PAD.encode(format!("{CURSOR}{last}")).into()
                } else {
                    Json::Null
                },
            ),
        ]))
    }
}
