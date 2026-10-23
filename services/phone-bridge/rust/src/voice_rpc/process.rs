//! Configured Voice connection using the shared native process owner.
use super::{
    VoiceConnect, VoiceConnectOptions, VoiceConnection, VoiceConnector, VoiceControl,
    VoiceControlHooks, VoiceControlSettings,
};
use ivy_contracts::{IvyJson, IvyResult, JsString, schema::SchemaValidators};
use ivy_host_runtime::{
    codex_connection::NativeConnection,
    codex_home::HomeContext,
    codex_process::{StartOptions, start_codex_process},
    codex_rpc::Events,
    node_path::{self, Context},
};
use std::{collections::BTreeMap, sync::Arc};

pub(super) fn paths_snapshot(paths: &Context) -> Context {
    paths.clone()
}

struct AccountContext {
    paths: Context,
    environment: BTreeMap<JsString, JsString>,
    user_home: Option<JsString>,
    cwd: JsString,
    account_home: JsString,
}
impl AccountContext {
    fn borrowed(&self) -> HomeContext<'_> {
        HomeContext {
            paths: &self.paths,
            environment: &self.environment,
            user_home: self.user_home.as_ref(),
            cwd: &self.cwd,
            account_home: &self.account_home,
        }
    }
}

/// Retains the executing account's explicit inputs across asynchronous opens.
/// It neither discovers a different account nor substitutes service storage.
#[derive(Clone)]
pub struct NativeVoiceConnector(Arc<AccountContext>);
impl NativeVoiceConnector {
    pub fn new(context: &HomeContext<'_>) -> Self {
        Self(Arc::new(AccountContext {
            paths: paths_snapshot(context.paths),
            environment: context.environment.clone(),
            user_home: context.user_home.cloned(),
            cwd: context.cwd.clone(),
            account_home: context.account_home.clone(),
        }))
    }
}
impl VoiceConnector for NativeVoiceConnector {
    fn connect(&self, options: VoiceConnectOptions, events: Events) -> VoiceConnect {
        let account = self.0.clone();
        Box::pin(async move {
            let context = account.borrowed();
            let connection = start_codex_process(StartOptions {
                settings: &options.settings,
                artifact_root: &options.artifact_root,
                data_root: &options.data_root,
                home_context: &context,
                client_name: options.client_name.into(),
                client_version: options.client_version.into(),
                hive_credential: options.credential,
                events,
            })
            .await?;
            Ok(VoiceConnection::from_native(connection))
        })
    }
}
impl VoiceConnection {
    /// The delegate keeps the original owner alive and joins its retained stop
    /// result. RPC clones cannot replace or become a second cleanup authority.
    pub fn from_native(connection: NativeConnection) -> Self {
        let home = node_path::from_native(&connection.home);
        let rpc = connection.rpc.clone();
        let owner = Arc::new(connection);
        Self {
            rpc,
            home,
            close: Arc::new(move || {
                let owner = owner.clone();
                Box::pin(async move { owner.close().await })
            }),
        }
    }
}
impl VoiceControl {
    /// Production composition: one account snapshot, shared process ownership
    /// and submitted-schema compiler. Opening remains lazy until requested.
    pub fn configured(
        native: IvyJson,
        artifact_root: JsString,
        data_root: JsString,
        credential: Option<JsString>,
        context: &HomeContext<'_>,
        hooks: VoiceControlHooks,
    ) -> IvyResult<Self> {
        Self::new(
            VoiceControlSettings {
                native,
                artifact_root,
                data_root,
                credential,
                paths: paths_snapshot(context.paths),
            },
            Arc::new(NativeVoiceConnector::new(context)),
            Arc::new(SchemaValidators::new()),
            hooks,
        )
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_voice_process.rs"]
mod tests;
