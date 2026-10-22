//! Secretary preflight admission and completion policy around the Host process driver.
use crate::{assignment_decision::trimmed, assignment_execution::AssignmentExecution};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    exec_file::{FileCommand, FileOptions, exec_file},
    node_path::{filesystem_path, to_native},
};
use std::path::{Path, PathBuf};

pub struct PreparedPreflight {
    pub current: IvyJson,
    pub started_at: JsString,
}
pub enum PreflightResult {
    Completed {
        stdout: JsString,
        stderr: JsString,
    },
    Failed {
        error: IvyError,
        is_ivy_error: bool,
        stdout: IvyJson,
        stderr: IvyJson,
    },
}
pub struct AssignmentPreflight<'a, 'b, 'engine, 'journal> {
    pub execution: &'a AssignmentExecution<'b, 'engine, 'journal>,
}
fn output(parts: &[&JsString]) -> JsString {
    let mut units = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            units.push(10);
        }
        units.extend_from_slice(&part.0);
    }
    units.truncate(65_536);
    JsString(units)
}
impl AssignmentPreflight<'_, '_, '_, '_> {
    /// Execute one already fenced job. The generation retains its active ID and
    /// polls this future through completion/drain; this method never retries it.
    /// The selected distribution and optional helper must share one candidate.
    pub async fn execute(
        &self,
        current: &IvyJson,
        bootstrap_root: &Path,
        job_launcher: Option<PathBuf>,
    ) -> IvyResult<IvyJson> {
        let Some(prepared) = self.prepare(current).await? else {
            return Ok(current.clone());
        };
        let value = prepared.current.required("value")?;
        let command = value
            .required("assignmentSnapshot")?
            .required("preflight")?;
        let native = |value: &IvyJson| {
            value
                .as_js_string()
                .map(|text| to_native(text).into_os_string())
                .ok_or_else(IvyError::internal)
        };
        let executable = command
            .required("executable")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let command = FileCommand {
            // Node's JS executable string crosses UTF-8 before native spawn.
            executable: filesystem_path(executable).into_os_string(),
            args: command
                .required("args")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(native)
                .collect::<IvyResult<_>>()?,
            cwd: Some(PathBuf::from(native(
                value.required("executionTarget")?.required("threadCwd")?,
            )?)),
            timeout_ms: command
                .required("timeoutMs")?
                .as_f64()
                .ok_or_else(IvyError::internal)? as u32,
            maximum_buffer_bytes: 65_536,
        };
        let result = exec_file(
            command,
            bootstrap_root.to_path_buf(),
            FileOptions {
                job_launcher,
                cancellation: self
                    .execution
                    .native
                    .evidence
                    .cancellation
                    .cancellation
                    .clone(),
                ..Default::default()
            },
        )
        .await;
        // Original execFile errors are ordinary JS errors. Keep that boundary
        // even though Host exposes typed failures internally; cancellation is
        // rethrown by complete, and every other process failure stays fenced.
        let result = match result {
            Ok(result) if result.failure.is_none() => PreflightResult::Completed {
                stdout: result.stdout,
                stderr: result.stderr,
            },
            Ok(result) => PreflightResult::Failed {
                error: IvyError::internal(),
                is_ivy_error: false,
                stdout: IvyJson::String(result.stdout),
                stderr: IvyJson::String(result.stderr),
            },
            Err(_) => PreflightResult::Failed {
                error: IvyError::internal(),
                is_ivy_error: false,
                stdout: IvyJson::Null,
                stderr: IvyJson::Null,
            },
        };
        self.complete(&prepared, result).await
    }

    /// None preserves the unchanged execution when no command was configured.
    /// The runtime invokes its Host adapter only after this owner check succeeds.
    pub async fn prepare(&self, current: &IvyJson) -> IvyResult<Option<PreparedPreflight>> {
        let value = current.required("value")?;
        if value
            .required("assignmentSnapshot")?
            .get("preflight")
            .is_none_or(IvyJson::is_null)
        {
            return Ok(None);
        }
        let target = self.execution.target(value).await?;
        let evidence = self.execution.native.evidence;
        if target.get("hostId") != evidence.settings.required("identity")?.get("hostId") {
            return Err(IvyError::new(
                "secretary_preflight_host_mismatch",
                "A preflight command must run on the AgentManager host.",
            ));
        }
        let started_at = self.execution.now()?;
        evidence.verify_owner().await?;
        Ok(Some(PreparedPreflight {
            current: current.clone(),
            started_at,
        }))
    }
    /// Uses the timestamp captured before process execution. Process errors carry
    /// both their normalized error and whether the original error was an IvyError.
    pub async fn complete(
        &self,
        prepared: &PreparedPreflight,
        result: PreflightResult,
    ) -> IvyResult<IvyJson> {
        let mut next = prepared.current.required("value")?.clone();
        let at = &prepared.started_at;
        match result {
            PreflightResult::Completed { stdout, stderr } => {
                let parts: Vec<_> = [&stdout, &stderr]
                    .into_iter()
                    .filter(|s| !s.0.is_empty())
                    .collect();
                next.insert("preflightOutput", IvyJson::String(output(&parts)));
                next.insert("phase", "starting".into());
                let started = next
                    .get("startedAt")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(IvyJson::String(at.clone()));
                next.insert("startedAt", started);
            }
            PreflightResult::Failed {
                error,
                is_ivy_error,
                stdout,
                stderr,
            } => {
                if self
                    .execution
                    .native
                    .evidence
                    .cancellation
                    .cancellation
                    .is_cancelled()
                {
                    return Err(error);
                }
                let code = if is_ivy_error {
                    error.code.as_str()
                } else {
                    "secretary_preflight_failed"
                };
                let parts: Vec<_> = [&stderr, &stdout]
                    .into_iter()
                    .filter_map(IvyJson::as_js_string)
                    .filter(|s| !trimmed(&s.0).is_empty())
                    .collect();
                let detail = output(&parts);
                next.insert("phase", "failed".into());
                next.insert("errorCode", code.into());
                next.insert(
                    "preflightOutput",
                    if detail.0.is_empty() {
                        code.into()
                    } else {
                        IvyJson::String(detail)
                    },
                );
                next.insert("completedAt", IvyJson::String(at.clone()));
            }
        }
        next.insert("updatedAt", IvyJson::String(at.clone()));
        self.execution.update(&prepared.current, &next).await
    }
}
