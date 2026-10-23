//! Lossless stored-record upgrades. Read admission, hydration and writes belong to the owner.
use ivy_contracts::{IvyJson as Json, IvyResult};
use std::borrow::Cow;
#[path = "task_board_record_configuration.rs"]
mod configuration;
#[path = "task_board_record_findings.rs"]
mod findings;
#[path = "task_board_record_result.rs"]
mod result;
#[path = "task_board_record_task.rs"]
mod task;
#[path = "task_board_record_value.rs"]
mod value;

/// Missing remains missing; only the former capability requirement is replaced.
pub fn upgrade_requirement(requirement: Option<&Json>) -> Option<Cow<'_, Json>> {
    configuration::requirement_json(requirement)
}
pub fn upgrade_configuration_record(configuration: &Json) -> IvyResult<Cow<'_, Json>> {
    configuration::json(configuration)
}
/// Normalize historical Task content to the current wakeAt/dependencies model.
/// Contract-version admission and conversation hydration belong to the caller.
pub fn upgrade_task_record(value: &Json) -> IvyResult<Json> {
    task::json(value)
}
pub fn upgrade_comment_record(comment: &Json) -> IvyResult<Json> {
    task::comment(comment)
}
pub fn upgrade_run_record(run: &Json) -> IvyResult<Json> {
    task::run(run)
}
pub fn upgrade_result_content(content: &Json) -> IvyResult<Json> {
    result::json_content(content)
}
pub fn upgrade_result_record(result: &Json) -> IvyResult<Json> {
    self::result::json_record(result)
}
