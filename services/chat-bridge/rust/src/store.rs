//! ChatBridge persistence. Admission and domain transitions belong to the caller.
pub mod hive;
pub mod local;

use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, hash::hash_json};
use ivy_sdk::{
    clock::{Clock, SystemClock, date_to_iso, parse_date},
    rpc::{RequestOptions, RpcClient},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Debug)]
pub enum StoreError {
    Domain(IvyError),
    Sqlite(rusqlite::Error),
}
impl From<IvyError> for StoreError {
    fn from(value: IvyError) -> Self {
        Self::Domain(value)
    }
}
impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(e) => e.fmt(f),
            Self::Sqlite(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for StoreError {}
impl StoreError {
    /// Match IvyError.from at the original RPC boundary; keep the SQLite cause
    /// available internally instead of turning raw database failures into a new code.
    pub fn into_ivy(self) -> IvyError {
        match self {
            Self::Domain(e) => e,
            Self::Sqlite(_) => IvyError::internal(),
        }
    }
}
pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub object_id: String,
    pub revision: i64,
}
impl Pin {
    pub fn json(&self) -> Json {
        Json::object([
            ("objectId", self.object_id.clone().into()),
            ("revision", Json::Number(self.revision as f64)),
        ])
    }
}

#[derive(Debug)]
pub struct Document {
    pub key: String,
    pub pin: Pin,
    pub value: Json,
    pub metadata: Json,
    pub revision: Json,
}

pub enum Destination<'a> {
    Create {
        parent_id: Option<&'a str>,
        name: &'a str,
    },
    Update {
        object_id: &'a str,
        expected_revision: i64,
    },
}

#[derive(Clone, Copy)]
pub enum Reference<'a> {
    Current(&'a str),
    Pinned(&'a Pin),
}
impl Reference<'_> {
    pub fn json(self) -> Json {
        match self {
            Self::Current(id) => Json::object([("objectId", id.into())]),
            Self::Pinned(pin) => pin.json(),
        }
    }
}

pub fn mutation(scope: &str, step: &str) -> IvyResult<String> {
    Ok(format!(
        "chat:{}",
        &hash_json(&Json::object([
            ("scope", scope.into()),
            ("step", step.into())
        ]))?[7..]
    ))
}

/// Public Hive records and local technical records share one persistence facade.
/// Each SQLite operation finishes on the blocking pool before native dispatch;
/// no database guard is held across a Hive request.
pub struct ChatStore {
    pub client: Arc<dyn RpcClient>,
    pub root_object_id: Option<String>,
    hive: hive::HiveStore,
    local: Arc<Mutex<local::LocalStore>>,
    clock: Arc<dyn Clock>,
    change_version: Arc<AtomicU64>,
}
impl ChatStore {
    pub async fn open(
        client: Arc<dyn RpcClient>,
        root_object_id: Option<String>,
        data_root: Option<PathBuf>,
    ) -> StoreResult<Self> {
        Self::with_clock(client, root_object_id, data_root, Arc::new(SystemClock)).await
    }
    pub async fn with_clock(
        client: Arc<dyn RpcClient>,
        root_object_id: Option<String>,
        data_root: Option<PathBuf>,
        clock: Arc<dyn Clock>,
    ) -> StoreResult<Self> {
        let local =
            tokio::task::spawn_blocking(move || local::LocalStore::open(data_root.as_deref()))
                .await
                .map_err(|_| IvyError::internal())??;
        Ok(Self {
            hive: hive::HiveStore::new(client.clone()),
            client,
            root_object_id,
            local: Arc::new(Mutex::new(local)),
            clock,
            change_version: Arc::new(AtomicU64::new(0)),
        })
    }
    async fn local<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut local::LocalStore) -> StoreResult<T> + Send + 'static,
    ) -> StoreResult<T> {
        let local = self.local.clone();
        tokio::task::spawn_blocking(move || {
            let mut local = local.lock().map_err(|_| IvyError::internal())?;
            work(&mut local)
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    pub fn change_version(&self) -> u64 {
        self.change_version.load(Ordering::Relaxed)
    }
    pub(crate) fn now_iso(&self) -> IvyResult<String> {
        date_to_iso(self.now_millis() as f64)
            .map_err(|_| IvyError::internal())?
            .to_utf8()
    }
    pub(crate) fn now_millis(&self) -> i64 {
        self.clock.utc_millis()
    }
    pub async fn set_limits(&self, limits: local::Limits) -> StoreResult<()> {
        self.local(move |local| {
            local.limits = limits;
            Ok(())
        })
        .await
    }
    pub async fn set_diagnostic_history_limits(&self, limits: local::Limits) -> StoreResult<()> {
        self.local(move |local| {
            local.diagnostic_history_limits = limits;
            Ok(())
        })
        .await
    }
    pub async fn set_runtime_epoch(&self, value: ivy_contracts::JsString) -> StoreResult<()> {
        let clock = self.clock.clone();
        self.local(move |local| local.set_runtime_epoch(value, clock.utc_millis()))
            .await
    }
    pub async fn validate_operation_id(
        &self,
        value: &ivy_contracts::JsString,
        now: Option<i64>,
        retained: bool,
    ) -> StoreResult<()> {
        let now = now.unwrap_or_else(|| self.clock.utc_millis());
        if !self
            .local(|local| Ok(local.runtime_epoch().is_some()))
            .await?
        {
            let status = self
                .client
                .request(
                    "system.status",
                    Json::Object(vec![]),
                    RequestOptions::default(),
                )
                .await?;
            let epoch = status
                .required("runtimeEpoch")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?
                .clone();
            self.set_runtime_epoch(epoch).await?;
        }
        let value = value.clone();
        self.local(move |local| local.validate_operation_id(&value, now, retained))
            .await
    }
    pub async fn read(&self, key: &str, reference: Reference<'_>) -> StoreResult<Document> {
        self.read_in(key, reference, self.root_object_id.as_deref())
            .await
    }
    pub async fn read_in(
        &self,
        key: &str,
        reference: Reference<'_>,
        parent: Option<&str>,
    ) -> StoreResult<Document> {
        if !crate::schema::local_contract(key) {
            return Ok(self.hive.read(key, reference, parent).await?);
        }
        let (id, revision) = match reference {
            Reference::Current(id) => (id.to_owned(), None),
            Reference::Pinned(pin) => (pin.object_id.clone(), Some(pin.revision)),
        };
        let key = key.to_owned();
        let parent = parent.map(str::to_owned);
        self.local(move |local| local.document(&key, &id, revision, parent.as_deref()))
            .await
    }
    pub async fn named(&self, key: &str, name: &str) -> StoreResult<Option<Document>> {
        self.named_in(key, name, self.root_object_id.as_deref())
            .await
    }
    pub async fn named_in(
        &self,
        key: &str,
        name: &str,
        parent: Option<&str>,
    ) -> StoreResult<Option<Document>> {
        if !crate::schema::local_contract(key) {
            return Ok(self.hive.named(key, name, parent).await?);
        }
        let key = key.to_owned();
        let name = name.to_owned();
        let parent = parent.map(str::to_owned);
        self.local(move |local| local.named(&key, &name, parent.as_deref()))
            .await
    }
    pub async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation_id: &str,
        destination: Destination<'_>,
    ) -> StoreResult<Pin> {
        if crate::schema::local_contract(key) {
            let key = key.to_owned();
            let value = value.clone();
            let clock = self.clock.clone();
            let change_version = self.change_version.clone();
            // Own borrowed destination fields before entering the database task.
            let destination = match destination {
                Destination::Create { parent_id, name } => {
                    (true, parent_id.map(str::to_owned), name.to_owned(), 0)
                }
                Destination::Update {
                    object_id,
                    expected_revision,
                } => (false, None, object_id.to_owned(), expected_revision),
            };
            let pin = self
                .local(move |local| {
                    let before = local.change_version;
                    let now = clock.utc_millis();
                    // Date#toISOString throws an ordinary RangeError here;
                    // preserve IvyError.from at this service boundary.
                    let iso = date_to_iso(now as f64)
                        .map_err(|_| IvyError::internal())?
                        .to_utf8()?;
                    let destination = if destination.0 {
                        Destination::Create {
                            parent_id: destination.1.as_deref(),
                            name: &destination.2,
                        }
                    } else {
                        Destination::Update {
                            object_id: &destination.2,
                            expected_revision: destination.3,
                        }
                    };
                    let pin = local.write(&key, &value, destination, now, &iso)?;
                    // The blocking write survives cancellation of its waiter.
                    // Publish the committed change in that same owner task.
                    change_version.fetch_add(local.change_version - before, Ordering::Relaxed);
                    Ok(pin)
                })
                .await?;
            return Ok(pin);
        }
        let receipt = self
            .hive
            .write(key, value, mutation_id, destination)
            .await?;
        if receipt.replayed {
            return Ok(receipt.pin);
        }
        if key == "chat-bridge/input"
            && ["completed", "failed", "cancelled"]
                .iter()
                .any(|state| value.get("state") == Some(&Json::from(*state)))
        {
            if let Some(finished_at) = value
                .get("finishedAt")
                .and_then(Json::as_js_string)
                .filter(|s| !s.0.is_empty())
            {
                let finished_at = parse_date(finished_at);
                let id = receipt.pin.object_id.clone();
                let clock = self.clock.clone();
                let change_version = self.change_version.clone();
                self.local(move |local| {
                    local.completed_input(&id, finished_at, clock.utc_millis())?;
                    change_version.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                })
                .await?;
                return Ok(receipt.pin);
            }
        }
        self.change_version.fetch_add(1, Ordering::Relaxed);
        Ok(receipt.pin)
    }
    pub async fn require_available(&self, pin: &Pin) -> StoreResult<()> {
        let reference = pin.clone();
        let checked = self
            .local(move |local| {
                if let Some((key, parent)) = local.local_info(&reference.object_id)? {
                    if crate::schema::local_contract(&key) {
                        local.document(
                            &key,
                            &reference.object_id,
                            Some(reference.revision),
                            parent.as_deref(),
                        )?;
                        return Ok(true);
                    }
                }
                Ok(false)
            })
            .await?;
        if !checked {
            self.hive.require_available(pin).await?;
        }
        Ok(())
    }
    pub async fn local_blob(&self, key: &str, pin: &Pin, parent: &str) -> StoreResult<Vec<u8>> {
        let key = key.to_owned();
        let pin = pin.clone();
        let parent = parent.to_owned();
        self.local(move |local| local.blob(&key, &pin, &parent))
            .await
    }
    pub async fn local_info(&self, id: &str) -> StoreResult<Option<(String, Option<String>)>> {
        let id = id.to_owned();
        self.local(move |local| local.local_info(&id)).await
    }
    pub async fn save_local_blob(
        &self,
        key: &str,
        name: &str,
        parent: &str,
        bytes: &[u8],
    ) -> StoreResult<Pin> {
        let key = key.to_owned();
        let name = name.to_owned();
        let parent = parent.to_owned();
        let bytes = bytes.to_vec();
        let clock = self.clock.clone();
        self.local(move |local| local.save_blob(&key, &name, &parent, &bytes, clock.utc_millis()))
            .await
    }
    pub async fn confirm_offer(&self, id: &str, replies: &[String]) -> StoreResult<()> {
        let id = id.to_owned();
        let replies = replies.to_vec();
        self.local(move |local| local.confirm_offer(&id, &replies))
            .await
    }
    pub async fn diagnostic_notice_state(&self, scope: &str) -> StoreResult<Json> {
        let scope = scope.to_owned();
        self.local(move |local| local.diagnostic_notice_state(&scope))
            .await
    }
    pub async fn save_diagnostic_notice_state(
        &self,
        scope: &str,
        value: &mut Json,
    ) -> StoreResult<()> {
        let scope = scope.to_owned();
        let mut input = value.clone();
        // Return the caller-visible eviction even if the SQLite marker write fails.
        let (result, output) = self
            .local(move |local| {
                let result = local.save_diagnostic_notice_state(&scope, &mut input);
                Ok((result, input))
            })
            .await?;
        *value = output;
        result
    }
    pub async fn remember_notice_input(&self, id: &str) -> StoreResult<()> {
        let id = id.to_owned();
        let clock = self.clock.clone();
        self.local(move |local| local.remember_notice_input(&id, clock.utc_millis()))
            .await
    }
    pub async fn is_notice_input(&self, id: &str) -> StoreResult<bool> {
        let id = id.to_owned();
        let clock = self.clock.clone();
        self.local(move |local| local.is_notice_input(&id, clock.utc_millis()))
            .await
    }
    pub async fn remember_notice_delivery(&self, id: &str) -> StoreResult<()> {
        let id = id.to_owned();
        let clock = self.clock.clone();
        self.local(move |local| local.remember_notice_delivery(&id, clock.utc_millis()))
            .await
    }
    pub async fn close(self) -> StoreResult<()> {
        let local = self.local;
        tokio::task::spawn_blocking(move || {
            let local = Arc::try_unwrap(local).map_err(|_| IvyError::internal())?;
            local
                .into_inner()
                .map_err(|_| IvyError::internal())?
                .close()
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}
