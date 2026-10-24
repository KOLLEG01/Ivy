//! Advisory native attention only. Saved input identities and notifications do
//! not confer answer authority; exact requests remain owned by AgentManager.
use crate::{
    native_inspector::{InspectionStore, NativeInspector},
    operations::mutation,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, agent_schema::validate_agent, hash::hash_json,
};
use std::cell::RefCell;

pub struct NativeSignals<'a, S> {
    pub inspector: NativeInspector<'a, S>,
    // Preserve insertion order: refreshing a key does not move it to the end.
    cursors: RefCell<Vec<(JsString, JsString, f64)>>,
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn number(value: &Json, key: &str) -> IvyResult<f64> {
    value.required(key)?.as_f64().ok_or_else(IvyError::internal)
}
fn equal(value: &Json, key: &str, want: &str) -> bool {
    value.get(key).and_then(Json::as_js_string) == Some(&want.into())
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn activity_key(item: &Json) -> IvyResult<String> {
    hash_json(&Json::object([
        ("epoch", item.required("epoch")?.clone()),
        ("sequence", item.required("sequence")?.clone()),
    ]))
}
fn set_activity(values: &mut Vec<(String, Json)>, key: String, value: Json) {
    if let Some(entry) = values.iter_mut().find(|entry| entry.0 == key) {
        entry.1 = value;
    } else {
        values.push((key, value));
    }
}

impl<'a, S: InspectionStore> NativeSignals<'a, S> {
    pub fn new(inspector: NativeInspector<'a, S>) -> Self {
        Self {
            inspector,
            cursors: RefCell::default(),
        }
    }
    fn advance(&self, run: &JsString, epoch: &JsString, sequence: f64) {
        let mut cursors = self.cursors.borrow_mut();
        if let Some(entry) = cursors.iter_mut().find(|entry| &entry.0 == run) {
            entry.1 = epoch.clone();
            entry.2 = sequence;
            return;
        }
        if cursors.len() >= 1024 {
            cursors.remove(0);
        }
        cursors.push((run.clone(), epoch.clone(), sequence));
    }
    fn forget(&self, run: &JsString) {
        self.cursors.borrow_mut().retain(|entry| &entry.0 != run);
    }

    pub async fn step(&self, run_id: &JsString) -> IvyResult<Option<Json>> {
        let store = self.inspector.store;
        let run = store
            .read_document("task-board/run", &Json::String(run_id.clone()), true)
            .await?;
        if ["completed", "failed", "cancelled"]
            .iter()
            .any(|phase| equal(&run.value, "phase", phase))
            || !run
                .value
                .get("turnId")
                .and_then(Json::as_js_string)
                .is_some_and(|turn| !turn.0.is_empty())
        {
            self.forget(run_id);
            return Ok(None);
        }
        let context = self.inspector.context(&run.pin).await?;
        let saved = store
            .named_document("task-board/native-signals", "Native signals", run_id)
            .await?;
        require(
            saved.as_ref().is_none_or(|saved| {
                saved.parent_id.as_ref() == Some(run_id)
                    && field(field(&saved.value, "run"), "objectId").as_js_string() == Some(run_id)
                    && field(field(&saved.value, "run"), "revision")
                        .as_f64()
                        .is_some_and(|revision| {
                            run.pin
                                .get("revision")
                                .and_then(Json::as_f64)
                                .is_some_and(|current| revision <= current)
                        })
                    && field(&saved.value, "serviceNodeId").as_js_string()
                        == Some(&context.service_node_id)
                    && field(&saved.value, "nativeVersion").as_js_string()
                        == Some(&context.native_version)
                    && field(&saved.value, "threadId").as_js_string() == Some(&context.thread_id)
                    && field(&saved.value, "turnId").as_js_string() == Some(&context.turn_id)
            }),
            "task_board_identity_conflict",
            "A native signals checkpoint must retain its original Run, owner and native context.",
        )?;
        let epoch_changed = saved.as_ref().is_some_and(|saved| {
            field(&saved.value, "epoch").as_js_string() != Some(&context.epoch)
        });
        let retained = if epoch_changed {
            0.0
        } else {
            saved
                .as_ref()
                .map(|saved| number(&saved.value, "notificationCursor"))
                .transpose()?
                .unwrap_or(0.0)
        };
        let volatile = self
            .cursors
            .borrow()
            .iter()
            .find(|entry| &entry.0 == run_id && entry.1 == context.epoch)
            .map(|entry| entry.2)
            .unwrap_or(0.0);
        let after = retained.max(volatile);
        let pending = self
            .inspector
            .management(
                &context.service_node_id,
                "inputs",
                Json::object([("limit", Json::Number(128.0))]),
            )
            .await?;
        validate_agent("PendingInputPage", &pending)?;
        let page = self
            .inspector
            .management(
                &context.service_node_id,
                "notifications",
                Json::object([
                    ("afterSequence", Json::Number(after)),
                    ("limit", Json::Number(100.0)),
                ]),
            )
            .await?;
        validate_agent("NotificationPage", &page)?;
        require(
            field(&pending, "epoch").as_js_string() == Some(&context.epoch)
                && field(&page, "epoch").as_js_string() == Some(&context.epoch),
            "task_board_native_epoch_changed",
            "Native signals changed connection epoch while being read.",
        )?;
        let mut inputs = Vec::new();
        for input in pending
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let identity = input.required("identity")?;
            require(
                field(identity, "serviceNodeId").as_js_string() == Some(&context.service_node_id),
                "task_board_target_mismatch",
                "A pending native request must identify its queried owner.",
            )?;
            if field(&run.value, "nativeEpoch").as_js_string() != Some(&context.epoch)
                || field(identity, "epoch").as_js_string() != Some(&context.epoch)
                || field(input, "threadId").as_js_string() != Some(&context.thread_id)
                || field(input, "turnId") != &Json::Null
                    && field(input, "turnId").as_js_string() != Some(&context.turn_id)
                || !equal(input, "state", "pending") && !equal(input, "state", "answering")
            {
                continue;
            }
            let mut selected = Json::Object(vec![]);
            for key in ["identity", "method", "turnId", "state", "observedAt"] {
                selected.insert(key, input.required(key)?.clone());
            }
            inputs.push((hash_json(identity)?, selected));
        }
        // Hash identities use a fixed prefix and lowercase ASCII hex.
        inputs.sort_by(|a, b| a.0.cmp(&b.0));
        require(
            inputs.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "task_board_native_input_conflict",
            "Current native input identities must be unique.",
        )?;
        let mut activity = Vec::new();
        if let Some(saved) = &saved {
            for item in saved
                .value
                .required("activity")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                set_activity(&mut activity, activity_key(item)?, item.clone());
            }
        }
        let mut sequence = after;
        let items = page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        for item in items {
            let observed = number(item, "sequence")?;
            require(
                field(item, "serviceNodeId").as_js_string() == Some(&context.service_node_id)
                    && field(item, "nativeVersion").as_js_string() == Some(&context.native_version)
                    && observed > sequence,
                "task_board_native_notification_conflict",
                "Native notification pages must retain the selected owner/version and increasing original sequence.",
            )?;
            sequence = observed;
            let params = field(item, "params");
            let turn = match params.get("turnId") {
                Some(value @ Json::String(_)) => Some(value),
                _ => field(params, "turn").get("id"),
            };
            if field(item, "epoch").as_js_string() != Some(&context.epoch)
                || field(params, "threadId").as_js_string() != Some(&context.thread_id)
                || turn.is_some_and(|turn| {
                    turn != &Json::Null && turn.as_js_string() != Some(&context.turn_id)
                })
            {
                continue;
            }
            let value = Json::object([
                ("epoch", item.required("epoch")?.clone()),
                ("sequence", item.required("sequence")?.clone()),
                ("method", item.required("method")?.clone()),
                ("observedAt", item.required("observedAt")?.clone()),
            ]);
            set_activity(&mut activity, activity_key(&value)?, value);
        }
        let through = number(&page, "throughSequence")?;
        let has_more = field(&page, "hasMore").as_bool() == Some(true);
        let gap = field(&page, "gap").as_bool() == Some(true);
        require(
            through >= sequence || gap && items.is_empty() && !has_more,
            "task_board_cursor_regression",
            "Only an explicit empty journal gap can lower an observed notification cursor.",
        )?;
        require(
            !has_more || through > after,
            "task_board_cursor_regression",
            "A pending notification page must advance its cursor.",
        )?;
        require(
            self.inspector.context(&run.pin).await?.epoch == context.epoch,
            "task_board_native_epoch_changed",
            "The selected owner changed before saving native signals.",
        )?;
        let material = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("run", run.pin.clone()),
            (
                "serviceNodeId",
                Json::String(context.service_node_id.clone()),
            ),
            (
                "nativeVersion",
                Json::String(context.native_version.clone()),
            ),
            ("epoch", Json::String(context.epoch.clone())),
            ("threadId", Json::String(context.thread_id.clone())),
            ("turnId", Json::String(context.turn_id.clone())),
            ("inputsTruncated", pending.required("truncated")?.clone()),
            (
                "inputs",
                Json::Array(inputs.into_iter().map(|(_, input)| input).collect()),
            ),
            ("notificationCursor", Json::Number(through)),
            (
                "notificationGap",
                (saved.as_ref().is_some_and(|saved| {
                    field(&saved.value, "notificationGap").as_bool() == Some(true)
                }) || epoch_changed
                    || gap)
                    .into(),
            ),
            ("notificationsPending", has_more.into()),
            (
                "activity",
                Json::Array(
                    activity
                        .into_iter()
                        .rev()
                        .take(100)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .map(|(_, value)| value)
                        .collect(),
                ),
            ),
        ]);
        if let Some(saved) = &saved {
            let mut previous = saved.value.clone();
            if let Json::Object(values) = &mut previous {
                values.retain(|(key, _)| key != &JsString::from("observedAt"));
            }
            let mut comparable = material.clone();
            for key in ["run", "notificationCursor", "notificationsPending"] {
                comparable.insert(key, saved.value.required(key)?.clone());
            }
            if hash_json(&previous)? == hash_json(&comparable)? {
                self.advance(run_id, &context.epoch, through);
                return Ok(Some(saved.pin.clone()));
            }
        }
        store.verify_owner().await?;
        let mut value = material.clone();
        value.insert("observedAt", Json::String(store.now()));
        let step = format!(
            "signals:{}",
            hash_json(&Json::object([
                (
                    "previous",
                    saved
                        .as_ref()
                        .map(|saved| saved.pin.clone())
                        .unwrap_or(Json::Null)
                ),
                ("material", material)
            ]))?
        );
        let destination = if let Some(saved) = &saved {
            Json::object([
                ("objectId", saved.pin.required("objectId")?.clone()),
                ("expectedRevision", saved.pin.required("revision")?.clone()),
            ])
        } else {
            Json::object([(
                "create",
                Json::object([
                    ("parentId", Json::String(run_id.clone())),
                    ("name", "Native signals".into()),
                ]),
            )])
        };
        match store
            .write_document(
                "task-board/native-signals",
                &value,
                &mutation(run_id, &step)?,
                &destination,
            )
            .await
        {
            Ok(pin) => {
                self.advance(run_id, &context.epoch, through);
                Ok(Some(pin))
            }
            Err(failure)
                if ["revision_conflict", "mutation_conflict"].contains(&failure.code.as_str()) =>
            {
                self.forget(run_id);
                match store
                    .named_document("task-board/native-signals", "Native signals", run_id)
                    .await?
                {
                    Some(winner) => Ok(Some(winner.pin)),
                    None => Err(failure),
                }
            }
            Err(failure) => Err(failure),
        }
    }
}
