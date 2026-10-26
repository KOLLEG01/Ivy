//! Authoritative instruction reads and bounded discovery guidance.
use crate::{
    environment::{
        EnvironmentKind, json_falsy, read_environment_document, select_environment_document,
    },
    instructions::{ComposedInstructions, compose_instructions},
    instructions_files::{InstructionsSource, regular_bytes},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
use std::{
    cmp::Ordering,
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, OnceLock},
};

/// Host display ordering for English discovery hints. Equal names retain
/// discovery order.
pub type EnglishHintComparator = dyn Fn(&JsString, &JsString) -> Ordering + Send + Sync;

#[derive(Clone)]
pub struct DiscoveryHint {
    pub service_name: JsString,
    pub discovery_hint: Option<JsString>,
}
fn trim(value: &JsString) -> JsString {
    fn space(c: &u16) -> bool {
        matches!(*c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
    }
    let start = value
        .0
        .iter()
        .position(|c| !space(c))
        .unwrap_or(value.0.len());
    let end = value
        .0
        .iter()
        .rposition(|c| !space(c))
        .map_or(start, |index| index + 1);
    JsString(value.0[start..end].to_vec())
}
pub fn instruction_summary() -> &'static JsString {
    static VALUE: OnceLock<JsString> = OnceLock::new();
    VALUE.get_or_init(|| {
        trim(&include_str!("../../../../instructions/hive-mcp-agent-summary.md").into())
    })
}
fn full_instructions() -> &'static JsString {
    static VALUE: OnceLock<JsString> = OnceLock::new();
    VALUE.get_or_init(|| {
        trim(&include_str!("../../../../instructions/hive-mcp-instructions.md").into())
    })
}
const CAPABILITIES: &str =
    "\n\nServices registered to this Hive provide the following capabilities:\n";
const WORKFLOWS: &str = "\n\nTool workflows:\n";
fn position(source: &[u16], pattern: &[u16]) -> Option<usize> {
    source
        .windows(pattern.len())
        .position(|candidate| candidate == pattern)
}
pub fn compact_mcp_instructions(value: &JsString) -> JsString {
    let capability: JsString = CAPABILITIES.into();
    let Some(start) = position(&value.0, &capability.0) else {
        return instruction_summary().clone();
    };
    let workflow: JsString = WORKFLOWS.into();
    let end = position(&value.0[start..], &workflow.0).map_or(value.0.len(), |index| start + index);
    let mut result = instruction_summary().clone();
    result.0.extend_from_slice(&value.0[start..end]);
    result
}
/// The legacy path checks the full Hive guidance budget before compacting it,
/// even though only the summary/capabilities are written into the override.
pub fn legacy_mcp_instructions(
    hints: &[DiscoveryHint],
    compare: &EnglishHintComparator,
) -> IvyResult<JsString> {
    let mut entries: Vec<(JsString, Vec<JsString>)> = Vec::new();
    for entry in hints {
        let Some(hint) = entry
            .discovery_hint
            .as_ref()
            .map(trim)
            .filter(|s| !s.0.is_empty())
        else {
            continue;
        };
        let index = entries
            .iter()
            .position(|(service, _)| service == &entry.service_name);
        let values = if let Some(index) = index {
            &mut entries[index].1
        } else {
            entries.push((entry.service_name.clone(), Vec::new()));
            &mut entries.last_mut().unwrap().1
        };
        if !values.contains(&hint) {
            values.push(hint);
        }
    }
    entries.sort_by(|(left, _), (right, _)| compare(left, right));
    let mut lines: Vec<JsString> = Vec::new();
    for (service, mut hints) in entries {
        hints.sort_by(|left, right| compare(left, right));
        for hint in hints {
            let mut line = JsString::from("- ");
            line.0.extend_from_slice(&service.0);
            line.0.extend([58, 32]);
            line.0.extend(hint.0);
            lines.push(line);
        }
    }
    let mut full = full_instructions().clone();
    if !lines.is_empty() {
        full.0.extend(CAPABILITIES.encode_utf16());
        for (index, line) in lines.iter().enumerate() {
            if index != 0 {
                full.0.push(10);
            }
            full.0.extend_from_slice(&line.0);
        }
    }
    if String::from_utf16_lossy(&full.0).len() > 32768 {
        return Err(IvyError::new(
            "limit_exceeded",
            "Composed Hive instructions exceed 32 KiB.",
        ));
    }
    Ok(compact_mcp_instructions(&full))
}
fn field<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    value.get(name).ok_or_else(IvyError::internal)
}
pub async fn discover_mcp_instructions(
    connection: &dyn RpcClient,
    compare: &EnglishHintComparator,
) -> IvyResult<JsString> {
    match connection
        .request(
            "discovery.instructions",
            IvyJson::object([] as [(&str, IvyJson); 0]),
            RequestOptions::default(),
        )
        .await
    {
        Ok(value) => {
            let instructions = field(&value, "instructions")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?;
            return Ok(compact_mcp_instructions(instructions));
        }
        Err(error) if error.code == "not_found" => {}
        Err(error) => return Err(error),
    }
    let mut hints = Vec::new();
    let mut cursor: Option<JsString> = None;
    for _ in 0..20 {
        let mut params = vec![("limit", IvyJson::Number(20.0))];
        if let Some(cursor) = &cursor {
            params.push(("cursor", IvyJson::String(cursor.clone())));
        }
        let page = match connection
            .request(
                "discovery.list",
                IvyJson::object(params),
                RequestOptions::default(),
            )
            .await
        {
            Ok(value) if json_falsy(&value) => return Ok(instruction_summary().clone()),
            Ok(value) => value,
            Err(error) if error.code == "not_found" => return Ok(instruction_summary().clone()),
            Err(error) => return Err(error),
        };
        let items = field(&page, "items")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        for item in items {
            if item.get("kind") != Some(&IvyJson::from("service")) {
                continue;
            }
            let service_name = field(item, "serviceName")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?;
            let discovery_hint = match item.get("description") {
                None | Some(IvyJson::Null) => None,
                Some(value) => Some(
                    value
                        .as_js_string()
                        .cloned()
                        .ok_or_else(IvyError::internal)?,
                ),
            };
            hints.push(DiscoveryHint {
                service_name,
                discovery_hint,
            });
        }
        cursor = match page.get("nextCursor").filter(|value| !json_falsy(value)) {
            None => None,
            Some(IvyJson::String(value)) => (!value.0.is_empty()).then(|| value.clone()),
            _ => {
                return Err(IvyError::new(
                    "invalid_frame",
                    "Hive discovery cursor must be a string.",
                ));
            }
        };
        if cursor.is_none() {
            return legacy_mcp_instructions(&hints, compare);
        }
    }
    Err(IvyError::new(
        "limit_exceeded",
        "Hive service discovery exceeds the bounded instruction projection.",
    ))
}
pub async fn prepare_instructions(
    home: &Path,
    connection: &dyn RpcClient,
    defaults: &IvyJson,
    host_id: &JsString,
    compare: &EnglishHintComparator,
) -> IvyResult<ComposedInstructions> {
    let global = read_environment_document(connection, EnvironmentKind::Instructions, None).await?;
    let host =
        read_environment_document(connection, EnvironmentKind::Instructions, Some(host_id)).await?;
    let selected = select_environment_document(defaults, global.as_ref(), host.as_ref())?.document;
    let active = selected.get("enabled") == Some(&IvyJson::Bool(true));
    let guidance = if active {
        discover_mcp_instructions(connection, compare).await?
    } else {
        instruction_summary().clone()
    };
    let local = if active && selected.get("includeLocal") == Some(&IvyJson::Bool(true)) {
        let path = home.join("AGENTS.md");
        tokio::task::spawn_blocking(move || regular_bytes(&path))
            .await
            .map_err(|_| IvyError::internal())??
    } else {
        None
    };
    compose_instructions(
        global.as_ref(),
        host.as_ref(),
        local.as_deref(),
        Some(defaults),
        &guidance,
    )
}
pub struct RpcInstructionsSource {
    connection: Arc<dyn RpcClient>,
    defaults: IvyJson,
    host_id: JsString,
    compare: Arc<EnglishHintComparator>,
}
impl RpcInstructionsSource {
    pub fn new(
        connection: Arc<dyn RpcClient>,
        defaults: IvyJson,
        host_id: JsString,
        compare: Arc<EnglishHintComparator>,
    ) -> Self {
        Self {
            connection,
            defaults,
            host_id,
            compare,
        }
    }
}
impl InstructionsSource for RpcInstructionsSource {
    fn desired<'a>(
        &'a self,
        home: &'a Path,
    ) -> Pin<Box<dyn Future<Output = IvyResult<ComposedInstructions>> + Send + 'a>> {
        Box::pin(prepare_instructions(
            home,
            self.connection.as_ref(),
            &self.defaults,
            &self.host_id,
            self.compare.as_ref(),
        ))
    }
}
