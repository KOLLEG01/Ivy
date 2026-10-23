//! Instruction RPC preparation composed with the coalesced output owner.
use crate::{
    instructions_source::{EnglishHintComparator, RpcInstructionsSource},
    instructions_writer::{InstructionsRun, InstructionsStatus, InstructionsWriter},
};
use ivy_contracts::{IvyError, IvyJson, JsString};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{path::PathBuf, sync::Arc};

pub struct InstructionsSettings {
    pub home: PathBuf,
    pub host_id: JsString,
    pub hive_url: JsString,
    pub defaults: IvyJson,
    pub english_hint_comparator: Arc<EnglishHintComparator>,
}
pub struct InstructionsManager {
    writer: InstructionsWriter,
    settings: InstructionsSettings,
}
impl InstructionsManager {
    pub fn new(settings: InstructionsSettings) -> Self {
        let writer = InstructionsWriter::new(
            settings.home.clone(),
            settings.host_id.clone(),
            settings.hive_url.clone(),
        );
        Self { writer, settings }
    }
    pub fn status(&self) -> InstructionsStatus {
        self.writer.status()
    }
    pub fn synchronize(&self, connection: Arc<dyn RpcClient>) -> InstructionsRun {
        self.writer.synchronize(Arc::new(RpcInstructionsSource::new(
            connection,
            self.settings.defaults.clone(),
            self.settings.host_id.clone(),
            self.settings.english_hint_comparator.clone(),
        )))
    }
    pub fn close(&self) -> InstructionsRun {
        self.writer.close()
    }
    /// Retain current bootstrap semantics. The separate AM03 policy decision
    /// governs adding an existing-state guard; this port does not add one.
    pub fn bootstrap(&self) -> InstructionsRun {
        self.synchronize(Arc::new(Bootstrap))
    }
}
struct Bootstrap;
impl RpcClient for Bootstrap {
    fn request<'a>(&'a self, _: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async {
            Err(IvyError::new(
                "not_found",
                "No stored instruction override.",
            ))
        })
    }
}
