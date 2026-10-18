//! Complete bounded native snapshots and guarded Hive publication.
use crate::{
    native_journal::{JournalClock, NativeJournal},
    registry::{NATIVE_INVENTORY_SCHEMA_VERSION, NATIVE_PROJECT_SCHEMA_VERSION},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, agent_schema::validate_agent, json::canonical,
    wire_schema::validate_shared,
};
use ivy_host_runtime::codex_rpc::{NativeRpc, RequestOptions as NativeOptions};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    service::{InvocationContext, ServiceConnection, ServiceFuture},
};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

const SNAPSHOT_BYTES: usize = 9 * 1024 * 1024;
static NULL: IvyJson = IvyJson::Null;
pub type InventoryRead =
    dyn Fn(JsString, IvyJson, NativeOptions) -> ServiceFuture<IvyJson> + Send + Sync;
pub type InventoryClock = dyn Fn() -> f64 + Send + Sync;

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
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null | IvyJson::Bool(false) => false,
        IvyJson::Number(n) => *n != 0.0 && !n.is_nan(),
        IvyJson::String(s) => !s.0.is_empty(),
        _ => true,
    }
}
fn resolve<'a>(root: &'a IvyJson, value: &'a IvyJson) -> IvyResult<&'a IvyJson> {
    let node = fields(value)?;
    let reference = property(node, "$ref");
    if !truthy(reference) {
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
pub fn native_source_kinds(catalog: &IvyJson) -> IvyResult<Vec<IvyJson>> {
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
    Ok(kinds.unwrap().to_vec())
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
    catalog: IvyJson,
    epoch: JsString,
    source_kinds: Vec<IvyJson>,
    read: Arc<InventoryRead>,
    projects: Mutex<Vec<IvyJson>>,
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
    pub fn new(catalog: IvyJson, epoch: JsString, rpc: NativeRpc) -> IvyResult<Self> {
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
        catalog: IvyJson,
        epoch: JsString,
        read: Arc<InventoryRead>,
        now: Arc<InventoryClock>,
        observed_at: Arc<JournalClock>,
    ) -> IvyResult<Self> {
        let source_kinds = native_source_kinds(&catalog)?;
        Ok(Self {
            catalog,
            epoch,
            source_kinds,
            read,
            projects: Mutex::new(vec![]),
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
        match self.thread_snapshot(&reader).await {
            Err(error) if error.code == "native_inventory_changed" => {
                reader.check()?;
                self.thread_snapshot(&reader).await
            }
            result => result,
        }
    }
    async fn thread_snapshot(&self, reader: &Reader<'_>) -> IvyResult<IvyJson> {
        let before = reader.loaded().await?;
        let owned: HashSet<_> = before.iter().cloned().collect();
        let mut threads = Vec::new();
        let mut ids = HashSet::new();
        let mut bytes = 0;
        for archived in [false, true] {
            reader
                .pages(
                    "thread/list",
                    IvyJson::object([
                        ("archived", archived.into()),
                        ("sourceKinds", IvyJson::Array(self.source_kinds.clone())),
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
                        )
                    },
                )
                .await?;
        }
        for id in &before {
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
                )?;
            }
        }
        let after: HashSet<_> = reader.loaded().await?.into_iter().collect();
        require(
            owned == after,
            "native_inventory_changed",
            "Loaded native ownership changed during enumeration; retain the previous snapshot.",
        )?;
        let observed_at = IvyJson::String((self.observed_at)());
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
        canonical(&result, SNAPSHOT_BYTES)?;
        reader.check()?;
        Ok(result)
    }
    pub fn current_projects(&self, observed_at: Option<JsString>) -> IvyResult<IvyJson> {
        let observed_at = IvyJson::String(observed_at.unwrap_or_else(|| (self.observed_at)()));
        let projects = self
            .projects
            .lock()
            .map_err(|_| IvyError::internal())?
            .clone();
        let mut ids = HashSet::new();
        require(
            projects
                .iter()
                .all(|p| ids.insert(property(p, "nativeId").as_js_string().unwrap().clone())),
            "native_inventory_invalid",
            "Native project inventory contains duplicate IDs.",
        )?;
        let result = IvyJson::object([
            ("observedAt", observed_at.clone()),
            (
                "projects",
                IvyJson::object([
                    ("source", "native".into()),
                    ("observedAt", observed_at),
                    ("projects", IvyJson::Array(projects)),
                ]),
            ),
        ]);
        validate_agent("ProjectsResult", property(&result, "projects"))?;
        canonical(&result, SNAPSHOT_BYTES)?;
        Ok(result)
    }
    pub async fn collect_projects(&self, signal: Option<&InventorySignal>) -> IvyResult<IvyJson> {
        require(
            self.catalog
                .get("clientRequests")
                .and_then(IvyJson::as_array)
                .is_some_and(|r| {
                    r.iter()
                        .any(|v| v.get("method") == Some(&"project/list".into()))
                }),
            "native_inventory_unavailable",
            "The connected Codex instance does not support project/list.",
        )?;
        let reader = self.reader(signal, 600_000);
        let projects = reader
            .pages(
                "project/list",
                IvyJson::object([
                    ("sortKey", "position".into()),
                    ("sortDirection", "asc".into()),
                ]),
                256,
                project_summary,
            )
            .await?;
        *self.projects.lock().map_err(|_| IvyError::internal())? = projects;
        reader.check()?;
        self.current_projects(None)
    }
    pub async fn collect(&self, signal: Option<&InventorySignal>) -> IvyResult<IvyJson> {
        let threads = self.collect_threads(signal).await?;
        let projects = self.collect_projects(signal).await?;
        let thread_at = threads.required("observedAt")?;
        let project_at = projects.required("observedAt")?;
        let observed_at = if thread_at.as_js_string().unwrap() > project_at.as_js_string().unwrap()
        {
            thread_at
        } else {
            project_at
        };
        Ok(IvyJson::object([
            ("observedAt", observed_at.clone()),
            ("threads", threads.required("threads")?.clone()),
            ("projects", projects.required("projects")?.clone()),
        ]))
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
        let reply = (self.owner.read)(
            method.into(),
            params,
            NativeOptions {
                timeout_ms: self.budget,
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
        Ok(fields(property(&reply, "result"))?.clone())
    }
    async fn pages<F>(
        &self,
        method: &str,
        params: IvyJson,
        maximum: usize,
        mut summarize: F,
    ) -> IvyResult<Vec<IvyJson>>
    where
        F: FnMut(&IvyJson) -> IvyResult<IvyJson> + Send,
    {
        let mut result = Vec::new();
        let mut cursors = HashSet::new();
        let mut cursor: Option<JsString> = None;
        let mut bytes = 0;
        loop {
            let mut params = params.clone();
            params.insert("limit", IvyJson::Number(100.0));
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
                result.len() + data.len() <= maximum,
                "native_inventory_capacity",
                "Complete native inventory exceeds its declared capacity.",
            )?;
            for value in data {
                let item = summarize(value)?;
                bytes += canonical(&item, SNAPSHOT_BYTES)?.len();
                require(
                    bytes <= SNAPSHOT_BYTES,
                    "native_inventory_capacity",
                    "Accumulated native inventory exceeds its byte capacity.",
                )?;
                result.push(item);
            }
            cursor = next.unwrap().as_js_string().cloned();
            let Some(cursor) = &cursor else {
                break;
            };
            require(
                !cursors.contains(cursor) && cursors.len() < maximum + 1,
                "native_inventory_invalid",
                "Native inventory cursor did not advance within its bound.",
            )?;
            cursors.insert(cursor.clone());
        }
        Ok(result)
    }
    async fn loaded(&self) -> IvyResult<Vec<JsString>> {
        let values = self
            .pages(
                "thread/loaded/list",
                IvyJson::object::<&str>([]),
                5000,
                |v| Ok(v.clone()),
            )
            .await?;
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for value in values {
            let id = value.into_js_string();
            require(
                id.as_ref().is_some_and(|id| seen.insert(id.clone())),
                "native_inventory_invalid",
                "Loaded native thread IDs must be unique strings.",
            )?;
            ids.push(id.unwrap());
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
fn add_thread(
    value: &IvyJson,
    archived: bool,
    owned: &HashSet<JsString>,
    epoch: &JsString,
    threads: &mut Vec<IvyJson>,
    ids: &mut HashSet<JsString>,
    bytes: &mut usize,
) -> IvyResult<IvyJson> {
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
    *bytes += canonical(&summary, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES)?.len();
    require(
        *bytes <= SNAPSHOT_BYTES,
        "native_inventory_capacity",
        "Complete native thread summaries exceed their byte capacity.",
    )?;
    threads.push(summary.clone());
    ids.insert(id.unwrap().clone());
    require(
        threads.len() <= 5000,
        "native_inventory_capacity",
        "Complete native thread inventory exceeds its declared capacity.",
    )?;
    Ok(summary)
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
    validate_agent("ProjectSummary", &summary)?;
    Ok(summary)
}

async fn publish_kind(
    connection: &dyn RpcClient,
    journal: &Mutex<NativeJournal>,
    kind: &str,
    entries: IvyJson,
    signal: Option<&InventorySignal>,
) -> IvyResult<()> {
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
            ("namespace", "codex".into()),
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
            ("entries", entries.clone()),
        ]);
        match connection
            .request(
                "inventory.sync",
                params,
                signal.map(|s| s.options.clone()).unwrap_or_default(),
            )
            .await
        {
            Ok(_) => return Ok(()),
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
                        && details.get("namespace") == Some(&"codex".into())
                        && details.get("kind") == Some(&kind.into()),
                    "target_conflict",
                    "Inventory conflict metadata belongs to a different owner or kind.",
                )?;
                minimum = property(details, "currentRevision").as_f64().unwrap();
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
pub async fn publish_native_threads(
    connection: &dyn RpcClient,
    journal: &Mutex<NativeJournal>,
    snapshot: &IvyJson,
    signal: Option<&InventorySignal>,
) -> IvyResult<()> {
    publish_kind(
        connection,
        journal,
        "thread",
        snapshot.required("threads")?.clone(),
        signal,
    )
    .await
}
pub async fn publish_native_projects(
    connection: &dyn RpcClient,
    journal: &Mutex<NativeJournal>,
    snapshot: &IvyJson,
    signal: Option<&InventorySignal>,
) -> IvyResult<()> {
    let projects = snapshot
        .required("projects")?
        .required("projects")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let entries = projects
        .iter()
        .map(|summary| {
            Ok(IvyJson::object([
                ("nativeId", summary.required("nativeId")?.clone()),
                ("summary", summary.clone()),
                ("observedAt", snapshot.required("observedAt")?.clone()),
            ]))
        })
        .collect::<IvyResult<Vec<_>>>()?;
    publish_kind(
        connection,
        journal,
        "project",
        IvyJson::Array(entries),
        signal,
    )
    .await
}
pub async fn publish_native_snapshot(
    connection: &dyn RpcClient,
    journal: &Mutex<NativeJournal>,
    snapshot: &IvyJson,
    signal: Option<&InventorySignal>,
) -> IvyResult<()> {
    publish_native_threads(connection, journal, snapshot, signal).await?;
    publish_native_projects(
        connection,
        journal,
        &IvyJson::object([
            ("observedAt", snapshot.required("observedAt")?.clone()),
            ("projects", snapshot.required("projects")?.clone()),
        ]),
        signal,
    )
    .await
}
