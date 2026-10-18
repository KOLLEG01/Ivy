//! Bounded connection-local operations and inputs. SQLite owns durable receipts.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::{NativeFrameKind, validate_agent, validate_agent_frame},
    hash::hash_json,
    json::{canonical, object_entries, stringify},
    limits::{
        MANAGEMENT_FRAME_BYTES, NATIVE_ANSWER_FRAME_BYTES, NATIVE_FRAME_BYTES,
        NATIVE_REQUEST_FRAME_BYTES,
    },
    wire_schema::validate_shared,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

const OUTCOME_RESERVATION: usize = NATIVE_FRAME_BYTES + 65536;
type TerminalCallback = Box<dyn FnMut(&mut IvyJson) -> IvyResult<()> + Send>;
pub type JournalClock = dyn Fn() -> JsString + Send + Sync;
#[derive(Clone)]
pub struct Owner {
    pub service_node_id: JsString,
    pub native_version: JsString,
    pub native_executable_hash: JsString,
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub max_operations: usize,
    pub max_journal_bytes: usize,
    pub max_pending_inputs: usize,
}
#[derive(Clone)]
pub struct Identity {
    pub caller_principal_id: JsString,
    pub operation_id: JsString,
}
impl Identity {
    pub fn from_operation(value: &IvyJson) -> IvyResult<Self> {
        Ok(Self {
            caller_principal_id: text(value, "callerPrincipalId")?.clone(),
            operation_id: text(value, "operationId")?.clone(),
        })
    }
    pub fn value(&self) -> IvyJson {
        IvyJson::object([
            (
                "callerPrincipalId",
                IvyJson::String(self.caller_principal_id.clone()),
            ),
            ("operationId", IvyJson::String(self.operation_id.clone())),
        ])
    }
}
struct Row {
    value: IvyJson,
    charge: usize,
}
// Retain Map insertion order without linear lookup or another dependency.
struct Rows<V> {
    indices: HashMap<String, u64>,
    rows: BTreeMap<u64, (String, V)>,
    next: u64,
}
impl<V> Rows<V> {
    fn new() -> Self {
        Self {
            indices: HashMap::new(),
            rows: BTreeMap::new(),
            next: 0,
        }
    }
    fn len(&self) -> usize {
        self.indices.len()
    }
    fn get(&self, key: &str) -> Option<&V> {
        self.indices
            .get(key)
            .and_then(|index| self.rows.get(index))
            .map(|(_, value)| value)
    }
    fn insert(&mut self, key: String, value: V) {
        let index = if let Some(index) = self.indices.get(&key) {
            *index
        } else {
            let index = self.next;
            self.next += 1;
            self.indices.insert(key.clone(), index);
            index
        };
        self.rows.insert(index, (key, value));
    }
    fn remove(&mut self, key: &str) {
        if let Some(index) = self.indices.remove(key) {
            self.rows.remove(&index);
        }
    }
    fn values(&self) -> impl DoubleEndedIterator<Item = &V> {
        self.rows.values().map(|(_, value)| value)
    }
    fn entries(&self) -> impl Iterator<Item = (&String, &V)> {
        self.rows.values().map(|(key, value)| (key, value))
    }
    fn clear(&mut self) {
        self.indices.clear();
        self.rows.clear();
        self.next = 0;
    }
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    value.get(name).ok_or_else(IvyError::internal)
}
fn text<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a JsString> {
    field(value, name)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn is(value: &IvyJson, name: &str, expected: &str) -> bool {
    value.get(name) == Some(&IvyJson::from(expected))
}
fn terminal(value: &IvyJson) -> bool {
    ["succeeded", "failed", "outcome_unknown"]
        .iter()
        .any(|phase| is(value, "phase", phase))
}
fn pending(value: &IvyJson) -> bool {
    is(value, "state", "pending") || is(value, "state", "answering")
}
fn set(value: &mut IvyJson, name: &str, replacement: IvyJson) -> IvyResult<()> {
    set_key(value, name.into(), replacement)
}
fn set_key(value: &mut IvyJson, key: JsString, replacement: IvyJson) -> IvyResult<()> {
    let IvyJson::Object(entries) = value else {
        return Err(IvyError::internal());
    };
    if let Some((_, value)) = entries.iter_mut().find(|(k, _)| k == &key) {
        *value = replacement;
    } else {
        entries.push((key, replacement));
    }
    Ok(())
}
fn spread(mut base: IvyJson, value: &IvyJson) -> IvyResult<IvyJson> {
    match value {
        IvyJson::Object(_) => {
            for (key, value) in object_entries(value) {
                set_key(&mut base, key.clone(), value.clone())?;
            }
        }
        IvyJson::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                set(&mut base, &index.to_string(), value.clone())?;
            }
        }
        IvyJson::String(value) => {
            for (index, unit) in value.0.iter().enumerate() {
                set(
                    &mut base,
                    &index.to_string(),
                    IvyJson::String(JsString(vec![*unit])),
                )?;
            }
        }
        _ => {}
    }
    Ok(base)
}
pub struct NativeJournal {
    owner: Owner,
    limits: Limits,
    operations: Rows<Row>,
    inputs: Rows<Row>,
    observations: Rows<IvyJson>,
    revisions: HashMap<String, f64>,
    epoch: Option<JsString>,
    durable: HashSet<String>,
    clock: Arc<JournalClock>,
    on_terminal: Option<TerminalCallback>,
}
impl NativeJournal {
    pub fn new(owner: Owner, limits: Limits) -> IvyResult<Self> {
        Self::with_clock(
            owner,
            limits,
            Arc::new(|| {
                chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into()
            }),
        )
    }
    pub fn with_clock(owner: Owner, limits: Limits, clock: Arc<JournalClock>) -> IvyResult<Self> {
        validate_shared(
            "Identifier",
            &IvyJson::String(owner.service_node_id.clone()),
        )?;
        validate_shared(
            "Hash",
            &IvyJson::String(owner.native_executable_hash.clone()),
        )?;
        Ok(Self {
            owner,
            limits,
            operations: Rows::new(),
            inputs: Rows::new(),
            observations: Rows::new(),
            revisions: HashMap::new(),
            epoch: None,
            durable: HashSet::new(),
            clock,
            on_terminal: None,
        })
    }
    pub fn set_on_terminal(&mut self, callback: Option<TerminalCallback>) {
        self.on_terminal = callback;
    }
    pub fn close(&mut self) {
        self.operations.clear();
        self.inputs.clear();
        self.observations.clear();
        self.revisions.clear();
        self.epoch = None;
        self.durable.clear();
        self.on_terminal = None;
    }
    pub fn epoch(&self) -> Option<&JsString> {
        self.epoch.as_ref()
    }
    pub fn owner(&self) -> &Owner {
        &self.owner
    }
    pub fn inventory_revision(&mut self, kind: &str, minimum: f64) -> f64 {
        let previous = *self.revisions.get(kind).unwrap_or(&0.0);
        let next = if previous.is_nan() || minimum.is_nan() {
            f64::NAN
        } else {
            previous.max(minimum) + 1.0
        };
        self.revisions.insert(kind.into(), next);
        next
    }
    pub fn status(&self) -> IvyJson {
        IvyJson::object([
            ("retained", IvyJson::Number(self.operations.len() as f64)),
            (
                "maximum",
                IvyJson::Number(self.limits.max_operations as f64),
            ),
            ("bytes", IvyJson::Number(self.bytes() as f64)),
            (
                "maximumBytes",
                IvyJson::Number(self.limits.max_journal_bytes as f64),
            ),
        ])
    }
    fn bytes(&self) -> usize {
        self.operations
            .values()
            .chain(self.inputs.values())
            .map(|row| row.charge)
            .sum()
    }
    fn key(identity: &Identity) -> IvyResult<String> {
        validate_shared(
            "Identifier",
            &IvyJson::String(identity.caller_principal_id.clone()),
        )?;
        validate_shared(
            "Identifier",
            &IvyJson::String(identity.operation_id.clone()),
        )?;
        canonical(&identity.value(), MANAGEMENT_FRAME_BYTES)
    }
    pub fn get(&self, identity: &Identity) -> IvyResult<Option<IvyJson>> {
        Ok(self
            .operations
            .get(&Self::key(identity)?)
            .map(|row| row.value.clone()))
    }
    pub fn forget(&mut self, identity: &Identity) -> IvyResult<()> {
        self.operations.remove(&Self::key(identity)?);
        Ok(())
    }
    pub fn retained_in_hive(&mut self, identity: &Identity) -> IvyResult<()> {
        self.durable.insert(Self::key(identity)?);
        Ok(())
    }
    fn existing(&self, identity: &Identity) -> IvyResult<IvyJson> {
        self.get(identity)?
            .ok_or_else(|| IvyError::new("not_found", "Operation is not in this connection."))
    }
    fn compact(&mut self) {
        let keys = self
            .operations
            .entries()
            .filter(|(key, row)| {
                terminal(&row.value)
                    && (self.durable.contains(*key)
                        || is(&row.value, "callerPrincipalId", "ivy-native-clock"))
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.operations.remove(&key);
            self.durable.remove(&key);
        }
        let keys = self
            .inputs
            .entries()
            .filter(|(_, row)| !pending(&row.value))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.inputs.remove(&key);
        }
    }
    fn save(&mut self, mut value: IvyJson) -> IvyResult<IvyJson> {
        let key = Self::key(&Identity::from_operation(&value)?)?;
        let size = stringify(&value, usize::MAX)?.len();
        let row = self.operations.get(&key);
        require(
            row.is_some_and(|row| size <= row.charge),
            "native_journal_capacity",
            "Outcome exceeds reserved memory.",
        )?;
        let completed = terminal(&value);
        let charge = if completed { size } else { row.unwrap().charge };
        self.operations.insert(
            key,
            Row {
                value: value.clone(),
                charge,
            },
        );
        if completed {
            let method = text(&value, "method")?;
            let key = canonical(&IvyJson::String(method.clone()), MANAGEMENT_FRAME_BYTES)?;
            let mut observed = self.observations.get(&key).cloned().unwrap_or_else(|| {
                IvyJson::object([
                    ("method", IvyJson::String(method.clone())),
                    ("lastSucceededAt", IvyJson::Null),
                    ("lastFailedAt", IvyJson::Null),
                    ("lastCode", IvyJson::Null),
                ])
            });
            if is(&value, "phase", "succeeded") {
                set(
                    &mut observed,
                    "lastSucceededAt",
                    field(&value, "updatedAt")?.clone(),
                )?;
            } else {
                set(
                    &mut observed,
                    "lastFailedAt",
                    field(&value, "updatedAt")?.clone(),
                )?;
                set(&mut observed, "lastCode", field(&value, "code")?.clone())?;
            }
            self.observations.insert(key, observed);
            if self.observations.len() > 256 {
                let key = self.observations.entries().next().unwrap().0.clone();
                self.observations.remove(&key);
            }
            if let Some(callback) = self.on_terminal.as_mut() {
                callback(&mut value)?;
            }
        }
        Ok(value)
    }
    pub fn accept(
        &mut self,
        identity: &Identity,
        method: &JsString,
        params: &IvyJson,
    ) -> IvyResult<(IvyJson, bool)> {
        self.admit(identity, method, params, false)
    }
    pub fn prevent(
        &mut self,
        identity: &Identity,
        method: &JsString,
        params: &IvyJson,
    ) -> IvyResult<IvyJson> {
        self.admit(identity, method, params, true)
            .map(|(value, _)| value)
    }
    fn admit(
        &mut self,
        identity: &Identity,
        method: &JsString,
        params: &IvyJson,
        prevent: bool,
    ) -> IvyResult<(IvyJson, bool)> {
        if method == &JsString::from("agent.answer") {
            require(
                params.as_object().is_some(),
                "invalid_arguments",
                "Answer requires its exact identity.",
            )?;
            let answer = spread(
                IvyJson::object([(
                    "operationId",
                    IvyJson::String(identity.operation_id.clone()),
                )]),
                params,
            )?;
            validate_agent("AnswerInput", &answer)?;
            let request_id = field(field(&answer, "identity")?, "requestId")?.clone();
            stringify(
                &spread(
                    IvyJson::object([("id", request_id)]),
                    field(&answer, "reply")?,
                )?,
                NATIVE_ANSWER_FRAME_BYTES,
            )?;
        } else {
            stringify(
                &IvyJson::object([
                    ("id", "00000000-0000-0000-0000-000000000000".into()),
                    ("method", IvyJson::String(method.clone())),
                    ("params", params.clone()),
                ]),
                NATIVE_REQUEST_FRAME_BYTES,
            )?;
        }
        let request_hash = hash_json(&IvyJson::object([
            ("method", IvyJson::String(method.clone())),
            ("params", params.clone()),
        ]))?;
        if let Some(prior) = self.get(identity)? {
            require(
                prior.get("requestHash") == Some(&IvyJson::from(request_hash.as_str())),
                "mutation_conflict",
                "Operation identity has different arguments.",
            )?;
            return Ok((prior, false));
        }
        self.compact();
        let now = IvyJson::String((self.clock)());
        let value = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            (
                "callerPrincipalId",
                IvyJson::String(identity.caller_principal_id.clone()),
            ),
            (
                "operationId",
                IvyJson::String(identity.operation_id.clone()),
            ),
            (
                "serviceNodeId",
                IvyJson::String(self.owner.service_node_id.clone()),
            ),
            (
                "nativeVersion",
                IvyJson::String(self.owner.native_version.clone()),
            ),
            (
                "nativeExecutableHash",
                IvyJson::String(self.owner.native_executable_hash.clone()),
            ),
            ("method", IvyJson::String(method.clone())),
            ("params", params.clone()),
            ("requestHash", request_hash.into()),
            (
                "phase",
                if prevent {
                    "failed".into()
                } else {
                    "accepted".into()
                },
            ),
            ("createdAt", now.clone()),
            ("updatedAt", now),
            ("epoch", IvyJson::Null),
            ("requestId", IvyJson::Null),
            ("reply", IvyJson::Null),
            (
                "code",
                if prevent {
                    "request_prevented".into()
                } else {
                    IvyJson::Null
                },
            ),
        ]);
        let charge =
            stringify(&value, usize::MAX)?.len() + if prevent { 0 } else { OUTCOME_RESERVATION };
        let cleanup = prevent
            || ["thread/unsubscribe", "turn/interrupt", "agent.answer"]
                .iter()
                .any(|name| method == &JsString::from(*name));
        require(
            self.operations.len() + if cleanup { 0 } else { 4 } < self.limits.max_operations
                && self.bytes()
                    + charge
                    + if cleanup {
                        0
                    } else {
                        OUTCOME_RESERVATION + 65536
                    }
                    <= self.limits.max_journal_bytes,
            "native_journal_capacity",
            "Connection memory is full; no native request sent.",
        )?;
        self.operations.insert(
            Self::key(identity)?,
            Row {
                value: value.clone(),
                charge,
            },
        );
        Ok((value, true))
    }
    pub fn begin_epoch(&mut self, epoch: &JsString) -> IvyResult<()> {
        validate_agent(
            "InputIdentity",
            &IvyJson::object([
                (
                    "serviceNodeId",
                    IvyJson::String(self.owner.service_node_id.clone()),
                ),
                ("epoch", IvyJson::String(epoch.clone())),
                ("requestId", IvyJson::Number(0.0)),
            ]),
        )?;
        require(
            self.epoch.as_ref() != Some(epoch),
            "mutation_conflict",
            "A new native connection must have a fresh epoch.",
        )?;
        self.lose_unfinished(&"native_epoch_lost".into())?;
        self.retire_inputs(&"native_epoch_lost".into())?;
        self.epoch = Some(epoch.clone());
        Ok(())
    }
    fn lose_unfinished(&mut self, code: &JsString) -> IvyResult<()> {
        let values = self
            .operations
            .values()
            .filter(|row| {
                is(&row.value, "phase", "accepted") || is(&row.value, "phase", "dispatched")
            })
            .map(|row| row.value.clone())
            .collect::<Vec<_>>();
        for mut value in values {
            let accepted = is(&value, "phase", "accepted");
            set(
                &mut value,
                "phase",
                if accepted {
                    "failed".into()
                } else {
                    "outcome_unknown".into()
                },
            )?;
            set(
                &mut value,
                "code",
                if accepted {
                    "request_not_dispatched".into()
                } else {
                    IvyJson::String(code.clone())
                },
            )?;
            set(&mut value, "updatedAt", IvyJson::String((self.clock)()))?;
            self.save(value)?;
        }
        Ok(())
    }
    pub fn lose_epoch(&mut self, epoch: &JsString, code: &JsString) -> IvyResult<()> {
        if self.epoch.as_ref() != Some(epoch) {
            return Ok(());
        }
        self.lose_unfinished(code)?;
        self.retire_inputs(code)?;
        self.epoch = None;
        Ok(())
    }
    pub fn dispatch(
        &mut self,
        identity: &Identity,
        epoch: &JsString,
        request_id: &IvyJson,
    ) -> IvyResult<IvyJson> {
        let mut value = self.existing(identity)?;
        require(
            is(&value, "phase", "accepted")
                && self.epoch.as_ref() == Some(epoch)
                && !epoch.0.is_empty(),
            "native_state_conflict",
            "Native dispatch requires a new accepted operation and the current epoch.",
        )?;
        set(&mut value, "phase", "dispatched".into())?;
        set(&mut value, "epoch", IvyJson::String(epoch.clone()))?;
        set(&mut value, "requestId", request_id.clone())?;
        set(&mut value, "updatedAt", IvyJson::String((self.clock)()))?;
        self.save(value)
    }
    pub fn finish(
        &mut self,
        identity: &Identity,
        epoch: &JsString,
        request_id: &IvyJson,
        reply: &IvyJson,
    ) -> IvyResult<IvyJson> {
        stringify(
            &spread(IvyJson::object([("id", request_id.clone())]), reply)?,
            NATIVE_FRAME_BYTES,
        )?;
        validate_agent_frame(reply, NativeFrameKind::Reply)?;
        let mut value = self.existing(identity)?;
        require(
            is(&value, "phase", "dispatched")
                && self.epoch.as_ref() == Some(epoch)
                && value.get("epoch") == Some(&IvyJson::String(epoch.clone()))
                && value.get("requestId") == Some(request_id),
            "native_state_conflict",
            "Only the exact dispatched request in the current epoch can record a native result.",
        )?;
        let succeeded = reply.get("result").is_some();
        set(
            &mut value,
            "phase",
            if succeeded {
                "succeeded".into()
            } else {
                "failed".into()
            },
        )?;
        set(&mut value, "reply", reply.clone())?;
        set(
            &mut value,
            "code",
            if succeeded {
                IvyJson::Null
            } else {
                "native_error".into()
            },
        )?;
        set(&mut value, "updatedAt", IvyJson::String((self.clock)()))?;
        self.save(value)
    }
    pub fn reject_before_dispatch(
        &mut self,
        identity: &Identity,
        code: &JsString,
    ) -> IvyResult<IvyJson> {
        let mut value = self.existing(identity)?;
        require(
            is(&value, "phase", "accepted"),
            "native_state_conflict",
            "A dispatched native effect cannot be rejected as unexecuted.",
        )?;
        set(&mut value, "phase", "failed".into())?;
        set(&mut value, "code", IvyJson::String(code.clone()))?;
        set(&mut value, "updatedAt", IvyJson::String((self.clock)()))?;
        self.save(value)
    }
    fn input_key(&self, identity: &IvyJson) -> IvyResult<String> {
        validate_agent("InputIdentity", identity)?;
        require(
            identity.get("serviceNodeId")
                == Some(&IvyJson::String(self.owner.service_node_id.clone())),
            "target_conflict",
            "Native input belongs to another Service Node.",
        )?;
        hash_json(identity)
    }
    pub fn input(&self, identity: &IvyJson) -> IvyResult<Option<IvyJson>> {
        Ok(self
            .inputs
            .get(&self.input_key(identity)?)
            .map(|row| row.value.clone()))
    }
    pub fn pending_input_count(&self) -> usize {
        self.inputs
            .values()
            .filter(|row| pending(&row.value))
            .count()
    }
    pub fn observe_input(
        &mut self,
        identity: &IvyJson,
        method: &JsString,
        params: &IvyJson,
    ) -> IvyResult<IvyJson> {
        let mut frame = Vec::new();
        if let Some(id) = identity.get("requestId") {
            frame.push(("id", id.clone()));
        }
        frame.push(("method", IvyJson::String(method.clone())));
        frame.push(("params", params.clone()));
        stringify(&IvyJson::object(frame), NATIVE_FRAME_BYTES)?;
        self.compact();
        require(
            identity.get("epoch")
                == self
                    .epoch
                    .as_ref()
                    .map(|v| IvyJson::String(v.clone()))
                    .as_ref(),
            "native_state_conflict",
            "Input must come from the current native connection.",
        )?;
        if let Some(prior) = self.input(identity)? {
            require(
                prior.get("method") == Some(&IvyJson::String(method.clone()))
                    && canonical(field(&prior, "params")?, MANAGEMENT_FRAME_BYTES)?
                        == canonical(params, MANAGEMENT_FRAME_BYTES)?
                    && is(&prior, "state", "pending"),
                "native_input_conflict",
                "Native request ID was reused or its original payload changed.",
            )?;
            return Ok(prior);
        }
        require(
            self.inputs.len() < self.limits.max_operations
                && self.pending_input_count() < self.limits.max_pending_inputs,
            "native_input_capacity",
            "Native pending input/history capacity is full.",
        )?;
        let thread = params
            .get("threadId")
            .filter(|v| !v.is_null())
            .or_else(|| params.get("conversationId"));
        let turn = params.get("turnId");
        let now = IvyJson::String((self.clock)());
        let value = IvyJson::object([
            ("identity", identity.clone()),
            ("method", IvyJson::String(method.clone())),
            ("params", params.clone()),
            ("observedAt", now.clone()),
            ("updatedAt", now),
            (
                "threadId",
                thread
                    .and_then(IvyJson::as_js_string)
                    .cloned()
                    .map(IvyJson::String)
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "turnId",
                turn.and_then(IvyJson::as_js_string)
                    .cloned()
                    .map(IvyJson::String)
                    .unwrap_or(IvyJson::Null),
            ),
            ("state", "pending".into()),
            ("answerOperationId", IvyJson::Null),
            ("answerCallerPrincipalId", IvyJson::Null),
            ("reply", IvyJson::Null),
            ("code", IvyJson::Null),
        ]);
        let charge = stringify(&value, usize::MAX)?.len() + OUTCOME_RESERVATION;
        require(
            self.bytes() + charge + OUTCOME_RESERVATION + 65536 <= self.limits.max_journal_bytes,
            "native_input_capacity",
            "Native journal cannot reserve this pending input, its answer and cleanup.",
        )?;
        self.inputs.insert(
            self.input_key(identity)?,
            Row {
                value: value.clone(),
                charge,
            },
        );
        Ok(value)
    }
    fn save_input(&mut self, value: IvyJson) -> IvyResult<IvyJson> {
        let key = self.input_key(field(&value, "identity")?)?;
        let row = self.inputs.get(&key);
        let size = stringify(&value, usize::MAX)?.len();
        require(
            row.is_some_and(|row| size <= row.charge),
            "native_input_capacity",
            "Input result exceeds its reserved memory.",
        )?;
        let charge = if pending(&value) {
            row.unwrap().charge
        } else {
            size
        };
        self.inputs.insert(
            key,
            Row {
                value: value.clone(),
                charge,
            },
        );
        Ok(value)
    }
    pub fn dispatch_answer(
        &mut self,
        operation: &Identity,
        identity: &IvyJson,
        reply: &IvyJson,
    ) -> IvyResult<IvyJson> {
        let value = self.input(identity)?;
        let mut accepted = self.existing(operation)?;
        require(
            value.as_ref().is_some_and(|v| is(v, "state", "pending"))
                && identity.get("epoch")
                    == self
                        .epoch
                        .as_ref()
                        .map(|v| IvyJson::String(v.clone()))
                        .as_ref(),
            "native_input_expired",
            "Only the exact pending input in the current native connection can be answered.",
        )?;
        let request_hash = hash_json(&IvyJson::object([
            ("method", "agent.answer".into()),
            (
                "params",
                IvyJson::object([("identity", identity.clone()), ("reply", reply.clone())]),
            ),
        ]))?;
        require(
            is(&accepted, "phase", "accepted")
                && is(&accepted, "method", "agent.answer")
                && accepted.get("requestHash") == Some(&IvyJson::from(request_hash)),
            "mutation_conflict",
            "Answer dispatch must match its accepted original caller operation.",
        )?;
        let now = IvyJson::String((self.clock)());
        set(&mut accepted, "phase", "dispatched".into())?;
        set(&mut accepted, "epoch", field(identity, "epoch")?.clone())?;
        set(
            &mut accepted,
            "requestId",
            field(identity, "requestId")?.clone(),
        )?;
        set(&mut accepted, "updatedAt", now.clone())?;
        self.save(accepted)?;
        let mut value = value.unwrap();
        set(&mut value, "state", "answering".into())?;
        set(
            &mut value,
            "answerOperationId",
            IvyJson::String(operation.operation_id.clone()),
        )?;
        set(
            &mut value,
            "answerCallerPrincipalId",
            IvyJson::String(operation.caller_principal_id.clone()),
        )?;
        set(&mut value, "reply", reply.clone())?;
        set(&mut value, "updatedAt", now)?;
        self.save_input(value)
    }
    pub fn resolve_input(
        &mut self,
        epoch: &JsString,
        request_id: &IvyJson,
        thread_id: &JsString,
    ) -> IvyResult<Option<IvyJson>> {
        self.resolve_input_json(epoch, request_id, &IvyJson::String(thread_id.clone()))
    }
    /// Native envelopes are structurally validated. Preserve the original
    /// nullable-thread comparison even for an untyped notification field.
    pub fn resolve_input_json(
        &mut self,
        epoch: &JsString,
        request_id: &IvyJson,
        thread_id: &IvyJson,
    ) -> IvyResult<Option<IvyJson>> {
        require(
            self.epoch.as_ref() == Some(epoch),
            "native_state_conflict",
            "Input resolution must come from the current native connection.",
        )?;
        let identity = IvyJson::object([
            (
                "serviceNodeId",
                IvyJson::String(self.owner.service_node_id.clone()),
            ),
            ("epoch", IvyJson::String(epoch.clone())),
            ("requestId", request_id.clone()),
        ]);
        let Some(mut value) = self.input(&identity)? else {
            return Ok(None);
        };
        require(
            value.get("threadId") == Some(&IvyJson::Null)
                || value.get("threadId") == Some(thread_id),
            "native_input_conflict",
            "Native input resolution names a different thread.",
        )?;
        if !pending(&value) {
            return Ok(Some(value));
        }
        let now = IvyJson::String((self.clock)());
        let answering = is(&value, "state", "answering");
        if answering {
            let operation = Identity {
                caller_principal_id: text(&value, "answerCallerPrincipalId")?.clone(),
                operation_id: text(&value, "answerOperationId")?.clone(),
            };
            let mut operation = self.existing(&operation)?;
            require(
                is(&operation, "phase", "dispatched")
                    && operation.get("epoch") == Some(&IvyJson::String(epoch.clone()))
                    && operation.get("requestId") == Some(request_id),
                "native_state_conflict",
                "Native answer outcome has no matching dispatched intent.",
            )?;
            set(&mut operation, "phase", "succeeded".into())?;
            set(
                &mut operation,
                "reply",
                IvyJson::object([(
                    "result",
                    IvyJson::object([("nativeRequestResolved", true.into())]),
                )]),
            )?;
            set(&mut operation, "code", IvyJson::Null)?;
            set(&mut operation, "updatedAt", now.clone())?;
            self.save(operation)?;
        }
        set(
            &mut value,
            "state",
            if answering {
                "answered".into()
            } else {
                "expired".into()
            },
        )?;
        set(
            &mut value,
            "code",
            if answering {
                IvyJson::Null
            } else {
                "native_request_resolved".into()
            },
        )?;
        set(&mut value, "updatedAt", now)?;
        self.save_input(value).map(Some)
    }
    fn retire_inputs(&mut self, code: &JsString) -> IvyResult<()> {
        let values = self
            .inputs
            .values()
            .filter(|row| pending(&row.value))
            .map(|row| row.value.clone())
            .collect::<Vec<_>>();
        for mut value in values {
            let answering = is(&value, "state", "answering");
            set(
                &mut value,
                "state",
                if answering {
                    "outcome_unknown".into()
                } else {
                    "expired".into()
                },
            )?;
            set(&mut value, "code", IvyJson::String(code.clone()))?;
            set(&mut value, "updatedAt", IvyJson::String((self.clock)()))?;
            self.save_input(value)?;
        }
        Ok(())
    }
    pub fn inputs(&self, query: &IvyJson) -> IvyResult<IvyJson> {
        validate_agent("PendingInputQuery", query)?;
        if let Some(identity) = query.get("identity") {
            let value = self.input(identity)?;
            return Ok(IvyJson::object([
                (
                    "epoch",
                    self.epoch
                        .as_ref()
                        .cloned()
                        .map(IvyJson::String)
                        .unwrap_or(IvyJson::Null),
                ),
                ("items", IvyJson::Array(value.into_iter().collect())),
                ("truncated", false.into()),
            ]));
        }
        let include_expired = query.get("includeExpired") == Some(&IvyJson::Bool(true));
        let mut values = self
            .inputs
            .values()
            .map(|row| &row.value)
            .filter(|value| include_expired || pending(value))
            .collect::<Vec<_>>();
        // All generated timestamps are fixed-width UTC ISO strings, so their
        // descending ordinal order is their localeCompare order.
        values.sort_by(|left, right| {
            (!pending(left)).cmp(&!pending(right)).then_with(|| {
                right
                    .get("updatedAt")
                    .and_then(IvyJson::as_js_string)
                    .cmp(&left.get("updatedAt").and_then(IvyJson::as_js_string))
            })
        });
        let limit = query
            .get("limit")
            .and_then(IvyJson::as_f64)
            .unwrap_or(128.0) as usize;
        let mut items = Vec::new();
        let mut bytes = 0;
        for value in &values {
            let size = stringify(value, usize::MAX)?.len();
            if items.len() >= limit || bytes + size > MANAGEMENT_FRAME_BYTES - 1024 * 1024 {
                break;
            }
            items.push((*value).clone());
            bytes += size;
        }
        let truncated = items.len() < values.len();
        Ok(IvyJson::object([
            (
                "epoch",
                self.epoch
                    .as_ref()
                    .cloned()
                    .map(IvyJson::String)
                    .unwrap_or(IvyJson::Null),
            ),
            ("items", IvyJson::Array(items)),
            ("truncated", truncated.into()),
        ]))
    }
    pub fn reserve_notification_sequence(&self) -> IvyJson {
        IvyJson::object([
            ("start", IvyJson::Number(0.0)),
            ("end", IvyJson::Number(9_007_199_254_740_991.0)),
        ])
    }
    pub fn observed_methods(&self) -> IvyJson {
        IvyJson::Array(
            self.observations
                .values()
                .rev()
                .take(256)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        )
    }
}
