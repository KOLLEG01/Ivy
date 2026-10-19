//! Exact frozen schema admission shared by isolated TaskBoard integration fixtures.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, builtin_schema::SchemaBundle};
use ivy_task_board::journal_store::Contracts;
use std::sync::OnceLock;
pub(crate) fn admission(name: &str, value: &Json) -> IvyResult<()> {
    static B: OnceLock<SchemaBundle> = OnceLock::new();
    let bundle = B.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../specs/schemas/task-board.schema.json"),
            include_str!("../../specs/schemas/chat.schema.json"),
            include_str!("../../specs/schemas/agent.schema.json"),
            include_str!("../../specs/schemas/hive-wire.schema.json"),
            include_str!("../../specs/schemas/hive-operations.schema.json"),
        ])
        .unwrap()
    });
    if bundle.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
        value,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            format!("Frozen {name} admission failed."),
        ))
    }
}
pub(crate) struct ContractsBinding;
impl Contracts for ContractsBinding {
    fn version(&self, key: &str) -> IvyResult<String> {
        Ok(match key {
            "task-board/task" => "1.8.0",
            "task-board/configuration" => "1.2.0",
            "task-board/run" => "1.1.0",
            _ => "1.0.0",
        }
        .into())
    }
    // Local public routing is finite; every retained value uses the full exact
    // frozen schema. No permissive Task/History/Result validation is supplied.
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from(self.version(key)?))
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        admission(name, value)
    }
    fn validate_record(&self, key: &str, _: &JsString, value: &Json) -> IvyResult<()> {
        admission(
            match key {
                "task-board/task" => "Task",
                "task-board/operation" => "Operation",
                "task-board/run" => "Run",
                "task-board/coordination" => "Coordination",
                "task-board/history" => "History",
                "task-board/result" => "Result",
                "task-board/configuration" => "Configuration",
                "task-board/task-key-allocation" => "TaskKeyAllocation",
                "task-board/task-key-sequence" => "TaskKeySequence",
                "task-board/native-read-evidence" => "NativeReadEvidence",
                "task-board/native-turn-snapshot" => "NativeTurnSnapshot",
                key => panic!("Unexpected record {key}"),
            },
            value,
        )
    }
}
