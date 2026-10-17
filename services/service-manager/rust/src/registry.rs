use ivy_contracts::{IvyJson, IvyResult};

pub const HOST_SCHEMA: &str = "https://ivy.invalid/schemas/host.schema.json";
pub const WIRE_SCHEMA: &str = "https://ivy.invalid/schemas/hive-wire.schema.json";

/// Schema expansion is supplied by the common host bundler. Keeping the exact
/// normative graph here avoids a second schema generator or approximated types.
pub fn registry(mut bundle: impl FnMut(&str, &str) -> IvyResult<IvyJson>) -> IvyResult<IvyJson> {
    let specifications = [
        (
            "status",
            "ManagerStatusInput",
            "HostStatus",
            "Check this host’s installed services, builds, readiness and deployment activity.",
        ),
        (
            "deployment",
            "ManagerDeploymentInput",
            "JournalEntry",
            "Read progress and outcome of an original deployment on this host.",
        ),
        (
            "deploy",
            "ManagerDeployInput",
            "DeploymentRecord",
            "Deploy a validated candidate or configured local source. Acceptance starts the operation; use host.deployment to check readiness.",
        ),
        (
            "rollback",
            "ManagerRollbackInput",
            "DeploymentRecord",
            "Roll back to a selected compatible build retained on this host; check host.deployment for the outcome.",
        ),
        (
            "restart",
            "ManagerLifecycleInput",
            "DeploymentRecord",
            "Request restart for one configured service instance on this host; check host.deployment for the outcome.",
        ),
        (
            "enable",
            "ManagerLifecycleInput",
            "DeploymentRecord",
            "Request enable for one configured service instance on this host; check host.deployment for the outcome.",
        ),
        (
            "disable",
            "ManagerLifecycleInput",
            "DeploymentRecord",
            "Request disable for one configured service instance on this host; check host.deployment for the outcome.",
        ),
    ];
    let mut tools = Vec::with_capacity(specifications.len());
    for (name, input, output, description) in specifications {
        tools.push(IvyJson::object([
            ("namespace", "host".into()),
            ("name", name.into()),
            ("interfaceVersion", "1.0.0".into()),
            ("description", description.into()),
            (
                "inputSchema",
                bundle(&format!("#/$defs/{input}"), HOST_SCHEMA)?,
            ),
            (
                "outputSchema",
                bundle(
                    &format!("#/$defs/{output}"),
                    if output == "DeploymentRecord" {
                        WIRE_SCHEMA
                    } else {
                        HOST_SCHEMA
                    },
                )?,
            ),
            (
                "annotations",
                IvyJson::object([
                    (
                        "readOnlyHint",
                        matches!(name, "status" | "deployment").into(),
                    ),
                    ("idempotentHint", true.into()),
                ]),
            ),
        ]));
    }
    Ok(IvyJson::object([
        ("discoveryHint", "".into()),
        ("namespaces", IvyJson::Array(vec![IvyJson::object([
            ("namespace", "host".into()),
            ("description", "Host-local deployment and recovery".into()),
            ("guideMarkdown", "Select the host that owns the service and read host.status for instance IDs, builds and unfinished operations. Deploy a validated candidate or configured source, then follow host.deployment until the intended build is ready. Operations continue in the local executor after disconnection; keep operationId and inspect the original deployment before retrying.".into()),
            ("tools", IvyJson::Array(tools)),
            ("topics", IvyJson::Array(vec![])),
            ("inventoryKinds", IvyJson::Array(vec![])),
        ])])),
        ("contracts", IvyJson::Array(vec![])),
        ("requiredContracts", IvyJson::Array(vec![])),
    ]))
}
