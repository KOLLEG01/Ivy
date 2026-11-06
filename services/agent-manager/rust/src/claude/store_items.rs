//! Persist item bodies once and append stream deltas without reading their history.
use super::*;
use ivy_store::rusqlite::{OptionalExtension, params};

#[derive(Clone, Copy)]
pub enum ItemDelta {
    Text = 0,
    Reasoning = 1,
    CommandOutput = 2,
}
#[derive(Clone, Copy)]
pub enum ItemsView {
    Full,
    Summary,
    NotLoaded,
}
#[derive(Clone, Copy)]
pub struct PageRequest {
    pub offset: u64,
    pub limit: u32,
    pub descending: bool,
}
pub struct Page {
    pub data: Vec<IvyJson>,
    pub has_more: bool,
}
/// Timestamp ties and NULL start times still have a stable, unique anchor.
#[derive(Clone)]
pub struct TurnCursor {
    started_at: Option<f64>,
    row: i64,
    inclusive: bool,
}
impl TurnCursor {
    pub fn decode(value: &str) -> Result<Self> {
        let mut parts = value.split(':');
        let inclusive = match parts.next() {
            Some("after") => false,
            Some("at") => true,
            _ => return Err(error("invalid turn cursor")),
        };
        let row = parts
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| error("invalid turn cursor"))?;
        let started_at = match parts.next() {
            Some("null") => None,
            Some(s) => Some(
                s.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| error("invalid turn cursor"))?,
            ),
            None => return Err(error("invalid turn cursor")),
        };
        if parts.next().is_some() {
            return Err(error("invalid turn cursor"));
        }
        Ok(Self {
            started_at,
            row,
            inclusive,
        })
    }
    pub fn encode(&self) -> String {
        format!(
            "{}:{}:{}",
            if self.inclusive { "at" } else { "after" },
            self.row,
            self.started_at
                .map(|n| n.to_string())
                .unwrap_or_else(|| "null".into())
        )
    }
}
pub struct TurnPage {
    pub data: Vec<IvyJson>,
    pub next: Option<TurnCursor>,
    pub backwards: Option<TurnCursor>,
}

struct ItemRecord {
    position: i64,
    body: String,
}
fn item_key(item: &IvyJson) -> Result<Option<String>> {
    item.get("id")
        .filter(|id| id.as_js_string().is_some())
        .map(encode)
        .transpose()
}

impl SessionStore {
    /// One transactional upgrade of the current store, one turn at a time.
    /// Invalid source data aborts the upgrade; it is never silently discarded.
    pub(super) fn migrate_items(&self) -> Result<()> {
        let old = self
            .all("PRAGMA table_info(turns)", &[])?
            .iter()
            .any(|row| is(field(row, "name"), "items_json"));
        if !old {
            return Ok(());
        }
        {
            let mut statement = self
                .db
                .prepare("SELECT id, items_json FROM turns ORDER BY rowid")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let source: String = row.get(1)?;
                let value = parse(&source, usize::MAX).map_err(json_error)?;
                let items = value
                    .as_array()
                    .ok_or_else(|| error("Stored turn items must be an array."))?;
                for (position, item) in items.iter().enumerate() {
                    self.write_item(&id, position as i64, item)?;
                }
            }
        }
        self.db
            .execute_batch("ALTER TABLE turns DROP COLUMN items_json")?;
        Ok(())
    }
    fn write_item(&self, turn: &str, position: i64, item: &IvyJson) -> Result<()> {
        self.db.execute(
            "INSERT INTO turn_items(turn_id, position, item_key, kind, body_json)
            VALUES (?, ?, ?, ?, ?) ON CONFLICT(turn_id, position) DO UPDATE SET
            item_key=excluded.item_key, kind=excluded.kind, body_json=excluded.body_json",
            params![
                turn,
                position,
                item_key(item)?,
                item.get("type").and_then(|v| v.as_str().ok()),
                encode(item)?
            ],
        )?;
        self.db.execute(
            "DELETE FROM item_deltas WHERE turn_id=? AND position=?",
            params![turn, position],
        )?;
        Ok(())
    }
    fn materialize_item(&self, turn: &str, record: ItemRecord) -> Result<IvyJson> {
        let mut item = parse(&record.body, usize::MAX).map_err(json_error)?;
        let mut statement = self.db.prepare(
            "SELECT target, delta_json FROM item_deltas
            WHERE turn_id=? AND position=? ORDER BY id",
        )?;
        let mut rows = statement.query(params![turn, record.position])?;
        while let Some(row) = rows.next()? {
            let target: i64 = row.get(0)?;
            let source: String = row.get(1)?;
            let delta = parse(&source, usize::MAX)
                .map_err(json_error)?
                .into_js_string()
                .ok_or_else(|| error("Stored item delta must be a string."))?;
            match target {
                0 => append_text(&mut item, "text", delta),
                1 => {
                    if let Some(IvyJson::Array(content)) = item.get_mut("content") {
                        if let Some(IvyJson::String(text)) = content.first_mut() {
                            text.0.extend(delta.0);
                            continue;
                        }
                    }
                    item.insert("content", IvyJson::Array(vec![IvyJson::String(delta)]));
                }
                2 => append_text(&mut item, "aggregatedOutput", delta),
                _ => return Err(error("Invalid stored item delta target.")),
            }
        }
        Ok(item)
    }
    pub fn get_item(&self, turn: &JsString, id: &JsString) -> Result<Option<IvyJson>> {
        let turn = turn.to_utf8().map_err(json_error)?;
        let record = self
            .db
            .query_row(
                "SELECT position, body_json FROM turn_items
            WHERE turn_id=? AND item_key=? ORDER BY position LIMIT 1",
                params![turn, encode(&IvyJson::String(id.clone()))?],
                |row| {
                    Ok(ItemRecord {
                        position: row.get(0)?,
                        body: row.get(1)?,
                    })
                },
            )
            .optional()?;
        record
            .map(|record| self.materialize_item(&turn, record))
            .transpose()
    }
    pub fn turn_in_progress(&self, turn: &JsString) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE id=? AND status='inProgress')",
            [turn.to_utf8().map_err(json_error)?],
            |row| row.get(0),
        )?)
    }
    pub(super) fn turn_items(&self, turn: &JsString, view: ItemsView) -> Result<Vec<IvyJson>> {
        if matches!(view, ItemsView::NotLoaded) {
            return Ok(Vec::new());
        }
        let turn = turn.to_utf8().map_err(json_error)?;
        let sql = if matches!(view, ItemsView::Summary) {
            "SELECT position, body_json FROM turn_items WHERE turn_id=? AND
             (position=(SELECT MIN(position) FROM turn_items WHERE turn_id=?1 AND kind='userMessage')
              OR position=(SELECT MAX(position) FROM turn_items WHERE turn_id=?1 AND kind='agentMessage'))
             ORDER BY position"
        } else {
            "SELECT position, body_json FROM turn_items WHERE turn_id=? ORDER BY position"
        };
        let mut statement = self.db.prepare(sql)?;
        let mut rows = statement.query([&turn])?;
        let mut items = Vec::new();
        while let Some(row) = rows.next()? {
            items.push(self.materialize_item(
                &turn,
                ItemRecord {
                    position: row.get(0)?,
                    body: row.get(1)?,
                },
            )?);
        }
        Ok(items)
    }
    pub fn append_item(&self, turn: &JsString, item: &IvyJson) -> Result<bool> {
        let turn = turn.to_utf8().map_err(json_error)?;
        self.transaction(|| {
            if !self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM turns WHERE id=?)",
                [&turn],
                |row| row.get::<_, bool>(0),
            )? {
                return Ok(false);
            }
            let position = self.db.query_row(
                "SELECT COALESCE(MAX(position), -1)+1 FROM turn_items WHERE turn_id=?",
                [&turn],
                |row| row.get(0),
            )?;
            self.write_item(&turn, position, item)?;
            Ok(true)
        })
    }
    pub fn append_item_delta(
        &self,
        turn: &JsString,
        id: &JsString,
        target: ItemDelta,
        delta: &JsString,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO item_deltas(turn_id, position, target, delta_json)
            SELECT turn_id, position, ?, ? FROM turn_items WHERE turn_id=? AND item_key=?
            AND (? <> 2 OR kind='commandExecution')",
            params![
                target as i64,
                encode(&IvyJson::String(delta.clone()))?,
                turn.to_utf8().map_err(json_error)?,
                encode(&IvyJson::String(id.clone()))?,
                target as i64
            ],
        )?;
        Ok(())
    }
    pub fn update_item(
        &self,
        turn: &JsString,
        id: &JsString,
        mut update: impl FnMut(IvyJson) -> Result<IvyJson>,
    ) -> Result<()> {
        let turn = turn.to_utf8().map_err(json_error)?;
        self.transaction(|| {
            let mut statement = self.db.prepare(
                "SELECT position, body_json FROM turn_items
                WHERE turn_id=? AND item_key=? ORDER BY position",
            )?;
            let mut rows = statement.query(params![turn, encode(&IvyJson::String(id.clone()))?])?;
            // Collect only matching identities; updating the index while iterating
            // it could otherwise revisit a row whose ID was changed by the updater.
            let mut records = Vec::new();
            while let Some(row) = rows.next()? {
                records.push(ItemRecord {
                    position: row.get(0)?,
                    body: row.get(1)?,
                });
            }
            for record in records {
                let position = record.position;
                let item = update(self.materialize_item(&turn, record)?)?;
                self.write_item(&turn, position, &item)?;
            }
            Ok(())
        })
    }
    pub fn update_item_and_move_to_end(
        &self,
        turn: &JsString,
        id: &JsString,
        update: impl FnOnce(IvyJson) -> Result<IvyJson>,
    ) -> Result<()> {
        let turn = turn.to_utf8().map_err(json_error)?;
        self.transaction(|| {
            let record = self
                .db
                .query_row(
                    "SELECT position, body_json FROM turn_items
                WHERE turn_id=? AND item_key=? ORDER BY position LIMIT 1",
                    params![turn, encode(&IvyJson::String(id.clone()))?],
                    |row| {
                        Ok(ItemRecord {
                            position: row.get(0)?,
                            body: row.get(1)?,
                        })
                    },
                )
                .optional()?;
            if let Some(record) = record {
                let old = record.position;
                let item = update(self.materialize_item(&turn, record)?)?;
                let position: i64 = self.db.query_row(
                    "SELECT MAX(position)+1 FROM turn_items WHERE turn_id=?",
                    [&turn],
                    |row| row.get(0),
                )?;
                self.db.execute(
                    "DELETE FROM turn_items WHERE turn_id=? AND position=?",
                    params![turn, old],
                )?;
                self.write_item(&turn, position, &item)?;
            }
            Ok(())
        })
    }
    pub fn items_page(
        &self,
        thread: &JsString,
        turn: Option<&JsString>,
        page: PageRequest,
    ) -> Result<Page> {
        if page.limit == 0 || page.limit > 100 || page.offset > i64::MAX as u64 {
            return Err(error("invalid item page"));
        }
        let thread = thread.to_utf8().map_err(json_error)?;
        let turn = turn.map(|s| s.to_utf8().map_err(json_error)).transpose()?;
        if let Some(turn) = &turn {
            if !self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM turns WHERE id=? AND thread_id=?)",
                params![turn, thread],
                |row| row.get::<_, bool>(0),
            )? {
                return Err(error(format!("turn does not belong to thread: {turn}")));
            }
        }
        let count: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM turn_items i JOIN turns t ON t.id=i.turn_id
            WHERE t.thread_id=? AND (? IS NULL OR t.id=?)",
            params![thread, turn, turn],
            |row| row.get(0),
        )?;
        if page.offset > count as u64 {
            return Err(error("invalid item cursor"));
        }
        let direction = if page.descending { "DESC" } else { "ASC" };
        let sql = format!("SELECT i.turn_id, i.position, i.body_json FROM turns t JOIN turn_items i ON i.turn_id=t.id
            WHERE t.thread_id=? AND (? IS NULL OR t.id=?)
            ORDER BY t.started_at {direction}, t.rowid {direction}, i.position {direction} LIMIT ? OFFSET ?");
        let mut statement = self.db.prepare(&sql)?;
        let mut rows = statement.query(params![
            thread,
            turn,
            turn,
            i64::from(page.limit) + 1,
            page.offset as i64
        ])?;
        let mut data = Vec::new();
        let mut has_more = false;
        while let Some(row) = rows.next()? {
            if data.len() == page.limit as usize {
                has_more = true;
                break;
            }
            let turn: String = row.get(0)?;
            let item = self.materialize_item(
                &turn,
                ItemRecord {
                    position: row.get(1)?,
                    body: row.get(2)?,
                },
            )?;
            data.push(IvyJson::object([("turnId", turn.into()), ("item", item)]));
        }
        Ok(Page { data, has_more })
    }
    pub fn turns_page(
        &self,
        thread: &JsString,
        anchor: Option<&TurnCursor>,
        limit: u32,
        descending: bool,
        view: ItemsView,
    ) -> Result<TurnPage> {
        if limit == 0 || limit > 100 {
            return Err(error("invalid turn limit"));
        }
        let direction = if descending { "DESC" } else { "ASC" };
        let comparison = match (descending, anchor.is_some_and(|c| c.inclusive)) {
            (true, true) => "<=",
            (true, false) => "<",
            (false, true) => ">=",
            (false, false) => ">",
        };
        let sql = format!("SELECT rowid AS turn_row, * FROM turns WHERE thread_id=?
            AND (? OR (started_at IS NOT NULL, COALESCE(started_at, 0), rowid) {comparison} (?, ?, ?))
            ORDER BY started_at {direction}, rowid {direction} LIMIT ?");
        let mut statement = self.db.prepare(&sql)?;
        let mut rows = statement.query(params![
            thread.to_utf8().map_err(json_error)?,
            anchor.is_none(),
            anchor.is_some_and(|c| c.started_at.is_some()),
            anchor.and_then(|c| c.started_at).unwrap_or(0.0),
            anchor.map_or(0, |c| c.row),
            i64::from(limit) + 1
        ])?;
        let names = [
            "id",
            "thread_id",
            "status",
            "started_at",
            "completed_at",
            "duration_ms",
            "diff",
            "error_json",
        ];
        let mut data = Vec::new();
        let mut has_more = false;
        let mut first = None;
        let mut last = None;
        while let Some(row) = rows.next()? {
            if data.len() == limit as usize {
                has_more = true;
                break;
            }
            let cursor = TurnCursor {
                row: row.get("turn_row")?,
                started_at: row.get("started_at")?,
                inclusive: false,
            };
            if first.is_none() {
                first = Some(TurnCursor {
                    inclusive: true,
                    ..cursor.clone()
                });
            }
            last = Some(cursor);
            let metadata = IvyJson::object(
                names
                    .into_iter()
                    .map(|name| Ok((name, cell(row.get_ref(name)?)?)))
                    .collect::<Result<Vec<_>>>()?,
            );
            let mut turn = row_to_turn(&metadata)?;
            turn.insert(
                "items",
                IvyJson::Array(self.turn_items(&stored_string(field(&metadata, "id")), view)?),
            );
            data.push(turn);
        }
        Ok(TurnPage {
            data,
            next: if has_more { last } else { None },
            backwards: first,
        })
    }
    pub(super) fn replace_turn_items(&self, turn: &JsString, items: &[IvyJson]) -> Result<()> {
        let turn = turn.to_utf8().map_err(json_error)?;
        self.db
            .execute("DELETE FROM turn_items WHERE turn_id=?", [&turn])?;
        for (position, item) in items.iter().enumerate() {
            self.write_item(&turn, position as i64, item)?;
        }
        Ok(())
    }
    pub(super) fn repair_turn_items(
        &self,
        turn: &JsString,
        message: &JsString,
        terminalize_activity: bool,
        strip_completed: bool,
    ) -> Result<bool> {
        let id = turn.to_utf8().map_err(json_error)?;
        let mut statement = self.db.prepare(
            "SELECT position, body_json FROM turn_items WHERE turn_id=? ORDER BY position",
        )?;
        let mut rows = statement.query([&id])?;
        let mut changed = false;
        while let Some(row) = rows.next()? {
            let position = row.get(0)?;
            let item = self.materialize_item(
                &id,
                ItemRecord {
                    position,
                    body: row.get(1)?,
                },
            )?;
            let next =
                terminalize_item(item.clone(), message, terminalize_activity, strip_completed);
            match next {
                Some(next) if next == item => (),
                Some(next) => {
                    self.write_item(&id, position, &next)?;
                    changed = true;
                }
                None => {
                    self.db.execute(
                        "DELETE FROM turn_items WHERE turn_id=? AND position=?",
                        params![id, position],
                    )?;
                    changed = true;
                }
            }
        }
        Ok(changed)
    }
}
fn append_text(item: &mut IvyJson, key: &str, delta: JsString) {
    if let Some(IvyJson::String(text)) = item.get_mut(key) {
        text.0.extend(delta.0);
    } else {
        item.insert(key, IvyJson::String(delta));
    }
}
