//! Shared history projections. Protocol evidence stays with its native owner.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult};
use std::{future::Future, pin::Pin, sync::Arc};

pub const MAX_HISTORY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_CACHED_TURNS: usize = 16;
pub type HistoryFuture<'a> = Pin<Box<dyn Future<Output = IvyResult<HistoryPage>> + Send + 'a>>;

pub struct HistoryPage {
    pub turns: Vec<Arc<Json>>,
    pub next_cursor: Option<Json>,
}
impl HistoryPage {
    pub fn from_json(page: &Json) -> IvyResult<Self> {
        let turns = page
            .get("data")
            .and_then(Json::as_array)
            .ok_or_else(|| IvyError::new("native_read_failed", "Native turn page missing."))?;
        Ok(Self {
            turns: turns
                .iter()
                .map(|turn| {
                    turn.as_object().ok_or_else(IvyError::internal)?;
                    Ok(Arc::new(match turn.get("items").and_then(Json::as_array) {
                        Some(items) => project_turn(turn, items.iter())?,
                        None => turn.clone(),
                    }))
                })
                .collect::<IvyResult<_>>()?,
            next_cursor: page.get("nextCursor").cloned(),
        })
    }
    // The command dispatcher consumes JSON at its existing protocol boundary.
    pub fn into_json(self) -> Json {
        let mut result = Json::object([(
            "data",
            Json::Array(self.turns.into_iter().map(Arc::unwrap_or_clone).collect()),
        )]);
        if let Some(cursor) = self.next_cursor {
            result.insert("nextCursor", cursor);
        }
        result
    }
}
pub fn turn_bytes(turn: &Json) -> usize {
    turn.retained_bytes()
        .saturating_add(2 * std::mem::size_of::<usize>() + std::mem::size_of::<Arc<Json>>())
}
pub fn project_turn<'a>(turn: &Json, items: impl Iterator<Item = &'a Json>) -> IvyResult<Json> {
    let mut projected = Json::object(
        turn.as_object()
            .ok_or_else(IvyError::internal)?
            .iter()
            .filter(|(key, _)| !key.eq_str("items"))
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    projected.insert(
        "items",
        Json::Array(
            items
                .map(|item| {
                    let kind = item
                        .get("type")
                        .and_then(Json::as_js_string)
                        .ok_or_else(|| {
                            IvyError::new(
                                "native_read_failed",
                                "Native history items require string types.",
                            )
                        })?;
                    if kind.eq_str("agentMessage") {
                        Ok(Json::object(
                            ["id", "type", "phase", "text"]
                                .into_iter()
                                .filter_map(|field| {
                                    item.get(field).map(|value| (field, value.clone()))
                                }),
                        ))
                    } else if kind.eq_str("userMessage") || kind.eq_str("steeringUserMessage") {
                        let mut user =
                            Json::object(["id", "type", "clientId"].into_iter().filter_map(
                                |field| item.get(field).map(|value| (field, value.clone())),
                            ));
                        // History delivery needs the delegation identity and the
                        // original client receipt, not user text or media data.
                        let source = super::listener::delegation_source(item)?;
                        user.insert(
                            "content",
                            Json::Array(
                                source
                                    .into_iter()
                                    .map(|source| {
                                        Json::object([(
                                            "codexDelegation",
                                            Json::object([(
                                                "sourceThreadId",
                                                Json::String(source),
                                            )]),
                                        )])
                                    })
                                    .collect(),
                            ),
                        );
                        Ok(user)
                    } else {
                        Ok(Json::object(["id", "type"].into_iter().filter_map(
                            |field| item.get(field).map(|value| (field, value.clone())),
                        )))
                    }
                })
                .collect::<IvyResult<_>>()?,
        ),
    );
    Ok(projected)
}
