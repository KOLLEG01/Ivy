#[path = "../fixtures/task-board-registry.rs"]
mod registry;
#[path = "../fixtures/task-board-runtime-inputs.rs"]
mod runtime_inputs;

#[test]
fn context_only_handoff_preserves_workflow_and_rejects_unapplied_scheduling_fields() {
    use ivy_contracts::{IvyJson as Json, json::parse};
    use ivy_task_board::{journal_store::Contracts, schema::RuntimeContracts};
    let contracts = RuntimeContracts::from_documents(
        ivy_task_board::runtime_inputs::documents()
            .unwrap()
            .into_iter()
            .map(|(_, value)| value)
            .collect(),
    )
    .unwrap();
    for text in [
        r#"{"nativeContext":"release"}"#,
        r#"{"nativeContext":"retain"}"#,
        r#"{"workflowState":"waiting","nativeContext":"release","dependencies":[],"wakeAt":null}"#,
    ] {
        contracts
            .validate_definition("Handoff", &parse(text, 65536).unwrap())
            .unwrap();
    }
    for text in [
        "{}",
        r#"{"nativeContext":"release","wakeAt":null}"#,
        r#"{"nativeContext":"release","dependencies":[]}"#,
    ] {
        assert!(
            contracts
                .validate_definition("Handoff", &parse(text, 65536).unwrap())
                .is_err()
        );
    }
    assert!(
        contracts
            .validate_definition("Handoff", &Json::Null)
            .is_err()
    );
}
