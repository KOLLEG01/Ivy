//! Packaged schema admission and explicit local routing for engine fixtures.
use ivy_contracts::{IvyJson as Json, IvyResult, JsString};
use ivy_task_board::journal_store::Contracts;
use std::sync::OnceLock;
pub(crate) fn admission(name: &str, value: &Json) -> IvyResult<()> {
    current().validate_definition(name, value)
}
pub(crate) struct ContractsBinding;
fn current() -> &'static ivy_task_board::schema::RuntimeContracts {
    static CURRENT: OnceLock<ivy_task_board::schema::RuntimeContracts> = OnceLock::new();
    CURRENT.get_or_init(|| ivy_task_board::runtime_inputs::contracts().unwrap())
}
impl Contracts for ContractsBinding {
    fn version(&self, key: &str) -> IvyResult<String> {
        current().version(key)
    }
    // These fixtures route records into their actual SQLite journal. Production
    // routing remains selected by the packaged RuntimeContracts.
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        current().readable_version(key, version)
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        admission(name, value)
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        current().validate_record(key, version, value)
    }
}
