use super::{FRAME, Root, compare, field, git_oracle, job, oracle, text, value};
use ivy_agent_manager::{
    local_state::{Limits, LocalAgentState},
    project_locations::{ProjectLocations, RegisterProject},
    project_management::ProjectManagement,
    project_policy::ProjectPolicy,
    project_repository::WorkspaceGit,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::canonical};
use ivy_host_runtime::node_path::{self, Context};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn expand(value: &IvyJson, root: &JsString) -> IvyJson {
    match value {
        IvyJson::String(text) => text
            .to_utf8()
            .unwrap()
            .replace("<root>", &root.to_utf8().unwrap())
            .into(),
        IvyJson::Array(items) => {
            IvyJson::Array(items.iter().map(|value| expand(value, root)).collect())
        }
        IvyJson::Object(items) => IvyJson::object(
            items
                .iter()
                .map(|(key, value)| (key.clone(), expand(value, root))),
        ),
        _ => value.clone(),
    }
}
fn fixture(input: IvyJson) -> IvyJson {
    oracle("agent-project-locations-oracle.mjs", Some(&input))
}
fn open_state(root: &Path) -> Arc<Mutex<LocalAgentState>> {
    Arc::new(Mutex::new(
        LocalAgentState::open(
            &root.join("state.sqlite"),
            "agent".into(),
            Limits {
                max_operations: 1000,
                max_journal_bytes: 128 * 1024 * 1024,
            },
        )
        .unwrap(),
    ))
}
fn close_state(state: Option<Arc<Mutex<LocalAgentState>>>) {
    if let Some(state) = state {
        match Arc::try_unwrap(state) {
            Ok(state) => state.into_inner().unwrap().close().unwrap(),
            Err(_) => panic!("Project fixture retained its SQLite owner"),
        }
    }
}

#[tokio::test]
async fn project_handlers_match_original_validation_availability_refresh_and_outer_identity_order()
{
    let root = Root::new();
    let catalog = fixture(IvyJson::object([("action", "handlerCatalog".into())]));
    let rows = catalog.as_array().unwrap();
    assert_eq!(rows.len(), 24);
    for (index, template) in rows.iter().enumerate() {
        let original_root = root.path.join(format!("original-handler-{index}"));
        let expected = fixture(IvyJson::object([
            ("action", "handlers".into()),
            ("root", value(&original_root)),
            ("case", template.clone()),
        ]));
        let rust_root = root.path.join(format!("rust-handler-{index}"));
        fixture(IvyJson::object([
            ("action", "prepare".into()),
            ("root", value(&rust_root)),
            ("case", template.clone()),
        ]));
        let row = expand(template, &node_path::from_native(&rust_root));
        let step = field(&row, "steps").as_array().unwrap()[0].clone();
        let state = open_state(&rust_root);
        let policy = ProjectPolicy::new(
            node_path::from_native(&rust_root.join("data")),
            &IvyJson::object([
                ("projectRoot", value(&rust_root.join("projects"))),
                ("internalProjectRoot", value(&rust_root.join("internal"))),
            ]),
            &[],
            Context::current().unwrap(),
            &node_path::from_native(&rust_root),
        )
        .unwrap();
        let locations = ProjectLocations::new(
            policy,
            None,
            Some(state.clone()),
            Arc::new(WorkspaceGit {
                bootstrap_root: rust_root.clone(),
                job_launcher: job(),
                redact_origin: Arc::new(|_| Err(IvyError::internal())),
                clock: Arc::new(|| "2026-10-18T00:00:00.000Z".into()),
            }),
        );
        locations
            .set_native_projects(&[IvyJson::object([
                ("nativeId", "internal".into()),
                ("name", "internal".into()),
                (
                    "paths",
                    IvyJson::Array(vec![value(&rust_root.join("internal"))]),
                ),
                ("source", "native".into()),
            ])])
            .unwrap();
        let effects = Arc::new(Mutex::new(Vec::<IvyJson>::new()));
        let owner = Arc::new(ProjectManagement {
            locations: locations.clone(),
            host_id: "host".into(),
            service_node_id: "agent".into(),
            available: Arc::new({
                let effects = effects.clone();
                let step = step.clone();
                move || {
                    effects.lock().unwrap().push("available".into());
                    if step.get("available") == Some(&IvyJson::Bool(false)) {
                        Err(IvyError::new(
                            "native_unavailable",
                            "The current native owner is unavailable.",
                        ))
                    } else {
                        Ok(())
                    }
                }
            }),
            fresh_projects: Arc::new({
                let effects = effects.clone();
                let step = step.clone();
                move || {
                    effects.lock().unwrap().push("fresh".into());
                    let fail = step.get("freshError") == Some(&IvyJson::Bool(true));
                    Box::pin(async move {
                        if fail {
                            Err(IvyError::new(
                                "fixture_fresh_failed",
                                "Fixture project refresh failed.",
                            ))
                        } else {
                            Ok(())
                        }
                    })
                }
            }),
            compare_directories: Arc::new(|_, _| {
                panic!("These wrapper cases do not enumerate a valid directory")
            }),
        });
        let handlers = owner.handlers();
        let context = field(&step, "context");
        let context_value = ivy_sdk::service::InvocationContext::detached(
            text(context, "callerPrincipalId"),
            context
                .get("operationId")
                .and_then(IvyJson::as_js_string)
                .cloned(),
            1,
            Default::default(),
            IvyError::new("fixture_cancelled", "Fixture caller cancelled."),
        );
        if context.get("cancelled") == Some(&IvyJson::Bool(true)) {
            context_value.signal.cancel();
        }
        let name = format!("agent.{}", text(&step, "name").to_utf8().unwrap());
        let result = (handlers.get(&JsString::from(name)).unwrap())(
            field(&step, "args").clone(),
            context_value,
        )
        .await;
        let trace = match result {
            Ok(value) => IvyJson::object([("result", value)]),
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        let actual = fixture(IvyJson::object([
            ("action", "normalize".into()),
            ("root", value(&rust_root)),
            (
                "value",
                IvyJson::object([
                    ("trace", IvyJson::Array(vec![trace])),
                    ("calls", IvyJson::Array(Vec::new())),
                    ("effects", IvyJson::Array(effects.lock().unwrap().clone())),
                ]),
            ),
        ]));
        compare(&actual, &expected, &text(&row, "name").to_utf8().unwrap());
        drop(handlers);
        drop(owner);
        locations.close().unwrap();
        drop(locations);
        close_state(Some(state));
    }
}

#[tokio::test]
async fn real_project_allocations_workspaces_and_original_sqlite_reader_preserve_identity() {
    let root = Root::new();
    let catalog = fixture(IvyJson::object([("action", "catalog".into())]));
    let rows = catalog.as_array().unwrap();
    assert_eq!(rows.len(), 17);
    for (index, template) in rows.iter().enumerate() {
        let original_root = root.path.join(format!("original-{index}"));
        let expected = fixture(IvyJson::object([
            ("action", "run".into()),
            ("root", value(&original_root)),
            ("case", template.clone()),
        ]));
        let rust_root = root.path.join(format!("rust-{index}"));
        fixture(IvyJson::object([
            ("action", "prepare".into()),
            ("root", value(&rust_root)),
            ("case", template.clone()),
        ]));
        let row = expand(template, &node_path::from_native(&rust_root));
        let internal = IvyJson::object([
            ("nativeId", "internal".into()),
            ("name", "internal".into()),
            (
                "paths",
                IvyJson::Array(vec![value(&rust_root.join("internal"))]),
            ),
            ("source", "native".into()),
        ]);
        let projects = Arc::new(Mutex::new(
            row.get("projects")
                .and_then(IvyJson::as_array)
                .cloned()
                .unwrap_or_else(|| vec![internal]),
        ));
        let calls = Arc::new(Mutex::new(Vec::<IvyJson>::new()));
        let current = Arc::new(Mutex::new(IvyJson::Null));
        let mut state =
            (row.get("state") != Some(&IvyJson::Bool(false))).then(|| open_state(&rust_root));
        let register: Option<Arc<RegisterProject>> = (row.get("register")
            != Some(&IvyJson::Bool(false)))
        .then(|| {
            let projects = projects.clone();
            let calls = calls.clone();
            let current = current.clone();
            let wrong = row.get("wrongRegistration") == Some(&IvyJson::Bool(true));
            let other = value(&rust_root.join("other"));
            Arc::new(move |request: IvyJson| {
                let projects = projects.clone();
                let calls = calls.clone();
                let current = current.lock().unwrap().clone();
                let other = other.clone();
                Box::pin(async move {
                    let principal = current
                        .get("principal")
                        .and_then(IvyJson::as_js_string)
                        .cloned()
                        .unwrap_or_else(|| "caller".into());
                    let key =
                        ProjectPolicy::allocation_key(&principal, field(&current, "selection"))?;
                    calls.lock().unwrap().push(IvyJson::object([
                        ("name", field(&request, "name").clone()),
                        ("cwd", field(&request, "cwd").clone()),
                        (
                            "idempotencyValid",
                            (request.get("idempotencyKey") == Some(&key.into())).into(),
                        ),
                    ]));
                    let project = IvyJson::object([
                        ("nativeId", "registered".into()),
                        ("name", "registered".into()),
                        (
                            "paths",
                            IvyJson::Array(vec![if wrong {
                                other
                            } else {
                                field(&request, "cwd").clone()
                            }]),
                        ),
                        ("source", "native".into()),
                    ]);
                    projects.lock().unwrap().push(project.clone());
                    Ok(project)
                }) as ivy_sdk::service::ServiceFuture<IvyJson>
            }) as Arc<RegisterProject>
        });
        let git = Arc::new(WorkspaceGit {
            bootstrap_root: rust_root.clone(),
            job_launcher: job(),
            redact_origin: Arc::new(|_| Err(IvyError::internal())),
            clock: Arc::new(|| "2026-10-18T00:00:00.000Z".into()),
        });
        let open = |state| {
            let mut settings = IvyJson::object([
                ("projectRoot", value(&rust_root.join("projects"))),
                ("internalProjectRoot", value(&rust_root.join("internal"))),
            ]);
            if let Some(entries) = row.get("settings").and_then(IvyJson::as_object) {
                for (key, value) in entries {
                    settings.insert(key.clone(), value.clone());
                }
            }
            let protected = row
                .get("protectedRoots")
                .and_then(IvyJson::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| item.as_js_string().unwrap().clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let policy = ProjectPolicy::new(
                node_path::from_native(&rust_root.join("data")),
                &settings,
                &protected,
                Context::current().unwrap(),
                &node_path::from_native(&rust_root),
            )
            .unwrap();
            let locations = ProjectLocations::new(policy, register.clone(), state, git.clone());
            locations
                .set_native_projects(&projects.lock().unwrap())
                .unwrap();
            locations
        };
        let mut locations = Some(open(state.clone()));
        let mut trace = Vec::new();
        for step in field(&row, "steps").as_array().unwrap() {
            *current.lock().unwrap() = step.clone();
            let result: IvyResult<IvyJson> = match text(step, "action").to_utf8().unwrap().as_str()
            {
                "resolve" => {
                    locations
                        .as_ref()
                        .unwrap()
                        .resolve(
                            field(step, "selection"),
                            step.get("principal")
                                .and_then(IvyJson::as_js_string)
                                .unwrap_or(&"caller".into()),
                            step.get("expected").and_then(IvyJson::as_js_string),
                        )
                        .await
                }
                "workspace" => {
                    locations
                        .as_ref()
                        .unwrap()
                        .workspace(
                            &text(step, "taskKey"),
                            field(step, "requirement"),
                            step.get("prepare") == Some(&IvyJson::Bool(true)),
                            &"host".into(),
                            &"agent".into(),
                            step.get("verifyOrigin") == Some(&IvyJson::Bool(true)),
                        )
                        .await
                }
                "state" => Ok(fixture(IvyJson::object([
                    ("action", "readAllocation".into()),
                    ("root", value(&rust_root)),
                    ("step", step.clone()),
                ]))),
                "fs" => {
                    fixture(IvyJson::object([
                        ("action", "mutate".into()),
                        ("root", value(&rust_root)),
                        ("step", step.clone()),
                    ]));
                    Ok(IvyJson::Null)
                }
                "close" => {
                    locations.as_ref().unwrap().close().unwrap();
                    projects.lock().unwrap().clear();
                    Ok(IvyJson::Null)
                }
                "restart" => {
                    let old = locations.take().unwrap();
                    old.close().unwrap();
                    drop(old);
                    close_state(state.take());
                    state = Some(open_state(&rust_root));
                    locations = Some(open(state.clone()));
                    Ok(IvyJson::Null)
                }
                _ => panic!("Unexpected original project action"),
            };
            // The synthetic registration provider publishes its updated inventory
            // before another fixture request, as the production refresh does.
            locations
                .as_ref()
                .unwrap()
                .set_native_projects(&projects.lock().unwrap())
                .unwrap();
            trace.push(match result {
                Ok(value) => IvyJson::object([("result", value)]),
                Err(error) => IvyJson::object([("error", error.to_wire())]),
            });
        }
        let calls = calls.lock().unwrap().clone();
        let actual = fixture(IvyJson::object([
            ("action", "normalize".into()),
            ("root", value(&rust_root)),
            (
                "value",
                IvyJson::object([
                    ("trace", IvyJson::Array(trace)),
                    ("calls", IvyJson::Array(calls)),
                ]),
            ),
        ]));
        compare(&actual, &expected, &text(&row, "name").to_utf8().unwrap());
        let owner = locations.take().unwrap();
        owner.close().unwrap();
        drop(owner);
        close_state(state.take());
    }
    // Actual worktree creation and repeated ownership verification, with the
    // same source repository observed by both implementations.
    let prepared = git_oracle(IvyJson::object([
        ("action", "prepare".into()),
        ("root", value(&root.path.join("worktree"))),
        ("variant", "clean".into()),
    ]));
    let source = text(&prepared, "path");
    let policy = ProjectPolicy::new(
        node_path::from_native(&root.path.join("data")),
        &IvyJson::object([
            ("projectRoot", value(&root.path.join("projects"))),
            ("internalProjectRoot", value(&root.path.join("internal"))),
        ]),
        &[],
        Context::current().unwrap(),
        &node_path::from_native(&root.path),
    )
    .unwrap();
    let locations = ProjectLocations::new(
        policy,
        None,
        None,
        Arc::new(WorkspaceGit {
            bootstrap_root: root.path.clone(),
            job_launcher: job(),
            redact_origin: Arc::new(|_| Err(IvyError::internal())),
            clock: Arc::new(|| "2026-10-18T00:00:00.000Z".into()),
        }),
    );
    locations
        .set_native_projects(&[IvyJson::object([
            ("nativeId", "known".into()),
            (
                "paths",
                IvyJson::Array(vec![IvyJson::String(source.clone())]),
            ),
        ])])
        .unwrap();
    let requirement = IvyJson::object([
        ("kind", "existing_project".into()),
        ("projectId", "known".into()),
        ("useWorktree", true.into()),
    ]);
    let mut first = None;
    for prepare in [false, true, true] {
        let actual = locations
            .workspace(
                &"TASK-0012".into(),
                &requirement,
                prepare,
                &"host".into(),
                &"agent".into(),
                true,
            )
            .await
            .unwrap();
        assert_eq!(actual.get("nativeProjectId"), Some(&"known".into()));
        let observed = field(&actual, "repository");
        assert!(!observed.is_null());
        if prepare {
            let cwd = text(&actual, "canonicalCwd");
            assert!(node_path::to_native(&cwd).is_dir());
            let common = git_oracle(IvyJson::object([
                ("path", IvyJson::String(cwd)),
                ("verifyOrigin", true.into()),
            ]));
            compare(
                observed,
                &common,
                "actual prepared worktree matches original repository observation",
            );
            let encoded = canonical(&actual, FRAME).unwrap();
            if let Some(first) = &first {
                assert_eq!(&encoded, first);
            } else {
                first = Some(encoded);
            }
        } else {
            assert_eq!(field(observed, "limitation"), &"The intended dedicated worktree had no Git repository; the selected source repository was observed instead.".into());
        }
    }
}
