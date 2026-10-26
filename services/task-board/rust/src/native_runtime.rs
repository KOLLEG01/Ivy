//! Native process composition. The executable supplies its matching contract,
//! registry, native catalog, locale, prompt and Chat-reader owners.
use crate::{
    engine_factory::EngineFactory,
    host_service::{self, PreparedTaskBoard},
    local_retention::HistoryLimits,
    native_intent::TaskPrompts,
    native_journal::NativeJournal,
    native_policies::NativePolicyFactory,
    registry::TaskBoardRegistry,
    schema::RuntimeContracts,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
};
use ivy_sdk::{clock::SystemClock, collation::LocaleComparator};
use std::{future::Future, path::PathBuf};

pub struct NativeRuntimeInputs<P> {
    pub contracts: RuntimeContracts,
    pub catalogs: CheckedNativeContracts,
    pub registry: TaskBoardRegistry,
    pub locale: LocaleComparator,
    pub prompts: P,
    pub chat_readable_versions: Vec<JsString>,
}

pub struct NativeRuntime<P> {
    pub host: PreparedTaskBoard,
    pub contracts: RuntimeContracts,
    pub catalogs: CheckedNativeContracts,
    pub policies: NativePolicyFactory<P>,
}

impl<P: TaskPrompts> NativeRuntime<P> {
    /// Host admission and build identity run before a journal is opened or the
    /// service starts. File placement comes only from the admitted data root.
    pub async fn prepare(
        path: impl Into<PathBuf>,
        inputs: NativeRuntimeInputs<P>,
    ) -> IvyResult<Self> {
        let config = ivy_host_runtime::config::native_instance_config(path).await?;
        Self::from_config(config, inputs)
    }

    pub(crate) fn from_config(
        config: ivy_host_runtime::config::InstanceConfig,
        inputs: NativeRuntimeInputs<P>,
    ) -> IvyResult<Self> {
        let host = host_service::from_config(
            config,
            &inputs.contracts,
            inputs.registry.dependency(),
            inputs.registry.tool_names().to_vec(),
        )?;
        let journal = NativeJournal::new(
            Some(host.config.data_root().to_owned()),
            HistoryLimits::default(),
        );
        Ok(Self {
            host,
            contracts: inputs.contracts,
            catalogs: inputs.catalogs,
            policies: NativePolicyFactory {
                journal,
                locale: inputs.locale,
                prompts: inputs.prompts,
                chat_readable_versions: inputs.chat_readable_versions,
            },
        })
    }

    /// Keep every native owner alive until SDK work, the generation, SQLite and
    /// final health publication have drained on this thread.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> IvyResult<()> {
        let Self {
            host,
            contracts,
            catalogs,
            policies,
        } = self;
        let service_node_id = host
            .config
            .json()
            .required("serviceNodeId")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?;
        let maintenance = crate::maintenance::MaintenanceContext::from_config(&host.config)?;
        let factory = EngineFactory {
            maintenance: Some(&maintenance),
            contracts: &contracts,
            catalogs: &catalogs,
            policies: &policies,
            open_journal: |_: &Json| policies.journal.open(),
            now: now as fn() -> String,
            new_id: new_id as fn() -> JsString,
            clock: SystemClock,
            service_node_id,
        };
        host.run(&factory, shutdown).await
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn new_id() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
