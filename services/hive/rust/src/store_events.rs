use super::{Principal, Store, require, sql_error, text};
use ivy_contracts::{
    IvyJson, IvyResult, JsString, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{OptionalExtension, params};

pub const EVENT_MAX_COUNT: i64 = 10_000;
const SECRETARY_EVENT_RESERVE: i64 = 1_024;

fn parent_scope(key: &JsString, parent: Option<&str>) -> JsString {
    let family = format!("objects/{}", text(key));
    let scope = match parent {
        None => format!("{family}/root"),
        Some(id)
            if !id.is_empty()
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)) =>
        {
            format!("{family}/parent/{id}")
        }
        Some(_) => return family.into(),
    };
    if scope.len() <= 512 {
        scope.into()
    } else {
        family.into()
    }
}

impl Store {
    pub fn durable_event_boundary(&self) -> IvyResult<Option<i64>> {
        self.db.query_row("SELECT MIN(s.acknowledged_sequence) FROM subscriptions s JOIN durable_subscriptions d ON d.node_id=s.node_id AND d.name=s.name",[],|r|r.get(0)).map_err(|e| self.query_error(e))
    }
    pub fn prune_event_journal_for_capacity(&self) -> IvyResult<()> {
        let count = self
            .db
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
            .map_err(sql_error)?;
        if count < EVENT_MAX_COUNT {
            return Ok(());
        }
        let candidate = self
            .db
            .query_row(
                "SELECT sequence FROM events ORDER BY sequence LIMIT 1 OFFSET ?",
                [count - (EVENT_MAX_COUNT - 1_024) - 1],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(sql_error)?
            .unwrap_or(0);
        let boundary = candidate.min(
            self.durable_event_boundary()?
                .unwrap_or(9_007_199_254_740_991),
        );
        if boundary <= self.metadata_integer("events_pruned_through")? {
            return Ok(());
        }
        self.db
            .execute("DELETE FROM events WHERE sequence<=?", [boundary])
            .map_err(sql_error)?;
        self.set_metadata("events_pruned_through", &boundary.to_string())?;
        self.db
            .execute(
                "UPDATE subscriptions SET batch_json=NULL WHERE acknowledged_sequence<?",
                [boundary],
            )
            .map_err(sql_error)?;
        Ok(())
    }
    pub fn admit_event(&self, secretary_reserve: bool) -> IvyResult<()> {
        self.prune_event_journal_for_capacity()?;
        let count = self
            .db
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
            .map_err(sql_error)?;
        require(
            count
                < EVENT_MAX_COUNT
                    + if secretary_reserve {
                        SECRETARY_EVENT_RESERVE
                    } else {
                        0
                    },
            "capacity_exceeded",
            "The durable Hive event journal is full; retry after Secretary acknowledges its backlog.",
        )
    }
    pub fn append_core_event(
        &mut self,
        principal: &Principal,
        object_id: &JsString,
        revision: u64,
        operation: &JsString,
        mutation_id: &JsString,
        now: &JsString,
        contract_key: Option<&JsString>,
    ) -> IvyResult<()> {
        let write = *operation == JsString::from("write");
        let metadata = if contract_key.is_none() || write {
            self.db
                .query_row(
                    "SELECT contract_key,parent_id FROM objects WHERE id=?",
                    [text(object_id)],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                )
                .optional()
                .map_err(sql_error)?
        } else {
            None
        };
        let key = contract_key
            .cloned()
            .or_else(|| metadata.as_ref().map(|(key, _)| key.as_str().into()));
        let scope = if write && key.as_ref().is_some_and(|k| !k.0.is_empty()) {
            match metadata.as_ref() {
                Some((_, parent)) => parent_scope(key.as_ref().unwrap(), parent.as_deref()),
                None => format!("objects/{}", text(key.as_ref().unwrap())).into(),
            }
        } else {
            "objects".into()
        };
        self.invalidate(scope);
        if key.as_ref().is_some_and(|key| {
            [
                "secretary/execution",
                "secretary/schedule-progress",
                "secretary/event-progress",
            ]
            .iter()
            .any(|value| *key == JsString::from(*value))
        }) {
            return Ok(());
        }
        let secretary=self.db.query_row("SELECT 1 FROM service_nodes WHERE principal_id=? AND service_name='secretary' LIMIT 1",[text(&principal.principal_id)],|r|r.get::<_,i64>(0)).optional().map_err(sql_error)?.is_some();
        self.admit_event(secretary)?;
        let version = if *operation == JsString::from("delete") {
            "1.1.0"
        } else {
            "1.0.0"
        };
        let mut source = JsString::from("principal:").0;
        source.extend_from_slice(&principal.principal_id.0);
        let payload = IvyJson::object([
            ("objectId", IvyJson::String(object_id.clone())),
            ("revision", IvyJson::Number(revision as f64)),
            ("operation", IvyJson::String(operation.clone())),
        ]);
        self.db.execute("INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES (?,?,?,?,?,?)",params!["hive.object.changed",version,text(&JsString(source)),text(now),text(mutation_id),canonical(&payload,MANAGEMENT_FRAME_BYTES)?]).map_err(sql_error)?;
        Ok(())
    }
}
