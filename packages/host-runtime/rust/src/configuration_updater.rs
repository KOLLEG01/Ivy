//! Durable Hive HostConfig adoption through the existing format-1 journal.
use crate::{
    IvyError, IvyJson, IvyResult,
    accepted_configuration::same_installation,
    config, host_config,
    journal::{self, HostJournal},
};
use ivy_contracts::{JsString, hash::hash_json, json};
use ivy_sdk::rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
pub struct SyncResult {
    pub changed: bool,
    pub revision: IvyJson,
    pub content_hash: IvyJson,
    pub bootstrap_required: Vec<IvyJson>,
}
impl SyncResult {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("changed", self.changed.into()),
            ("revision", self.revision.clone()),
            ("contentHash", self.content_hash.clone()),
            (
                "bootstrapRequired",
                IvyJson::Array(self.bootstrap_required.clone()),
            ),
        ])
    }
}
struct Inner {
    config_path: PathBuf,
    journal: HostJournal,
    client: Arc<dyn RpcClient>,
    sync: Mutex<()>,
}
#[derive(Clone)]
pub struct ConfigurationUpdater(Arc<Inner>);
impl ConfigurationUpdater {
    pub async fn new(
        config_path: PathBuf,
        journal: HostJournal,
        credential: String,
    ) -> IvyResult<Self> {
        let config = journal.config().await?;
        let client = HiveClient::new(
            &config.required("publicBaseUrl")?.as_str()?,
            ClientOptions {
                credential: Some(credential),
                ..Default::default()
            },
        )?;
        Ok(Self::with_client(config_path, journal, Arc::new(client)))
    }
    /// Replaces only the SDK transport. Admission, persistence and action replay
    /// still use the actual HostConfig checker and HostJournal.
    pub fn with_client(
        config_path: PathBuf,
        journal: HostJournal,
        client: Arc<dyn RpcClient>,
    ) -> Self {
        Self(Arc::new(Inner {
            config_path,
            journal,
            client,
            sync: Mutex::new(()),
        }))
    }
    /// The admitted adoption remains owned if its observer is cancelled. Its
    /// durable staged state also resumes after process interruption or failure.
    pub async fn sync(&self) -> IvyResult<SyncResult> {
        let owner = self.0.clone();
        tokio::spawn(async move {
            let _guard = owner.sync.lock().await;
            owner.sync().await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    static NULL: IvyJson = IvyJson::Null;
    value.get(key).unwrap_or(&NULL)
}
fn instances(config: &IvyJson) -> IvyResult<&[IvyJson]> {
    config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)
}
fn input(config: &IvyJson, instance: &IvyJson) -> IvyResult<String> {
    let mut instance = instance.clone();
    instance.remove("enabled");
    json::canonical(
        &IvyJson::object([
            ("publicBaseUrl", config.required("publicBaseUrl")?.clone()),
            ("instance", instance),
        ]),
        usize::MAX,
    )
}
impl Inner {
    async fn apply(&self, value: IvyJson) -> IvyResult<SyncResult> {
        if IvyJson::from(hash_json(&self.journal.config().await?)?)
            != *value.required("contentHash")?
        {
            config::atomic_json(
                self.config_path.clone(),
                value.required("configuration")?.clone(),
            )
            .await?;
            self.journal
                .use_configuration(value.required("configuration")?.clone())
                .await?;
        }
        for action in value
            .required("actions")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            self.journal.accept(action.clone(), None).await?;
        }
        self.journal
            .finish_configuration_adoption(value.clone())
            .await?;
        Ok(SyncResult {
            changed: true,
            revision: value.required("revision")?.clone(),
            content_hash: value.required("contentHash")?.clone(),
            bootstrap_required: value
                .required("bootstrapRequired")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .to_vec(),
        })
    }
    async fn sync(&self) -> IvyResult<SyncResult> {
        if let Some(retained) = self.journal.configuration_adoption().await? {
            return self.apply(retained).await;
        }
        let previous = self.journal.config().await?;
        let remote = self
            .client
            .request(
                "hostConfigurations.get",
                IvyJson::object([("hostId", previous.required("hostId")?.clone())]),
                RequestOptions {
                    timeout: Duration::from_secs(10),
                    ..Default::default()
                },
            )
            .await?;
        let previous = self.journal.config().await?;
        require(
            remote.get("hostId") == previous.get("hostId"),
            "target_conflict",
            "Hive returned configuration for another host.",
        )?;
        let remote_config = remote
            .get("configuration")
            .filter(|v| !v.is_null())
            .ok_or_else(IvyError::internal)?;
        require(
            remote_config.get("hostId") == previous.get("hostId"),
            "target_conflict",
            "Hive returned configuration for another host.",
        )?;
        require(
            IvyJson::from(hash_json(remote_config)?) == *field(&remote, "contentHash"),
            "configuration_changed",
            "Hive configuration hash does not match its exact content.",
        )?;
        let next =
            host_config::checked_host_config(remote_config.clone(), self.config_path.clone())
                .await?;
        same_installation(&previous, &next)?;
        let content_hash = IvyJson::from(hash_json(&next)?);
        if hash_json(&previous)? == hash_json(&next)? {
            return Ok(SyncResult {
                changed: false,
                revision: field(&remote, "revision").clone(),
                content_hash,
                bootstrap_required: vec![],
            });
        }
        require(
            self.journal.unfinished().await?.is_empty(),
            "service_busy",
            "Host configuration waits for current deployments to finish.",
        )?;
        for before in instances(&previous)? {
            let id = before.required("instanceId")?.as_str()?;
            if let Some(installed) = self.journal.installed(&id).await? {
                require(
                    instances(&next)?
                        .iter()
                        .any(|v| v.get("instanceId") == before.get("instanceId"))
                        || installed.get("enabled") == Some(&false.into()),
                    "restart_required",
                    "Installed instances must be disabled before removal from desired configuration.",
                )?;
            }
        }
        let mut actions = Vec::new();
        let mut bootstrap_required = Vec::new();
        for instance in instances(&next)? {
            let Some(before) = instances(&previous)?
                .iter()
                .find(|v| v.get("instanceId") == instance.get("instanceId"))
            else {
                continue;
            };
            let Some(installed) = self
                .journal
                .installed(&instance.required("instanceId")?.as_str()?)
                .await?
            else {
                continue;
            };
            let enabled_changed = instance.get("enabled") != installed.get("enabled");
            let input_changed = input(&previous, before)? != input(&next, instance)?;
            if !enabled_changed && !input_changed {
                continue;
            }
            if journal::requires_bootstrap_replacement(instance) {
                require(
                    !enabled_changed,
                    "restart_required",
                    "Changing a bootstrap owner enabled state requires explicit OS maintenance.",
                )?;
                bootstrap_required.push(instance.required("instanceId")?.clone());
                continue;
            }
            let action = if enabled_changed {
                if instance.get("enabled") == Some(&true.into()) {
                    "enable"
                } else {
                    "disable"
                }
            } else {
                "restart"
            };
            let mut request = IvyJson::object([
                ("instanceId", instance.required("instanceId")?.clone()),
                ("action", action.into()),
            ]);
            let mut identity = IvyJson::object([
                ("hostId", next.required("hostId")?.clone()),
                ("revision", field(&remote, "revision").clone()),
            ]);
            for (key, value) in request.as_object().ok_or_else(IvyError::internal)? {
                identity.insert(key.clone(), value.clone());
            }
            request.insert(
                "operationId",
                format!("configuration-{}", &hash_json(&identity)?[7..47]).into(),
            );
            actions.push(request);
        }
        let adoption = IvyJson::object([
            ("configuration", next),
            ("revision", field(&remote, "revision").clone()),
            ("contentHash", content_hash),
            ("bootstrapRequired", IvyJson::Array(bootstrap_required)),
            ("actions", IvyJson::Array(actions)),
        ]);
        self.journal
            .stage_configuration_adoption(adoption.clone())
            .await?;
        self.apply(adoption).await
    }
}

/// The explicit launching instance selects its own credential. An absent match
/// never borrows a different configured executor's identity. Empty explicit
/// credentials remain explicit, as with the original nullish-coalescing choice.
pub fn update_credential(
    config: &IvyJson,
    own_instance_id: Option<&str>,
    explicit: Option<&JsString>,
) -> IvyResult<Option<JsString>> {
    if let Some(value) = explicit {
        return Ok(Some(value.clone()));
    }
    let own = own_instance_id.filter(|id| !id.is_empty());
    let instance = instances(config)?.iter().find(|value| match own {
        Some(id) => value.get("instanceId") == Some(&id.into()),
        None => value.get("componentId") == Some(&"host-executor".into()),
    });
    Ok(instance
        .and_then(|v| v.get("credential"))
        .and_then(IvyJson::as_js_string)
        .cloned())
}
