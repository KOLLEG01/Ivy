//! Bounded connection-local operations and inputs. SQLite owns durable receipts.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::{NativeFrameKind, validate_agent, validate_agent_frame},
    hash::hash_json,
    json::{canonical, parse, stringify},
    limits::{
        MANAGEMENT_FRAME_BYTES, NATIVE_ANSWER_FRAME_BYTES, NATIVE_FRAME_BYTES,
        NATIVE_REQUEST_FRAME_BYTES,
    },
    wire_schema::validate_shared,
};
use std::{collections::HashMap, mem::size_of, sync::Arc};

const OUTCOME_RESERVATION: usize = NATIVE_FRAME_BYTES + 65536;
const OBSERVATION_BYTES: usize = 1024 * 1024;
const OBSERVATION_ENTRY_BYTES: usize = 64 * 1024;
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
// Keep unknown payloads as the lossless text already produced for frame sizing.
// Only selected/read rows become temporary JSON trees at the API boundary.
struct Row<S> {
    json: Box<str>,
    charge: usize,
    state: S,
}
impl<S> Row<S> {
    fn read(&self) -> IvyResult<IvyJson> {
        parse(&self.json, usize::MAX)
    }
}
struct OperationState {
    unfinished: bool,
    clock: bool,
    durable: bool,
}
impl OperationState {
    fn new(value: &IvyJson, durable: bool) -> Self {
        Self {
            unfinished: !terminal(value),
            clock: is(value, "callerPrincipalId", "ivy-native-clock"),
            durable,
        }
    }
}
struct InputState {
    pending: bool,
    updated_at: JsString,
}
impl InputState {
    fn new(value: &IvyJson) -> IvyResult<Self> {
        Ok(Self {
            pending: pending(value),
            updated_at: text(value, "updatedAt")?.clone(),
        })
    }
    fn bytes(&self) -> usize {
        self.updated_at.0.capacity() * size_of::<u16>()
    }
}
#[derive(Clone)]
struct Observation {
    method: JsString,
    succeeded_at: Option<JsString>,
    failed_at: Option<JsString>,
    code: Option<JsString>,
}
impl Observation {
    fn bytes(&self) -> usize {
        [&self.method]
            .into_iter()
            .chain(self.succeeded_at.iter())
            .chain(self.failed_at.iter())
            .chain(self.code.iter())
            .map(|value| value.0.capacity() * size_of::<u16>())
            .sum()
    }
    fn value(&self) -> IvyJson {
        let optional =
            |value: &Option<JsString>| value.clone().map(IvyJson::String).unwrap_or(IvyJson::Null);
        IvyJson::object([
            ("method", IvyJson::String(self.method.clone())),
            ("lastSucceededAt", optional(&self.succeeded_at)),
            ("lastFailedAt", optional(&self.failed_at)),
            ("lastCode", optional(&self.code)),
        ])
    }
}
// One ordered row allocation and shared keys. Batch removal compacts the vector
// once; capacity accounting includes both containers, even after eviction.
struct Rows<V> {
    indices: HashMap<Arc<str>, usize>,
    rows: Vec<(Arc<str>, V)>,
}
impl<V> Rows<V> {
    fn new() -> Self {
        Self {
            indices: HashMap::new(),
            rows: Vec::new(),
        }
    }
    fn len(&self) -> usize {
        self.indices.len()
    }
    fn get(&self, key: &str) -> Option<&V> {
        self.indices
            .get(key)
            .and_then(|index| self.rows.get(*index))
            .map(|(_, value)| value)
    }
    fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.indices
            .get(key)
            .and_then(|index| self.rows.get_mut(*index))
            .map(|(_, value)| value)
    }
    fn insert(&mut self, key: String, value: V) {
        if let Some(index) = self.indices.get(key.as_str()) {
            self.rows[*index].1 = value;
            return;
        }
        let key: Arc<str> = key.into();
        self.indices.insert(key.clone(), self.rows.len());
        self.rows.push((key, value));
    }
    fn remove(&mut self, key: &str) {
        if let Some(index) = self.indices.remove(key) {
            self.rows.remove(index);
            self.reindex();
        }
    }
    fn retain(&mut self, mut keep: impl FnMut(&V) -> bool) {
        let previous = self.rows.len();
        self.rows.retain(|(key, value)| {
            if keep(value) {
                true
            } else {
                self.indices.remove(key);
                false
            }
        });
        if previous == self.rows.len() {
            return;
        }
        self.reindex();
        if self.rows.len() < self.rows.capacity() / 4 {
            self.shrink();
        }
    }
    fn reindex(&mut self) {
        for (index, (key, _)) in self.rows.iter().enumerate() {
            *self.indices.get_mut(key).unwrap() = index;
        }
    }
    fn shrink(&mut self) {
        self.rows.shrink_to_fit();
        self.indices.shrink_to_fit();
    }
    fn bytes(&self, charge: impl Fn(&V) -> usize) -> usize {
        // HashMap capacity excludes spare buckets/control bytes. A conservative
        // two buckets per usable slot avoids depending on its private layout.
        self.rows.capacity() * size_of::<(Arc<str>, V)>()
            + if self.indices.capacity() == 0 {
                0
            } else {
                self.indices.capacity() * 2 * (size_of::<(Arc<str>, usize)>() + 1) + 16
            }
            + self
                .rows
                .iter()
                // Shared-key counters and their alignment padding.
                .map(|(key, value)| key.len() + 3 * size_of::<usize>() + charge(value))
                .sum::<usize>()
    }
    fn values(&self) -> impl DoubleEndedIterator<Item = &V> {
        self.rows.iter().map(|(_, value)| value)
    }
    fn entries(&self) -> impl Iterator<Item = (&Arc<str>, &V)> {
        self.rows.iter().map(|(key, value)| (key, value))
    }
    fn clear(&mut self) {
        *self = Self::new();
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
fn merge_object(mut base: IvyJson, value: &IvyJson) -> IvyResult<IvyJson> {
    let entries = value
        .as_object()
        .ok_or_else(|| IvyError::new("invalid_arguments", "Native reply must be an object."))?;
    for (key, value) in entries {
        set_key(&mut base, key.clone(), value.clone())?;
    }
    Ok(base)
}
pub struct NativeJournal {
    owner: Owner,
    limits: Limits,
    operations: Rows<Row<OperationState>>,
    inputs: Rows<Row<InputState>>,
    observations: Rows<Observation>,
    revisions: HashMap<String, f64>,
    epoch: Option<JsString>,
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
        self.revisions = HashMap::new();
        self.epoch = None;
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
        let next = previous.max(minimum) + 1.0;
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
        self.operations.bytes(|row| row.charge)
            + self.inputs.bytes(|row| row.charge)
            + self.observations.bytes(Observation::bytes)
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
        self.operations
            .get(&Self::key(identity)?)
            .map(Row::read)
            .transpose()
    }
    pub fn forget(&mut self, identity: &Identity) -> IvyResult<()> {
        self.operations.remove(&Self::key(identity)?);
        Ok(())
    }
    pub fn retained_in_hive(&mut self, identity: &Identity) -> IvyResult<()> {
        if let Some(row) = self.operations.get_mut(&Self::key(identity)?) {
            row.state.durable = true;
        }
        Ok(())
    }
    fn existing(&self, identity: &Identity) -> IvyResult<IvyJson> {
        self.get(identity)?
            .ok_or_else(|| IvyError::new("not_found", "Operation is not in this connection."))
    }
    fn compact(&mut self) {
        self.operations
            .retain(|row| row.state.unfinished || !(row.state.durable || row.state.clock));
        self.inputs.retain(|row| row.state.pending);
    }
    fn save(&mut self, mut value: IvyJson) -> IvyResult<IvyJson> {
        let key = Self::key(&Identity::from_operation(&value)?)?;
        let json = stringify(&value, usize::MAX)?.into_boxed_str();
        let size = json.len();
        let row = self.operations.get(&key);
        require(
            row.is_some_and(|row| size <= row.charge),
            "native_journal_capacity",
            "Outcome exceeds reserved memory.",
        )?;
        let completed = terminal(&value);
        let charge = if completed { size } else { row.unwrap().charge };
        let state = OperationState::new(&value, row.unwrap().state.durable);
        self.operations.insert(
            key,
            Row {
                json,
                charge,
                state,
            },
        );
        if completed {
            let method = text(&value, "method")?;
            let key = canonical(&IvyJson::String(method.clone()), MANAGEMENT_FRAME_BYTES)?;
            self.observe_method(key, method, &value)?;
            if let Some(callback) = self.on_terminal.as_mut() {
                callback(&mut value)?;
            }
        }
        Ok(value)
    }
    fn observe_method(&mut self, key: String, method: &JsString, value: &IvyJson) -> IvyResult<()> {
        let mut observed = self
            .observations
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Observation {
                method: method.clone(),
                succeeded_at: None,
                failed_at: None,
                code: None,
            });
        let now = text(value, "updatedAt")?.clone();
        if is(value, "phase", "succeeded") {
            observed.succeeded_at = Some(now);
        } else {
            observed.failed_at = Some(now);
            observed.code = field(value, "code")?.as_js_string().cloned();
        }
        if key.len() + observed.bytes() > OBSERVATION_ENTRY_BYTES {
            return Ok(());
        }
        self.observations.insert(key, observed);
        while self.observations.len() > 256
            || self.observations.bytes(Observation::bytes) > OBSERVATION_BYTES
            || self.bytes() > self.limits.max_journal_bytes
        {
            let key = self.observations.entries().next().unwrap().0.clone();
            self.observations.remove(&key);
            self.observations.shrink();
        }
        Ok(())
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
            let answer = merge_object(
                IvyJson::object([(
                    "operationId",
                    IvyJson::String(identity.operation_id.clone()),
                )]),
                params,
            )?;
            validate_agent("AnswerInput", &answer)?;
            let request_id = field(field(&answer, "identity")?, "requestId")?.clone();
            stringify(
                &merge_object(
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
        let json = stringify(&value, usize::MAX)?.into_boxed_str();
        let charge = json.len() + if prevent { 0 } else { OUTCOME_RESERVATION };
        let cleanup = prevent
            || ["thread/unsubscribe", "turn/interrupt", "agent.answer"]
                .iter()
                .any(|name| method == &JsString::from(*name));
        require(
            self.operations.len() + if cleanup { 0 } else { 4 } < self.limits.max_operations,
            "native_journal_capacity",
            "Connection memory is full; no native request sent.",
        )?;
        let key = Self::key(identity)?;
        self.operations.insert(
            key.clone(),
            Row {
                json,
                charge,
                state: OperationState::new(&value, false),
            },
        );
        // Check the capacities actually allocated, including any table growth.
        // Admission precedes native effects; rollback releases spare containers.
        if self.bytes()
            + if cleanup {
                0
            } else {
                OUTCOME_RESERVATION + 65536
            }
            > self.limits.max_journal_bytes
        {
            self.operations.remove(&key);
            self.operations.shrink();
            return Err(IvyError::new(
                "native_journal_capacity",
                "Connection memory is full; no native request sent.",
            ));
        }
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
        let keys = self
            .operations
            .entries()
            .filter(|(_, row)| row.state.unfinished)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            let mut value = self.operations.get(&key).unwrap().read()?;
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
            &merge_object(IvyJson::object([("id", request_id.clone())]), reply)?,
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
        self.inputs
            .get(&self.input_key(identity)?)
            .map(Row::read)
            .transpose()
    }
    pub fn pending_input_count(&self) -> usize {
        self.inputs.values().filter(|row| row.state.pending).count()
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
        let json = stringify(&value, usize::MAX)?.into_boxed_str();
        let state = InputState::new(&value)?;
        let charge = json.len() + state.bytes() + OUTCOME_RESERVATION;
        let key = self.input_key(identity)?;
        self.inputs.insert(
            key.clone(),
            Row {
                json,
                charge,
                state,
            },
        );
        if self.bytes() + OUTCOME_RESERVATION + 65536 > self.limits.max_journal_bytes {
            self.inputs.remove(&key);
            self.inputs.shrink();
            return Err(IvyError::new(
                "native_input_capacity",
                "Native journal cannot reserve this pending input, its answer and cleanup.",
            ));
        }
        Ok(value)
    }
    fn save_input(&mut self, value: IvyJson) -> IvyResult<IvyJson> {
        let key = self.input_key(field(&value, "identity")?)?;
        let row = self.inputs.get(&key);
        let json = stringify(&value, usize::MAX)?.into_boxed_str();
        let state = InputState::new(&value)?;
        let size = json.len() + state.bytes();
        require(
            row.is_some_and(|row| size <= row.charge),
            "native_input_capacity",
            "Input result exceeds its reserved memory.",
        )?;
        let charge = if state.pending {
            row.unwrap().charge
        } else {
            size
        };
        self.inputs.insert(
            key,
            Row {
                json,
                charge,
                state,
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
        let keys = self
            .inputs
            .entries()
            .filter(|(_, row)| row.state.pending)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            let mut value = self.inputs.get(&key).unwrap().read()?;
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
            .filter(|row| include_expired || row.state.pending)
            .collect::<Vec<_>>();
        // All generated timestamps are fixed-width UTC ISO strings, so their
        // descending ordinal order is their localeCompare order.
        values.sort_by(|left, right| {
            (!left.state.pending)
                .cmp(&!right.state.pending)
                .then_with(|| right.state.updated_at.cmp(&left.state.updated_at))
        });
        let limit = query
            .get("limit")
            .and_then(IvyJson::as_f64)
            .unwrap_or(128.0) as usize;
        let mut items = Vec::new();
        let mut bytes = 0;
        for row in &values {
            let size = row.json.len();
            if items.len() >= limit || bytes + size > MANAGEMENT_FRAME_BYTES - 1024 * 1024 {
                break;
            }
            items.push(row.read()?);
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
        IvyJson::Array(self.observations.values().map(Observation::value).collect())
    }
}
