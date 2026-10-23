//! Packaged public schemas, writer selection and current prompt/registry inputs.
use crate::{
    native_runtime::NativeRuntimeInputs,
    prompts::CurrentTaskPrompts,
    registry::{RegistryText, TaskBoardRegistry},
    schema::RuntimeContracts,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, canonical_bundle::Bundle,
    checked_native_contract::CheckedNativeContracts, json::parse,
};
use ivy_sdk::collation::LocaleComparator;
use std::path::PathBuf;

pub(crate) fn metadata() -> IvyResult<Json> {
    parse(include_str!("runtime-inputs.json"), usize::MAX)
}

pub fn documents() -> IvyResult<Vec<(String, Json)>> {
    [
        (
            "TaskBoard",
            include_str!("../../../../specs/schemas/task-board.schema.json"),
        ),
        (
            "Chat",
            include_str!("../../../../specs/schemas/chat.schema.json"),
        ),
        (
            "Agent",
            include_str!("../../../../specs/schemas/agent.schema.json"),
        ),
        (
            "Wire",
            include_str!("../../../../specs/schemas/hive-wire.schema.json"),
        ),
        (
            "Operation",
            include_str!("../../../../specs/schemas/hive-operations.schema.json"),
        ),
    ]
    .into_iter()
    .map(|(name, document)| Ok((name.into(), parse(document, usize::MAX)?)))
    .collect()
}

pub fn contracts() -> IvyResult<RuntimeContracts> {
    RuntimeContracts::from_documents(documents()?.into_iter().map(|(_, value)| value).collect())
}

pub fn registry(contracts: &RuntimeContracts) -> IvyResult<TaskBoardRegistry> {
    let input = metadata()?;
    let text = input.required("registryText")?;
    let descriptions = text
        .required("descriptions")?
        .as_object()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|(name, value)| {
            Ok((
                name.to_utf8()?,
                value
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?,
            ))
        })
        .collect::<IvyResult<_>>()?;
    TaskBoardRegistry::new(
        &parse(
            include_str!("../../../../specs/schemas/task-board.operations.json"),
            usize::MAX,
        )?,
        &Bundle::from_documents(documents()?)?,
        contracts,
        RegistryText {
            descriptions,
            guide_markdown: text
                .required("guideMarkdown")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?,
        },
    )
}

/// Native catalog distribution and ICU ownership are explicit process inputs.
pub fn packaged(
    distribution: impl Into<PathBuf>,
    locale: LocaleComparator,
) -> IvyResult<NativeRuntimeInputs<CurrentTaskPrompts>> {
    let contracts = contracts()?;
    let registry = registry(&contracts)?;
    let input = metadata()?;
    let chat_readable_versions = input
        .required("chatReadableVersions")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|value| value.as_js_string().cloned().ok_or_else(IvyError::internal))
        .collect::<IvyResult<_>>()?;
    Ok(NativeRuntimeInputs {
        contracts,
        catalogs: CheckedNativeContracts::new(distribution),
        registry,
        locale,
        prompts: CurrentTaskPrompts,
        chat_readable_versions,
    })
}
