//! Codex owns project identities; this owner prepares and validates directories.
use crate::{
    local_state::LocalAgentState, project_policy::ProjectPolicy, project_repository::WorkspaceGit,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{filesystem, node_path, paths};
use ivy_sdk::service::ServiceFuture;
use std::{
    io,
    sync::{Arc, Mutex},
};

pub type RegisterProject = dyn Fn(IvyJson) -> ServiceFuture<IvyJson> + Send + Sync;
struct Inner {
    policy: Arc<ProjectPolicy>,
    native_projects: Mutex<Vec<IvyJson>>,
    register: Option<Arc<RegisterProject>>,
    state: Option<Arc<Mutex<LocalAgentState>>>,
    git: Arc<WorkspaceGit>,
}
#[derive(Clone)]
pub struct ProjectLocations(Arc<Inner>);

fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn internal<T>(value: Result<T, impl std::fmt::Debug>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> IvyResult<T> + Send + 'static,
) -> IvyResult<T> {
    internal(tokio::task::spawn_blocking(work).await)?
}
fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn realpath(path: &JsString) -> IvyResult<JsString> {
    internal(paths::realpath(&node_path::to_native(path))).map(|path| node_path::from_native(&path))
}
fn access(path: &JsString) -> IvyResult<()> {
    internal(filesystem::access(
        &node_path::to_native(path),
        filesystem::READ_OK | filesystem::WRITE_OK,
    ))
}
fn mkdir(path: &JsString, recursive: bool) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(recursive)
        .create(node_path::filesystem_path(path))
}
fn directory(path: &JsString) -> io::Result<bool> {
    std::fs::metadata(node_path::filesystem_path(path)).map(|metadata| metadata.is_dir())
}
fn reference(service_node_id: &JsString, native_id: &JsString) -> IvyJson {
    IvyJson::object([
        ("namespace", "codex".into()),
        ("kind", "project".into()),
        ("serviceNodeId", IvyJson::String(service_node_id.clone())),
        ("nativeId", IvyJson::String(native_id.clone())),
    ])
}

impl ProjectLocations {
    pub fn new(
        policy: ProjectPolicy,
        register: Option<Arc<RegisterProject>>,
        state: Option<Arc<Mutex<LocalAgentState>>>,
        git: Arc<WorkspaceGit>,
    ) -> Self {
        Self(Arc::new(Inner {
            policy: Arc::new(policy),
            native_projects: Mutex::new(Vec::new()),
            register,
            state,
            git,
        }))
    }
    pub fn defaults(&self) -> IvyJson {
        self.0.policy.defaults()
    }
    pub fn set_native_projects(&self, projects: &[IvyJson]) -> IvyResult<()> {
        *self
            .0
            .native_projects
            .lock()
            .map_err(|_| IvyError::internal())? = projects.to_vec();
        Ok(())
    }
    pub fn close(&self) -> IvyResult<()> {
        self.set_native_projects(&[])
    }
    fn projects(&self) -> IvyResult<Vec<IvyJson>> {
        Ok(self
            .0
            .native_projects
            .lock()
            .map_err(|_| IvyError::internal())?
            .clone())
    }
    async fn prepare_directory(
        &self,
        path: JsString,
        root: JsString,
        exclusive: bool,
    ) -> IvyResult<JsString> {
        let policy = self.0.policy.clone();
        blocking(move || {
            let boundaries = policy
                .protected_roots
                .iter()
                .map(paths::future_path)
                .collect::<IvyResult<Vec<_>>>()?;
            let intended = paths::future_path(&path)?;
            let parent = paths::future_path(&root)?;
            require(
                policy.paths.inside(&parent, &intended)
                    && boundaries
                        .iter()
                        .all(|value| !policy.paths.inside(value, &intended)),
                "target_conflict",
                "Working directories cannot enter protected service, Codex or artifact storage.",
            )?;
            if exclusive {
                internal(mkdir(&root, true))?;
            }
            if let Err(error) = mkdir(&path, !exclusive) {
                if exclusive && error.kind() == io::ErrorKind::AlreadyExists {
                    let mut message = JsString::from("Project directory already exists: ");
                    message.0.extend_from_slice(&path.0);
                    message.0.extend(
                        ". Choose another name or select the existing directory.".encode_utf16(),
                    );
                    return Err(
                        IvyError::new_js("target_conflict".into(), message).with_details(
                            IvyJson::object([("reason", "project_directory_exists".into())]),
                        ),
                    );
                }
                return Err(IvyError::internal());
            }
            let cwd = realpath(&path)?;
            require(
                policy.same_path(&cwd, &intended)
                    && policy.paths.inside(&realpath(&root)?, &cwd)
                    && boundaries
                        .iter()
                        .all(|value| !policy.paths.inside(value, &cwd)),
                "target_conflict",
                "The working directory was redirected outside its allocation.",
            )?;
            access(&cwd)?;
            Ok(cwd)
        })
        .await
    }
    pub async fn resolve(
        &self,
        selection: &IvyJson,
        principal: &JsString,
        expected: Option<&JsString>,
    ) -> IvyResult<IvyJson> {
        let policy = &self.0.policy;
        let kind = text(selection, "kind")?.to_utf8()?;
        let expected_truthy = expected.filter(|value| !value.0.is_empty());
        if kind == "projectless" {
            require(
                expected_truthy.is_none(),
                "invalid_arguments",
                "A projectless task cannot select a project identity.",
            )?;
            let path = policy.projectless_path(principal, selection.required("key")?)?;
            let cwd = self
                .prepare_directory(path, policy.internal_project_root.clone(), false)
                .await?;
            return Ok(IvyJson::object([
                ("kind", kind.into()),
                ("cwd", IvyJson::String(cwd)),
                ("project", IvyJson::Null),
            ]));
        }
        if kind == "internal" || kind == "task" {
            let project = policy.internal_project(&self.projects()?, expected)?;
            let root = &policy.internal_project_root;
            let path = if kind == "task" {
                let key = text(selection, "key")?;
                ProjectPolicy::task_key(key)?;
                node_path::join(policy.paths.style, &[root.clone(), key.clone()])
            } else {
                root.clone()
            };
            let cwd = self.prepare_directory(path, root.clone(), false).await?;
            return Ok(IvyJson::object([
                ("kind", kind.into()),
                ("cwd", IvyJson::String(cwd)),
                ("project", project),
            ]));
        }
        let cwd = if kind == "existing" {
            let path = text(selection, "cwd")?.clone();
            require(
                node_path::is_absolute(policy.paths.style, &path),
                "invalid_arguments",
                "Selected project cwd must be absolute.",
            )?;
            blocking(move || {
                let cwd = realpath(&path)?;
                require(
                    internal(directory(&cwd))?,
                    "target_conflict",
                    "Selected project cwd must be a directory.",
                )?;
                access(&cwd)?;
                Ok(cwd)
            })
            .await?
        } else {
            let name = text(selection, "name")?;
            policy.project_name(name)?;
            let root = policy.project_root.clone();
            let path = node_path::join(policy.paths.style, &[root.clone(), name.clone()]);
            let internal_root = policy.internal_project_root.clone();
            let target = path.clone();
            let policy_copy = policy.clone();
            blocking(move || {
                require(
                    !policy_copy.paths.inside(
                        &paths::future_path(&internal_root)?,
                        &paths::future_path(&target)?,
                    ),
                    "target_conflict",
                    "Normal projects cannot be allocated inside the internal working directory.",
                )
            })
            .await?;
            let state = self.0.state.clone().ok_or_else(|| {
                IvyError::new(
                    "service_not_ready",
                    "Project allocation state is unavailable.",
                )
            })?;
            let key = ProjectPolicy::allocation_key(principal, selection)?;
            let prior = {
                let state = state.clone();
                let key = key.clone();
                blocking(move || {
                    state
                        .lock()
                        .map_err(|_| IvyError::internal())?
                        .read("project-allocation", &key)
                })
                .await?
            };
            if let Some(prior) = &prior {
                let path = path.clone();
                let prior = prior.value.clone();
                let policy = policy.clone();
                blocking(move || {
                    let identity =
                        internal(filesystem::stat_identity(&node_path::to_native(&path)))?;
                    require(
                        policy.same_path(text(&prior, "cwd")?, &realpath(&path)?)
                            && text(&prior, "dev")? == &JsString::from(identity.dev)
                            && text(&prior, "ino")? == &JsString::from(identity.ino),
                        "target_conflict",
                        "The original project directory has been replaced or redirected.",
                    )
                })
                .await?;
            }
            let cwd = self.prepare_directory(path, root, prior.is_none()).await?;
            if prior.is_none() {
                let cwd = cwd.clone();
                blocking(move || {
                    let identity =
                        internal(filesystem::stat_identity(&node_path::to_native(&cwd)))?;
                    state.lock().map_err(|_| IvyError::internal())?.write(
                        "project-allocation",
                        &key,
                        &IvyJson::object([
                            ("cwd", IvyJson::String(cwd)),
                            ("dev", identity.dev.into()),
                            ("ino", identity.ino.into()),
                        ]),
                        None,
                    )?;
                    Ok(())
                })
                .await?;
            }
            cwd
        };
        if let Some(project) = policy.existing_project(&self.projects()?, &cwd, expected)? {
            return Ok(IvyJson::object([
                ("kind", kind.into()),
                ("cwd", IvyJson::String(cwd)),
                ("project", project),
            ]));
        }
        let register = self.0.register.as_ref().ok_or_else(|| {
            IvyError::new(
                "native_inventory_unavailable",
                "Native project registration is unavailable.",
            )
        })?;
        let name = if kind == "existing" {
            let name = node_path::basename(policy.paths.style, &cwd);
            if name.0.is_empty() { cwd.clone() } else { name }
        } else {
            text(selection, "name")?.clone()
        };
        let project = register(IvyJson::object([
            ("name", IvyJson::String(name)),
            ("cwd", IvyJson::String(cwd.clone())),
            (
                "idempotencyKey",
                ProjectPolicy::allocation_key(principal, selection)?.into(),
            ),
        ]))
        .await?;
        require(
            project
                .required("paths")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .any(|value| {
                    value
                        .as_js_string()
                        .is_some_and(|path| policy.same_path(path, &cwd))
                }),
            "target_conflict",
            "The original Codex project registration belongs to a different working directory.",
        )?;
        Ok(IvyJson::object([
            ("kind", kind.into()),
            ("cwd", IvyJson::String(cwd)),
            ("project", project),
        ]))
    }
    pub async fn workspace(
        &self,
        task_key: &JsString,
        requirement: &IvyJson,
        prepare: bool,
        host_id: &JsString,
        service_node_id: &JsString,
        verify_origin: bool,
    ) -> IvyResult<IvyJson> {
        ProjectPolicy::workspace_key(task_key)?;
        let policy = &self.0.policy;
        let root = policy.internal_project_root.clone();
        let task_root = blocking(move || paths::future_path(&root)).await?;
        let kind = text(requirement, "kind")?.to_utf8()?;
        let mut suffix = JsString::from("taskboard-");
        suffix.0.extend_from_slice(&task_key.0);
        let bootstrap_path = if kind == "task_workspace" {
            task_root.clone()
        } else {
            policy.paths.resolve(&[task_root.clone(), suffix])
        };
        require(
            policy.paths.inside(&task_root, &bootstrap_path),
            "target_conflict",
            "Task workspace left the configured task root.",
        )?;
        let mut intended_path = bootstrap_path.clone();
        let mut source_path = None;
        let mut project = IvyJson::Null;
        let mut use_worktree = false;
        if kind == "existing_project" {
            let known = self
                .projects()?
                .into_iter()
                .find(|project| project.get("nativeId") == requirement.get("projectId"))
                .ok_or_else(|| {
                    IvyError::new(
                        "not_found",
                        "Requested Codex project is unavailable on this host.",
                    )
                })?;
            let entries = known
                .required("paths")?
                .as_array()
                .ok_or_else(IvyError::internal)?;
            let selected = requirement
                .get("path")
                .filter(|value| !value.is_null())
                .or_else(|| entries.first())
                .and_then(IvyJson::as_js_string);
            require(
                selected.is_some_and(|selected| {
                    !selected.0.is_empty()
                        && entries.iter().any(|value| {
                            value
                                .as_js_string()
                                .is_some_and(|path| policy.same_path(path, selected))
                        })
                }),
                "target_conflict",
                "Requested project path does not belong to the selected project.",
            )?;
            let selected = selected.unwrap().clone();
            let source = blocking(move || {
                let source = realpath(&selected)?;
                access(&source)?;
                Ok(source)
            })
            .await?;
            use_worktree = requirement.get("useWorktree") == Some(&IvyJson::Bool(true));
            intended_path = if use_worktree {
                let mut name = JsString::from("taskboard-worktree-");
                name.0.extend_from_slice(&task_key.0);
                policy.paths.resolve(&[task_root.clone(), name])
            } else {
                source.clone()
            };
            source_path = Some(source);
            project = reference(service_node_id, text(&known, "nativeId")?);
        } else if kind == "repository_path" || kind == "new_project_path" {
            let folder = text(requirement, "folderName")?;
            ProjectPolicy::workspace_folder(folder)?;
            let root = policy.project_root.clone();
            let project_root = blocking(move || paths::future_path(&root)).await?;
            intended_path = policy
                .paths
                .resolve(&[project_root.clone(), folder.clone()]);
            require(
                policy.paths.inside(&project_root, &intended_path),
                "target_conflict",
                "Requested project path left projectRoot.",
            )?;
        } else if kind == "directory_path" {
            let path = text(requirement, "path")?.clone();
            require(
                node_path::is_absolute(policy.paths.style, &path),
                "invalid_arguments",
                "Selected directory must be absolute.",
            )?;
            intended_path = blocking(move || {
                let path = realpath(&path)?;
                require(
                    internal(directory(&path))?,
                    "target_conflict",
                    "Selected workspace must be a directory.",
                )?;
                access(&path)?;
                Ok(path)
            })
            .await?;
        }
        if prepare {
            if kind == "task_workspace" {
                self.resolve(
                    &IvyJson::object([
                        ("kind", "internal".into()),
                        ("key", "taskboard".into()),
                        ("name", "IvyInternal".into()),
                    ]),
                    &"task-board".into(),
                    None,
                )
                .await?;
            } else if use_worktree {
                if let Some(source) = &source_path {
                    let root = task_root.clone();
                    let intended = intended_path.clone();
                    let prepare_policy = policy.clone();
                    blocking(move || {
                        internal(mkdir(&root, true))?;
                        require(
                            prepare_policy
                                .paths
                                .inside(&realpath(&root)?, &paths::future_path(&intended)?),
                            "target_conflict",
                            "Worktree path left internalProjectRoot.",
                        )
                    })
                    .await?;
                    let intended = intended_path.clone();
                    let present = blocking(move || match directory(&intended) {
                        Ok(value) => Ok(value),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                        Err(_) => Err(IvyError::internal()),
                    })
                    .await?;
                    if !present {
                        self.0.git.common(source, &policy.paths).await?;
                        self.0.git.add_worktree(source, &intended_path).await?;
                    }
                    let intended = intended_path.clone();
                    let actual = blocking(move || realpath(&intended)).await?;
                    require(
                        actual == intended_path
                            && self.0.git.common(source, &policy.paths).await?
                                == self.0.git.common(&intended_path, &policy.paths).await?,
                        "target_conflict",
                        "Dedicated worktree does not belong to the selected repository.",
                    )?;
                }
            } else if kind == "repository_path" || kind == "new_project_path" {
                let policy = policy.clone();
                let intended = intended_path.clone();
                blocking(move || {
                    internal(mkdir(&policy.project_root, true))?;
                    require(
                        policy.paths.inside(
                            &realpath(&policy.project_root)?,
                            &paths::future_path(&intended)?,
                        ),
                        "target_conflict",
                        "Project path left projectRoot.",
                    )?;
                    internal(mkdir(&intended, true))?;
                    require(
                        realpath(&intended)? == intended,
                        "target_conflict",
                        "Requested project path was redirected.",
                    )
                })
                .await?;
            }
        }
        let repository = self
            .0
            .git
            .inspect_workspace(
                &intended_path,
                source_path.as_ref(),
                use_worktree,
                verify_origin,
            )
            .await?
            .unwrap_or(IvyJson::Null);
        let id = policy.workspace_project_id(
            &self.projects()?,
            &intended_path,
            project.get("nativeId").and_then(IvyJson::as_js_string),
        )?;
        let id = id.filter(|value| !value.0.is_empty());
        if let Some(id) = &id {
            project = reference(service_node_id, id);
        }
        let mut result = IvyJson::object([
            ("hostId", IvyJson::String(host_id.clone())),
            ("serviceNodeId", IvyJson::String(service_node_id.clone())),
            ("canonicalCwd", IvyJson::String(intended_path.clone())),
            ("taskRoot", IvyJson::String(task_root)),
            ("bootstrapPath", IvyJson::String(bootstrap_path)),
            ("intendedPath", IvyJson::String(intended_path)),
            (
                "sourcePath",
                source_path.map(IvyJson::String).unwrap_or(IvyJson::Null),
            ),
            ("project", project),
            ("useWorktree", use_worktree.into()),
            ("repository", repository),
        ]);
        if let Some(id) = id {
            result.insert("nativeProjectId", IvyJson::String(id));
        }
        Ok(result)
    }
}
