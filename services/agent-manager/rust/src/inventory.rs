//! Bounded native observations and guarded Hive publication.
use crate::{
    native_journal::{JournalClock, NativeJournal},
    project_model::{NativeProject, NativeProjects, ProjectSnapshot},
    registry::{NATIVE_INVENTORY_SCHEMA_VERSION, NATIVE_PROJECT_SCHEMA_VERSION},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, agent_schema::validate_agent,
    json::canonical_size, wire_schema::validate_shared,
};
use ivy_host_runtime::codex_rpc::{NativeRpc, RequestOptions as NativeOptions};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    service::{InvocationContext, ServiceConnection, ServiceFuture},
};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};

const SNAPSHOT_BYTES: usize = 9 * 1024 * 1024;
const SNAPSHOT_HEAP_BYTES: usize = 32 * 1024 * 1024;
const PAGE_ITEMS: usize = 100;
const MAXIMUM_PAGES: usize = 64;
const CURSOR_UNITS: usize = 8192;
const CURSOR_BYTES: usize = 512 * 1024;
static NULL: IvyJson = IvyJson::Null;
pub type InventoryRead =
    dyn Fn(JsString, IvyJson, NativeOptions) -> ServiceFuture<IvyJson> + Send + Sync;
pub type InventoryClock = dyn Fn() -> f64 + Send + Sync;

#[derive(Clone, Copy)]
pub(crate) enum ThreadChange {
    Read { archived: bool },
    Removed,
}
pub(crate) struct ThreadChanges {
    // The first admitted hint owns this batch time through coalescing and waiting.
    pub observed_at: Box<str>,
    pub entries: BTreeMap<JsString, ThreadChange>,
}
pub(crate) struct ThreadDelta {
    pub snapshot: IvyJson,
    pub removed: Vec<JsString>,
}

fn require(test: bool, code: &str, message: &str) -> IvyResult<()> {
    if test {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn fields(value: &IvyJson) -> IvyResult<&IvyJson> {
    require(
        value.as_object().is_some(),
        "native_inventory_invalid",
        "Native inventory expected an object.",
    )?;
    Ok(value)
}
fn property<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap_or(&NULL)
}
fn resolve<'a>(root: &'a IvyJson, value: &'a IvyJson) -> IvyResult<&'a IvyJson> {
    let node = fields(value)?;
    let reference = property(node, "$ref");
    if reference.is_null()
        || reference
            .as_js_string()
            .is_some_and(|value| value.0.is_empty())
    {
        return Ok(node);
    }
    let reference = reference.as_js_string();
    let prefix = JsString::from("#/$defs/");
    require(
        reference.is_some_and(|r| {
            r.0.starts_with(&prefix.0)
                && r.0.len() > prefix.0.len()
                && !r.0[prefix.0.len()..].contains(&47)
        }),
        "native_catalog_mismatch",
        "Thread source schema must use a local definition.",
    )?;
    let key = JsString(reference.unwrap().0[prefix.0.len()..].to_vec());
    fields(
        fields(property(root, "$defs"))?
            .get_key(&key)
            .unwrap_or(&NULL),
    )
}
pub fn native_source_kinds(catalog: &IvyJson) -> IvyResult<Vec<JsString>> {
    let requests = catalog
        .get("clientRequests")
        .and_then(IvyJson::as_array)
        .ok_or_else(IvyError::internal)?;
    let schema = requests
        .iter()
        .find(|item| item.get("method") == Some(&"thread/list".into()))
        .and_then(|item| item.get("inputSchema"));
    require(
        schema.is_some_and(|v| matches!(v, IvyJson::Array(_) | IvyJson::Object(_))),
        "native_catalog_mismatch",
        "Native thread inventory has no generated input schema.",
    )?;
    let root = schema.unwrap();
    let source = property(
        fields(property(resolve(root, root)?, "properties"))?,
        "sourceKinds",
    );
    let kinds = property(
        resolve(root, property(resolve(root, source)?, "items"))?,
        "enum",
    )
    .as_array();
    require(
        kinds.is_some_and(|v| !v.is_empty() && v.iter().all(|v| v.as_js_string().is_some())),
        "native_catalog_mismatch",
        "Installed thread source kinds are not an explicit native enum.",
    )?;
    Ok(kinds
        .unwrap()
        .iter()
        .map(|kind| kind.as_js_string().unwrap().clone())
        .collect())
}

/// Keep the actual context/generation close reason and cancellation token.
#[derive(Clone)]
pub struct InventorySignal {
    check: Arc<dyn Fn() -> IvyResult<()> + Send + Sync>,
    options: RequestOptions,
}
impl InventorySignal {
    pub fn from_context(context: InvocationContext) -> Self {
        let options = RequestOptions {
            cancellation: context.signal.clone(),
            ..Default::default()
        };
        Self {
            check: Arc::new(move || context.throw_if_aborted()),
            options,
        }
    }
    pub fn from_connection(connection: ServiceConnection) -> Self {
        let options = RequestOptions {
            cancellation: connection.signal(),
            ..Default::default()
        };
        Self {
            check: Arc::new(move || connection.throw_if_aborted()),
            options,
        }
    }
    pub(crate) fn check(&self) -> IvyResult<()> {
        (self.check)()
    }
    pub(crate) fn is_aborted(&self) -> bool {
        self.options.cancellation.is_cancelled()
    }
}

pub struct NativeInventory {
    supports_projects: bool,
    epoch: JsString,
    source_kinds: Vec<JsString>,
    read: Arc<InventoryRead>,
    projects: Mutex<Option<ProjectSnapshot>>,
    now: Arc<InventoryClock>,
    observed_at: Arc<JournalClock>,
}
struct Reader<'a> {
    owner: &'a NativeInventory,
    signal: Option<&'a InventorySignal>,
    budget: u64,
    deadline: f64,
}
impl NativeInventory {
    pub fn new(catalog: &IvyJson, epoch: JsString, rpc: NativeRpc) -> IvyResult<Self> {
        Self::with_reader(
            catalog,
            epoch,
            Arc::new(move |method, params, options| {
                let rpc = rpc.clone();
                Box::pin(async move { rpc.request(method, params, options).await })
            }),
            Arc::new(|| chrono::Utc::now().timestamp_millis() as f64),
            Arc::new(|| {
                chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into()
            }),
        )
    }
    pub fn with_reader(
        catalog: &IvyJson,
        epoch: JsString,
        read: Arc<InventoryRead>,
        now: Arc<InventoryClock>,
        observed_at: Arc<JournalClock>,
    ) -> IvyResult<Self> {
        let source_kinds = native_source_kinds(catalog)?;
        let supports_projects = catalog
            .get("clientRequests")
            .and_then(IvyJson::as_array)
            .unwrap()
            .iter()
            .any(|request| request.get("method") == Some(&"project/list".into()));
        Ok(Self {
            supports_projects,
            epoch,
            source_kinds,
            read,
            projects: Mutex::new(None),
            now,
            observed_at,
        })
    }
    fn reader<'a>(&'a self, signal: Option<&'a InventorySignal>, budget: u64) -> Reader<'a> {
        Reader {
            owner: self,
            signal,
            budget,
            deadline: (self.now)() + budget as f64,
        }
    }
    pub async fn collect_threads(&self, signal: Option<&InventorySignal>) -> IvyResult<IvyJson> {
        let reader = self.reader(signal, 120_000);
        match self.thread_snapshot(&reader, true).await {
            Err(error) if error.code == "native_inventory_changed" => {
                reader.check()?;
                self.thread_snapshot(&reader, true).await
            }
            result => result,
        }
    }
    /// A current-partition observation cannot replace the complete Hive set.
    /// The manager admits it only as a delta with unchanged visible membership.
    pub(crate) async fn collect_current_threads(
        &self,
        signal: Option<&InventorySignal>,
    ) -> IvyResult<IvyJson> {
        self.thread_snapshot(&self.reader(signal, 120_000), false)
            .await
    }
    pub(crate) async fn collect_thread_changes(
        &self,
        changes: ThreadChanges,
        signal: Option<&InventorySignal>,
    ) -> IvyResult<ThreadDelta> {
        require(
            changes.entries.len() <= 64,
            "native_inventory_capacity",
            "Targeted inventory work exceeds its pending-ID bound.",
        )?;
        let reader = self.reader(signal, 120_000);
        reader.check()?;
        let observed_at = (self.observed_at)();
        let reads = changes
            .entries
            .values()
            .any(|change| matches!(change, ThreadChange::Read { .. }));
        let owned: HashSet<_> = if reads {
            reader.loaded().await?.into_iter().collect()
        } else {
            HashSet::new()
        };
        let mut threads = Vec::new();
        let mut ids = HashSet::new();
        let mut removed = Vec::new();
        let (mut bytes, mut heap) = (0, 0);
        for (id, change) in &changes.entries {
            match change {
                ThreadChange::Removed => removed.push(id.clone()),
                ThreadChange::Read { archived } => {
                    let result = reader
                        .read(
                            "thread/read",
                            IvyJson::object([
                                ("threadId", IvyJson::String(id.clone())),
                                ("includeTurns", false.into()),
                            ]),
                        )
                        .await?;
                    let thread = property(&result, "thread");
                    require(
                        thread.get("id").and_then(IvyJson::as_js_string) == Some(id),
                        "native_inventory_invalid",
                        "Native thread read returned a different identity.",
                    )?;
                    add_thread(
                        thread,
                        *archived,
                        &owned,
                        &self.epoch,
                        &mut threads,
                        &mut ids,
                        &mut bytes,
                        &mut heap,
                    )?;
                }
            }
        }
        if reads {
            let after: HashSet<_> = reader.loaded().await?.into_iter().collect();
            require(
                owned == after,
                "native_inventory_changed",
                "Loaded ownership changed during the targeted observation.",
            )?;
        }
        reader.check()?;
        let at = IvyJson::String(observed_at);
        let snapshot = IvyJson::object([
            ("observedAt", changes.observed_at.into_string().into()),
            (
                "threads",
                IvyJson::Array(
                    threads
                        .into_iter()
                        .map(|summary| {
                            IvyJson::object([
                                ("nativeId", property(&summary, "nativeId").clone()),
                                ("summary", summary),
                                ("observedAt", at.clone()),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]);
        canonical_size(&snapshot, SNAPSHOT_BYTES)?;
        Ok(ThreadDelta { snapshot, removed })
    }
    /// Shutdown needs current activity, not persisted conversations or turns.
    pub async fn has_active_threads(&self) -> IvyResult<bool> {
        let reader = self.reader(None, 120_000);
        for id in reader.loaded().await? {
            let result = reader
                .read(
                    "thread/read",
                    IvyJson::object([
                        ("threadId", IvyJson::String(id)),
                        ("includeTurns", false.into()),
                    ]),
                )
                .await?;
            let status = result
                .required("thread")?
                .required("status")?
                .required("type")?
                .as_js_string()
                .ok_or_else(|| {
                    IvyError::new(
                        "native_inventory_invalid",
                        "Native activity must have a known thread status.",
                    )
                })?;
            if status.eq_str("active") {
                return Ok(true);
            }
            require(
                ["idle", "notLoaded", "systemError"]
                    .iter()
                    .any(|value| status.eq_str(value)),
                "native_inventory_invalid",
                "Native activity must have a known thread status.",
            )?;
        }
        Ok(false)
    }
    async fn thread_snapshot(&self, reader: &Reader<'_>, complete: bool) -> IvyResult<IvyJson> {
        // A complete observation may span many native pages. Its oldest data
        // must not become fresh again at the scan or publication's completion.
        let observed_at = IvyJson::String((self.observed_at)());
        let before = reader.loaded().await?;
        let owned: HashSet<_> = before.iter().cloned().collect();
        let mut threads = Vec::new();
        let mut ids = HashSet::new();
        let mut bytes = 0;
        let mut heap = 0;
        for archived in [false, true] {
            if archived && !complete {
                break;
            }
            reader
                .pages(
                    "thread/list",
                    IvyJson::object([
                        ("archived", archived.into()),
                        (
                            "sourceKinds",
                            IvyJson::Array(
                                self.source_kinds
                                    .iter()
                                    .cloned()
                                    .map(IvyJson::String)
                                    .collect(),
                            ),
                        ),
                        ("modelProviders", IvyJson::Array(vec![])),
                        ("sortKey", "recency_at".into()),
                        ("sortDirection", "desc".into()),
                        ("useStateDbOnly", true.into()),
                    ]),
                    5000,
                    |value| {
                        add_thread(
                            value,
                            archived,
                            &owned,
                            &self.epoch,
                            &mut threads,
                            &mut ids,
                            &mut bytes,
                            &mut heap,
                        )
                    },
                )
                .await?;
        }
        for id in before.iter().filter(|_| complete) {
            if !ids.contains(id) {
                let result = reader
                    .read(
                        "thread/read",
                        IvyJson::object([
                            ("threadId", IvyJson::String(id.clone())),
                            ("includeTurns", false.into()),
                        ]),
                    )
                    .await?;
                add_thread(
                    property(&result, "thread"),
                    false,
                    &owned,
                    &self.epoch,
                    &mut threads,
                    &mut ids,
                    &mut bytes,
                    &mut heap,
                )?;
            }
        }
        let after: HashSet<_> = reader.loaded().await?.into_iter().collect();
        require(
            owned == after,
            "native_inventory_changed",
            "Loaded native ownership changed during enumeration; retain the previous snapshot.",
        )?;
        let result = IvyJson::object([
            ("observedAt", observed_at.clone()),
            (
                "threads",
                IvyJson::Array(
                    threads
                        .into_iter()
                        .map(|summary| {
                            IvyJson::object([
                                ("nativeId", property(&summary, "nativeId").clone()),
                                ("summary", summary),
                                ("observedAt", observed_at.clone()),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]);
        canonical_size(&result, SNAPSHOT_BYTES)?;
        reader.check()?;
        Ok(result)
    }
    pub fn current_project_snapshot(&self) -> IvyResult<Option<ProjectSnapshot>> {
        Ok(self
            .projects
            .lock()
            .map_err(|_| IvyError::internal())?
            .clone())
    }
    pub async fn collect_projects(&self, signal: Option<&InventorySignal>) -> IvyResult<IvyJson> {
        Ok(self.collect_project_snapshot(signal).await?.to_json())
    }
    pub async fn collect_project_snapshot(
        &self,
        signal: Option<&InventorySignal>,
    ) -> IvyResult<ProjectSnapshot> {
        require(
            self.supports_projects,
            "native_inventory_unavailable",
            "The connected Codex instance does not support project/list.",
        )?;
        let reader = self.reader(signal, 600_000);
        reader.check()?;
        let observed_at = (self.observed_at)();
        let mut projects = Vec::new();
        reader
            .pages(
                "project/list",
                IvyJson::object([
                    ("sortKey", "position".into()),
                    ("sortDirection", "asc".into()),
                ]),
                256,
                |value| {
                    let project = NativeProject::from_summary(&project_summary(value)?)?;
                    let size = (
                        canonical_size(&project.to_json(), SNAPSHOT_BYTES)?,
                        project.retained_bytes(),
                    );
                    projects.push(project);
                    Ok(size)
                },
            )
            .await?;
        let projects = NativeProjects::new(projects)?;
        let snapshot = ProjectSnapshot {
            observed_at,
            projects,
        };
        let wire = snapshot.to_json();
        validate_agent("ProjectsResult", property(&wire, "projects"))?;
        canonical_size(&wire, SNAPSHOT_BYTES)?;
        reader.check()?;
        *self.projects.lock().map_err(|_| IvyError::internal())? = Some(snapshot.clone());
        Ok(snapshot)
    }
}
impl Reader<'_> {
    fn check(&self) -> IvyResult<()> {
        if let Some(signal) = self.signal {
            signal.check()?;
        }
        require(
            (self.owner.now)() < self.deadline,
            "native_inventory_deadline",
            "Complete native inventory exceeded its observation deadline.",
        )
    }
    async fn read(&self, method: &str, params: IvyJson) -> IvyResult<IvyJson> {
        self.check()?;
        let timeout_ms = self
            .budget
            .min(crate::native_management::native_read_timeout_ms(
                &method.into(),
                &params,
            ));
        let reply = (self.owner.read)(
            method.into(),
            params,
            NativeOptions {
                timeout_ms,
                detach_on_timeout: true,
                ..Default::default()
            },
        )
        .await?;
        self.check()?;
        if let Some(error) = reply.get("error") {
            if error.is_null() {
                return Err(IvyError::internal());
            }
            let mut details = IvyJson::object([("method", method.into())]);
            if let Some(code) = error.get("code") {
                details.insert("nativeCode", code.clone());
            }
            return Err(IvyError::new(
                "native_inventory_unavailable",
                "A required native inventory read failed.",
            )
            .with_details(details));
        }
        let result = reply
            .into_object()
            .and_then(|fields| fields.into_iter().find(|(key, _)| key.eq_str("result")))
            .map(|(_, result)| result)
            .unwrap_or(IvyJson::Null);
        fields(&result)?;
        Ok(result)
    }
    async fn pages<F>(
        &self,
        method: &str,
        params: IvyJson,
        maximum: usize,
        mut consume: F,
    ) -> IvyResult<()>
    where
        F: FnMut(&IvyJson) -> IvyResult<(usize, usize)> + Send,
    {
        let mut cursors = HashSet::new();
        let mut cursor: Option<JsString> = None;
        let mut cursor_bytes = 0;
        let mut count = 0;
        let mut bytes = 0;
        let mut heap = 0;
        for page_number in 1..=MAXIMUM_PAGES {
            let mut params = params.clone();
            params.insert("limit", IvyJson::Number(PAGE_ITEMS as f64));
            if let Some(cursor) = &cursor {
                params.insert("cursor", IvyJson::String(cursor.clone()));
            }
            let page = self.read(method, params).await?;
            let data = property(&page, "data").as_array();
            let next = page.get("nextCursor");
            require(
                data.is_some() && next.is_some_and(|n| n.is_null() || n.as_js_string().is_some()),
                "native_inventory_invalid",
                "Native page must explicitly describe its continuation.",
            )?;
            let data = data.unwrap();
            require(
                data.len() <= PAGE_ITEMS && count + data.len() <= maximum,
                "native_inventory_capacity",
                "Native inventory exceeds its page or entry capacity.",
            )?;
            // Bound continuation work and retained opaque strings independently
            // of row counts: a filtered native page may contain no rows at all.
            let next = next.unwrap().as_js_string();
            if let Some(next) = next {
                require(
                    !next.0.is_empty() && !cursors.contains(next),
                    "native_inventory_invalid",
                    "Native inventory continuation must be nonempty and advance.",
                )?;
                let retained = std::mem::size_of::<JsString>() + next.0.len() * 2;
                require(
                    next.0.len() <= CURSOR_UNITS
                        && page_number < MAXIMUM_PAGES
                        && cursor_bytes + retained <= CURSOR_BYTES,
                    "native_inventory_capacity",
                    "Native inventory exceeds its continuation capacity.",
                )?;
                cursor_bytes += retained;
                cursors.insert(next.clone());
            }
            for value in data {
                let size = consume(value)?;
                bytes += size.0;
                heap += size.1;
                require(
                    bytes <= SNAPSHOT_BYTES && heap <= SNAPSHOT_HEAP_BYTES,
                    "native_inventory_capacity",
                    "Accumulated native inventory exceeds its byte capacity.",
                )?;
            }
            count += data.len();
            let Some(next) = next else {
                return Ok(());
            };
            cursor = Some(next.clone());
        }
        unreachable!("The last page must terminate or reject its continuation")
    }
    async fn loaded(&self) -> IvyResult<Vec<JsString>> {
        let mut ids = Vec::new();
        self.pages(
            "thread/loaded/list",
            IvyJson::object::<&str>([]),
            5000,
            |value| {
                let id = value.as_js_string().ok_or_else(|| {
                    IvyError::new(
                        "native_inventory_invalid",
                        "Loaded native thread IDs must be strings.",
                    )
                })?;
                let size = snapshot_size(value)?;
                ids.push(id.clone());
                Ok(size)
            },
        )
        .await?;
        let mut seen = HashSet::new();
        for id in &ids {
            require(
                seen.insert(id),
                "native_inventory_invalid",
                "Loaded native thread IDs must be unique strings.",
            )?;
        }
        Ok(ids)
    }
}
fn safe_nonnegative(value: &IvyJson) -> bool {
    value
        .as_f64()
        .is_some_and(|v| v >= 0.0 && v <= 9_007_199_254_740_991.0 && v.fract() == 0.0)
}
fn slice(value: &IvyJson, limit: usize) -> IvyResult<IvyJson> {
    match value {
        IvyJson::String(v) => Ok(IvyJson::String(JsString(
            v.0[..v.0.len().min(limit)].to_vec(),
        ))),
        IvyJson::Array(v) => Ok(IvyJson::Array(v[..v.len().min(limit)].to_vec())),
        _ => Err(IvyError::internal()),
    }
}
fn snapshot_size(value: &IvyJson) -> IvyResult<(usize, usize)> {
    Ok((
        canonical_size(value, SNAPSHOT_BYTES)?,
        value.retained_bytes(),
    ))
}
fn add_thread(
    value: &IvyJson,
    archived: bool,
    owned: &HashSet<JsString>,
    epoch: &JsString,
    threads: &mut Vec<IvyJson>,
    ids: &mut HashSet<JsString>,
    bytes: &mut usize,
    heap: &mut usize,
) -> IvyResult<(usize, usize)> {
    let thread = fields(value)?;
    let id = property(thread, "id").as_js_string();
    require(
        !id.is_some_and(|id| ids.contains(id)),
        "native_inventory_changed",
        "Thread enumeration repeated an ID; retain the last complete snapshot.",
    )?;
    let source = property(thread, "threadSource");
    let project = property(thread, "projectId");
    let recency = property(thread, "recencyAt");
    require(
        source.is_null() || source.as_js_string().is_some(),
        "native_inventory_invalid",
        "Native thread classification must be explicit text or absent.",
    )?;
    require(
        project.is_null() || project.as_js_string().is_some(),
        "native_inventory_invalid",
        "Native thread project assignment must be explicit text or absent.",
    )?;
    require(
        recency.is_null() || safe_nonnegative(recency),
        "native_inventory_invalid",
        "Native thread recency must be a nonnegative integer or absent.",
    )?;
    let attached = id.is_some_and(|id| owned.contains(id));
    let mut summary = IvyJson::object([
        (
            "owner",
            if attached {
                "this-native-connection"
            } else {
                "historical-unattached"
            }
            .into(),
        ),
        (
            "epoch",
            if attached {
                IvyJson::String(epoch.clone())
            } else {
                IvyJson::Null
            },
        ),
        ("preview", slice(property(thread, "preview"), 2048)?),
        (
            "name",
            if thread.get("name") == Some(&IvyJson::Null) {
                IvyJson::Null
            } else {
                slice(property(thread, "name"), 512)?
            },
        ),
        (
            "canAcceptDirectInput",
            property(thread, "canAcceptDirectInput")
                .as_bool()
                .map(IvyJson::Bool)
                .unwrap_or(IvyJson::Null),
        ),
        ("projectId", project.clone()),
        ("recencyAt", recency.clone()),
        ("threadSource", source.clone()),
        ("archived", archived.into()),
    ]);
    for (from, to) in [
        ("id", "nativeId"),
        ("cwd", "cwd"),
        ("status", "status"),
        ("source", "source"),
        ("updatedAt", "updatedAt"),
        ("ephemeral", "ephemeral"),
    ] {
        if let Some(value) = thread.get(from) {
            summary.insert(to, value.clone());
        }
    }
    validate_agent("ThreadSummary", &summary)?;
    let size = snapshot_size(&summary)?;
    *bytes += size.0;
    *heap += size.1;
    require(
        *bytes <= SNAPSHOT_BYTES && *heap <= SNAPSHOT_HEAP_BYTES,
        "native_inventory_capacity",
        "Complete native thread summaries exceed their byte capacity.",
    )?;
    threads.push(summary);
    ids.insert(id.unwrap().clone());
    require(
        threads.len() <= 5000,
        "native_inventory_capacity",
        "Complete native thread inventory exceeds its declared capacity.",
    )?;
    Ok(size)
}
fn project_summary(value: &IvyJson) -> IvyResult<IvyJson> {
    let project = fields(value)?;
    let roots = property(project, "roots").as_array();
    let position = property(project, "position");
    let recency = property(project, "recencyAt");
    require(
        property(project, "id").as_js_string().is_some()
            && property(project, "name").as_js_string().is_some()
            && roots.is_some(),
        "native_inventory_invalid",
        "Native project identity, name and roots are required.",
    )?;
    require(
        safe_nonnegative(position) && (recency.is_null() || safe_nonnegative(recency)),
        "native_inventory_invalid",
        "Native project position and recency must be bounded integers.",
    )?;
    let paths = roots
        .unwrap()
        .iter()
        .map(|root| fields(root).map(|root| property(root, "path").clone()))
        .collect::<IvyResult<Vec<_>>>()?;
    require(
        paths.iter().all(|v| v.as_js_string().is_some()),
        "native_inventory_invalid",
        "Native project roots must contain paths.",
    )?;
    let summary = IvyJson::object([
        ("nativeId", property(project, "id").clone()),
        ("source", "native".into()),
        ("name", property(project, "name").clone()),
        ("paths", IvyJson::Array(paths)),
        ("position", position.clone()),
        ("recencyAt", recency.clone()),
    ]);
    Ok(summary)
}

async fn publish_kind<F>(
    connection: &dyn RpcClient,
    journal: &Mutex<NativeJournal>,
    kind: &str,
    observed_at: &IvyJson,
    entries: F,
    signal: Option<&InventorySignal>,
) -> IvyResult<u64>
where
    F: Fn() -> IvyResult<IvyJson> + Send,
{
    let namespace = journal
        .lock()
        .map_err(|_| IvyError::internal())?
        .inventory_namespace()
        .to_owned();
    let mut minimum = 0.0;
    for attempt in 0..2 {
        if let Some(signal) = signal {
            signal.check()?;
        }
        let revision = journal
            .lock()
            .map_err(|_| IvyError::internal())?
            .inventory_revision(kind, minimum);
        let params = IvyJson::object([
            ("namespace", namespace.as_str().into()),
            ("kind", kind.into()),
            (
                "schemaVersion",
                if kind == "project" {
                    NATIVE_PROJECT_SCHEMA_VERSION
                } else {
                    NATIVE_INVENTORY_SCHEMA_VERSION
                }
                .into(),
            ),
            ("mode", "snapshot".into()),
            ("snapshotRevision", IvyJson::Number(revision)),
            ("observedAt", observed_at.clone()),
            ("entries", entries()?),
        ]);
        match connection
            .request(
                "inventory.sync",
                params,
                signal.map(|s| s.options.clone()).unwrap_or_default(),
            )
            .await
        {
            Ok(_) => return Ok(revision as u64),
            Err(error)
                if error.code == "revision_conflict"
                    && error.outcome == Outcome::NotExecuted
                    && attempt == 0 =>
            {
                let details = error.details.as_ref().unwrap_or(&NULL);
                validate_shared("InventoryRevisionConflictDetails", details)?;
                let owner = journal
                    .lock()
                    .map_err(|_| IvyError::internal())?
                    .owner()
                    .service_node_id
                    .clone();
                require(
                    details.get("serviceNodeId") == Some(&IvyJson::String(owner))
                        && details.get("namespace") == Some(&namespace.as_str().into())
                        && details.get("kind") == Some(&kind.into()),
                    "target_conflict",
                    "Inventory conflict metadata belongs to a different owner or kind.",
                )?;
                minimum = property(details, "currentRevision").as_f64().unwrap();
            }
            Err(error) => return Err(error),
        }
    }
    Err(IvyError::internal())
}
pub async fn publish_native_threads(
    connection: &dyn RpcClient,
    journal: &Mutex<NativeJournal>,
    snapshot: &IvyJson,
    signal: Option<&InventorySignal>,
) -> IvyResult<u64> {
    publish_kind(
        connection,
        journal,
        "thread",
        snapshot.required("observedAt")?,
        || Ok(snapshot.required("threads")?.clone()),
        signal,
    )
    .await
}
pub async fn publish_native_projects(
    connection: &dyn RpcClient,
    journal: &Mutex<NativeJournal>,
    snapshot: &ProjectSnapshot,
    signal: Option<&InventorySignal>,
) -> IvyResult<()> {
    publish_kind(
        connection,
        journal,
        "project",
        &IvyJson::String(snapshot.observed_at.clone()),
        || {
            Ok(IvyJson::Array(
                snapshot
                    .projects
                    .entries()
                    .iter()
                    .map(|project| {
                        IvyJson::object([
                            ("nativeId", IvyJson::String(project.native_id.clone())),
                            ("summary", project.to_json()),
                            ("observedAt", IvyJson::String(snapshot.observed_at.clone())),
                        ])
                    })
                    .collect(),
            ))
        },
        signal,
    )
    .await
    .map(|_| ())
}

pub(crate) async fn publish_thread_delta(
    connection: &dyn RpcClient,
    journal: &Mutex<NativeJournal>,
    delta: &ThreadDelta,
    expected_revision: u64,
    signal: &InventorySignal,
) -> IvyResult<u64> {
    signal.check()?;
    let (namespace, owner, revision) = {
        let mut journal = journal.lock().map_err(|_| IvyError::internal())?;
        (
            journal.inventory_namespace().to_owned(),
            journal.owner().service_node_id.clone(),
            journal.inventory_revision("thread", expected_revision as f64),
        )
    };
    let params = IvyJson::object([
        ("namespace", namespace.as_str().into()),
        ("kind", "thread".into()),
        ("schemaVersion", NATIVE_INVENTORY_SCHEMA_VERSION.into()),
        ("mode", "delta".into()),
        ("snapshotRevision", IvyJson::Number(revision)),
        ("observedAt", delta.snapshot.required("observedAt")?.clone()),
        (
            "expectedRevision",
            IvyJson::Number(expected_revision as f64),
        ),
        ("entries", delta.snapshot.required("threads")?.clone()),
        (
            "removedNativeIds",
            IvyJson::Array(delta.removed.iter().cloned().map(IvyJson::String).collect()),
        ),
    ]);
    match connection
        .request("inventory.sync", params, signal.options.clone())
        .await
    {
        Ok(_) => Ok(revision as u64),
        Err(error)
            if error.code == "revision_conflict" && error.outcome == Outcome::NotExecuted =>
        {
            let details = error.details.as_ref().unwrap_or(&NULL);
            validate_shared("InventoryRevisionConflictDetails", details)?;
            require(
                details.get("serviceNodeId") == Some(&IvyJson::String(owner))
                    && details.get("namespace") == Some(&namespace.as_str().into())
                    && details.get("kind") == Some(&"thread".into()),
                "target_conflict",
                "Inventory delta conflict belongs to a different owner or kind.",
            )?;
            // A gap requires a newly observed complete snapshot, never a rebased delta.
            journal
                .lock()
                .map_err(|_| IvyError::internal())?
                .inventory_revision(
                    "thread",
                    details.required("currentRevision")?.as_f64().unwrap(),
                );
            Err(error)
        }
        Err(error) => Err(error),
    }
}
