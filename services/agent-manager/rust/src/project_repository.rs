//! Bounded workspace Git observation. No fetch, checkout or remote mutation.
use crate::native_journal::JournalClock;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    exec_file::{self, FileCommand, FileOptions},
    node_path,
};
use std::{path::PathBuf, sync::Arc};

/// Shared WHATWG URL semantics implement username/password clearing. Invalid
/// URLs retain the original SCP-style remote, as in the original source.
pub type OriginUrlRedactor = dyn Fn(&JsString) -> IvyResult<JsString> + Send + Sync;

pub struct WorkspaceGit {
    pub bootstrap_root: PathBuf,
    pub job_launcher: Option<PathBuf>,
    pub redact_origin: Arc<OriginUrlRedactor>,
    pub clock: Arc<JournalClock>,
}
impl WorkspaceGit {
    pub fn new(bootstrap_root: PathBuf, job_launcher: Option<PathBuf>) -> Self {
        Self {
            bootstrap_root,
            job_launcher,
            redact_origin: Arc::new(ivy_contracts::whatwg_url::clear_url_credentials),
            clock: Arc::new(|| {
                chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into()
            }),
        }
    }
    pub async fn inspect_workspace(
        &self,
        intended: &JsString,
        source: Option<&JsString>,
        use_worktree: bool,
        verify_origin: bool,
    ) -> IvyResult<Option<IvyJson>> {
        let selected = if use_worktree && verify_origin {
            intended
        } else {
            source.unwrap_or(intended)
        };
        let mut result = self.inspect(selected, verify_origin).await?;
        if result.is_none() && use_worktree && verify_origin {
            if let Some(source) = source {
                result = self.inspect(source, verify_origin).await?;
                if let Some(repository) = &mut result {
                    let mut limitation = JsString::from(
                        "The intended dedicated worktree had no Git repository; the selected source repository was observed instead.",
                    );
                    if let Some(prior) = repository
                        .get("limitation")
                        .and_then(IvyJson::as_js_string)
                        .filter(|value| !value.0.is_empty())
                    {
                        limitation.0.push(32);
                        limitation.0.extend_from_slice(&prior.0);
                    }
                    repository.insert("limitation", IvyJson::String(limitation));
                }
            }
        }
        Ok(result)
    }
    pub async fn git(
        &self,
        cwd: &JsString,
        args: &[JsString],
        timeout_ms: u32,
    ) -> IvyResult<JsString> {
        let mut arguments = vec!["-C".into(), node_path::to_native(cwd).into_os_string()];
        arguments.extend(
            args.iter()
                .map(|value| node_path::to_native(value).into_os_string()),
        );
        let result = exec_file::exec_file(
            FileCommand {
                executable: "git".into(),
                args: arguments,
                cwd: None,
                timeout_ms,
                maximum_buffer_bytes: 1024 * 1024,
            },
            self.bootstrap_root.clone(),
            FileOptions {
                job_launcher: self.job_launcher.clone(),
                ..Default::default()
            },
        )
        .await?;
        if result.failure.is_some() {
            return Err(IvyError::internal());
        }
        Ok(result.stdout.trim())
    }
    pub async fn common(&self, cwd: &JsString, paths: &node_path::Context) -> IvyResult<JsString> {
        let common = self
            .git(
                cwd,
                &["rev-parse".into(), "--git-common-dir".into()],
                30_000,
            )
            .await?;
        let path = node_path::to_native(&paths.resolve(&[cwd.clone(), common]));
        let resolved =
            tokio::task::spawn_blocking(move || ivy_host_runtime::paths::realpath(&path))
                .await
                .map_err(|_| IvyError::internal())?
                .map_err(|_| IvyError::internal())?;
        Ok(node_path::from_native(&resolved))
    }
    pub async fn inspect(
        &self,
        path: &JsString,
        verify_origin: bool,
    ) -> IvyResult<Option<IvyJson>> {
        if !tokio::fs::metadata(node_path::filesystem_path(path))
            .await
            .is_ok_and(|metadata| metadata.is_dir())
        {
            return Ok(None);
        }
        // Every command inside this original observation is best-effort. A
        // failed repository read yields null; remote failures retain limitation.
        let commit = match self
            .git(path, &["rev-parse".into(), "HEAD".into()], 3000)
            .await
        {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let branch = self
            .git(
                path,
                &[
                    "symbolic-ref".into(),
                    "--quiet".into(),
                    "--short".into(),
                    "HEAD".into(),
                ],
                3000,
            )
            .await
            .unwrap_or_default();
        let dirty = match self
            .git(path, &["status".into(), "--porcelain".into()], 3000)
            .await
        {
            Ok(value) => !value.0.is_empty(),
            Err(_) => return Ok(None),
        };
        let raw = self
            .git(
                path,
                &["remote".into(), "get-url".into(), "origin".into()],
                3000,
            )
            .await
            .unwrap_or_default();
        let origin = (self.redact_origin)(&raw).unwrap_or(raw);
        let mut origin_state = if origin.0.is_empty() {
            "no_origin"
        } else {
            "unknown"
        };
        let mut remote_ref = None;
        let mut limitation = None;
        if !origin.0.is_empty() && verify_origin {
            let reference = if branch.0.is_empty() {
                JsString::from("HEAD")
            } else {
                let mut value = JsString::from("refs/heads/").0;
                value.extend_from_slice(&branch.0);
                JsString(value)
            };
            match self
                .git(
                    path,
                    &["ls-remote".into(), "origin".into(), reference.clone()],
                    3000,
                )
                .await
            {
                Ok(lines) => {
                    let first_line = lines
                        .0
                        .split(|unit| *unit == 10)
                        .find(|line| !line.is_empty())
                        .unwrap_or(&[]);
                    let first_word = first_line
                        .iter()
                        .copied()
                        .take_while(|unit| !JsString(vec![*unit]).trim().0.is_empty())
                        .collect::<Vec<_>>();
                    let remote = JsString(first_word);
                    if remote.0.is_empty() {
                        origin_state = "local_only";
                        limitation = Some(format!("Origin has no {}.", reference.to_utf8()?));
                    } else if remote == commit {
                        origin_state = "confirmed";
                    } else {
                        let reachable = self
                            .git(
                                path,
                                &[
                                    "merge-base".into(),
                                    "--is-ancestor".into(),
                                    commit.clone(),
                                    remote,
                                ],
                                3000,
                            )
                            .await
                            .is_ok();
                        origin_state = if reachable { "confirmed" } else { "local_only" };
                        if !reachable {
                            limitation = Some(format!(
                                "Commit is not confirmed reachable from origin {}.",
                                reference.to_utf8()?
                            ));
                        }
                    }
                }
                Err(_) => {
                    limitation = Some(format!(
                        "Origin {} could not be observed at result publication time.",
                        reference.to_utf8()?
                    ))
                }
            }
            remote_ref = Some(reference);
        } else if !origin.0.is_empty() {
            limitation =
                Some("Origin reachability was not checked for this workspace observation.".into());
        }
        let name = path
            .0
            .rsplit(|unit| matches!(*unit, 47 | 92))
            .next()
            .filter(|part| !part.is_empty())
            .map(|part| JsString(part.to_vec()))
            .unwrap_or_else(|| path.clone());
        let nullable = |value: JsString| {
            if value.0.is_empty() {
                IvyJson::Null
            } else {
                IvyJson::String(value)
            }
        };
        Ok(Some(IvyJson::object([
            ("name", IvyJson::String(name)),
            ("branch", nullable(branch)),
            ("commit", nullable(commit)),
            ("dirty", dirty.into()),
            (
                "originName",
                if origin.0.is_empty() {
                    IvyJson::Null
                } else {
                    "origin".into()
                },
            ),
            ("originUrl", nullable(origin)),
            ("originState", origin_state.into()),
            (
                "remoteRef",
                remote_ref.map(IvyJson::String).unwrap_or(IvyJson::Null),
            ),
            ("observedAt", IvyJson::String((self.clock)())),
            (
                "limitation",
                limitation.map(IvyJson::from).unwrap_or(IvyJson::Null),
            ),
        ])))
    }
}
