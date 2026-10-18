use ivy_agent_manager::{project_policy::ProjectPolicy, project_repository::WorkspaceGit};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::{
    node_path::{self, Context, Style},
    paths,
};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

const FRAME: usize = 4 * 1024 * 1024;
#[path = "agent-project-locations.rs"]
mod project_locations_cases;
fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> JsString {
    field(value, key).as_js_string().unwrap().clone()
}
fn optional_text<'a>(value: &'a IvyJson, key: &str) -> Option<&'a JsString> {
    value.get(key).and_then(IvyJson::as_js_string)
}
fn oracle(file: &str, input: Option<&IvyJson>) -> IvyJson {
    let mut child = Command::new("node")
        .arg(repository().join("tests/fixtures").join(file))
        .current_dir(repository())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stringify(input, FRAME).unwrap().as_bytes())
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(std::str::from_utf8(&output.stdout).unwrap(), FRAME).unwrap()
}
fn compare(actual: &IvyJson, expected: &IvyJson, label: &str) {
    assert_eq!(
        canonical(actual, FRAME).unwrap(),
        canonical(expected, FRAME).unwrap(),
        "{label}"
    );
}
fn context(row: &IvyJson) -> Context {
    Context {
        style: if text(row, "style") == "win32".into() {
            Style::Windows
        } else {
            Style::Posix
        },
        cwd: text(row, "cwd"),
        drive_cwds: BTreeMap::new(),
    }
}

#[test]
fn staged_names_preserve_original_policy_with_the_agreed_reserved_prefix_cap() {
    let data = oracle("agent-status-staged-oracle.mjs", None);
    let names = field(&data, "names").as_array().unwrap();
    assert_eq!(names.len(), 90);
    let mut exceptions = 0;
    for row in names {
        let mut expected = text(row, "result");
        if expected.0.len() == 129 {
            // Explicit parent agreement: truncate only the reserved-prefix
            // overflow; schemas and existing paths remain unchanged.
            expected.0.truncate(128);
            exceptions += 1;
        }
        assert_eq!(
            ivy_agent_manager::staged_files::staged_name(&text(row, "name")),
            expected
        );
    }
    assert_eq!(exceptions, 2);
}

#[test]
fn project_policy_preserves_original_guards_hashes_path_membership_and_selection() {
    let data = oracle("agent-project-policy-oracle.mjs", None);
    let rows = field(&data, "cases").as_array().unwrap();
    assert_eq!(rows.len(), 352);
    for (index, row) in rows.iter().enumerate() {
        let input = field(row, "input");
        let action = text(row, "action").to_utf8().unwrap();
        let actual: IvyResult<IvyJson> = (|| {
            let policy = ProjectPolicy::new(
                text(row, "data"),
                input.get("settings").unwrap_or(&IvyJson::Object(vec![])),
                &[],
                context(row),
                &text(row, "home"),
            )?;
            match action.as_str() {
                "defaults" => Ok(policy.defaults()),
                "samePath" => Ok(policy
                    .same_path(&text(input, "left"), &text(input, "right"))
                    .into()),
                "taskKey" => ProjectPolicy::task_key(&text(input, "key")).map(|_| IvyJson::Null),
                "workspaceKey" => {
                    ProjectPolicy::workspace_key(&text(input, "key")).map(|_| IvyJson::Null)
                }
                "projectName" => policy
                    .project_name(&text(input, "name"))
                    .map(|_| IvyJson::Null),
                "workspaceFolder" => {
                    ProjectPolicy::workspace_folder(&text(input, "name")).map(|_| IvyJson::Null)
                }
                "allocationKey" => ProjectPolicy::allocation_key(
                    &text(input, "principal"),
                    field(input, "selection"),
                )
                .map(IvyJson::from),
                "projectlessPath" => policy
                    .projectless_path(&text(input, "principal"), field(input, "key"))
                    .map(IvyJson::String),
                "internalProject" => policy.internal_project(
                    field(input, "projects").as_array().unwrap(),
                    optional_text(input, "expectedProjectId"),
                ),
                "existingProject" => policy
                    .existing_project(
                        field(input, "projects").as_array().unwrap(),
                        &text(input, "cwd"),
                        optional_text(input, "expectedProjectId"),
                    )
                    .map(|value| value.unwrap_or(IvyJson::Null)),
                "workspaceProject" => policy
                    .workspace_project_id(
                        field(input, "projects").as_array().unwrap(),
                        &text(input, "cwd"),
                        optional_text(input, "explicit"),
                    )
                    .map(|value| value.map(IvyJson::String).unwrap_or(IvyJson::Null)),
                _ => panic!("Unexpected original policy action {action}"),
            }
        })();
        let actual = match actual {
            Ok(value) => IvyJson::object([("result", value)]),
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        compare(
            &actual,
            field(row, "expected"),
            &format!("{index}: {action}"),
        );
    }
}

struct Root {
    path: PathBuf,
    parent: PathBuf,
}
impl Root {
    fn new() -> Self {
        let parent = paths::realpath(&std::env::temp_dir()).unwrap();
        let path = parent.join(format!("ivy-workspace-Grüße😀-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self {
            path: paths::realpath(&path).unwrap(),
            parent,
        }
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        assert!(
            self.path.is_absolute()
                && self.path.parent() == Some(self.parent.as_path())
                && self
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ivy-workspace-")
        );
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
fn value(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn job() -> Option<PathBuf> {
    cfg!(windows).then(|| {
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ivy-job.exe")
    })
}
fn git_oracle(input: IvyJson) -> IvyJson {
    oracle("agent-project-git-oracle.mjs", Some(&input))
}

#[tokio::test]
async fn directory_browsing_matches_original_filesystem_errors_links_and_stable_locale_ties() {
    let root = Root::new();
    let prepared = oracle(
        "agent-directories-oracle.mjs",
        Some(&IvyJson::object([
            ("action", "prepare".into()),
            ("root", value(&root.path)),
        ])),
    );
    let pairs = field(&prepared, "pairs").as_array().unwrap().clone();
    // Explicit synthetic comparator injection until the shared locale operation
    // is available. There is no ordinal fallback in the production module.
    let compare_names: Arc<ivy_agent_manager::directories::DirectoryComparator> =
        Arc::new(move |left, right| {
            let pair = pairs
                .iter()
                .find(|pair| {
                    optional_text(pair, "left") == Some(left)
                        && optional_text(pair, "right") == Some(right)
                })
                .expect("Unexpected directory in synthetic locale fixture");
            field(pair, "result")
                .as_f64()
                .unwrap()
                .partial_cmp(&0.0)
                .unwrap()
        });
    for path in field(&prepared, "paths").as_array().unwrap() {
        let path = path.as_js_string().unwrap();
        let expected = oracle(
            "agent-directories-oracle.mjs",
            Some(&IvyJson::object([("path", IvyJson::String(path.clone()))])),
        );
        let actual =
            match ivy_agent_manager::directories::list_directories(path, compare_names.clone())
                .await
            {
                Ok(value) => IvyJson::object([("result", value)]),
                Err(error) => IvyJson::object([("error", error.to_wire())]),
            };
        compare(
            &actual,
            &expected,
            &path
                .to_utf8()
                .unwrap_or_else(|_| "unpaired surrogate".into()),
        );
    }
}

#[tokio::test]
async fn workspace_git_matches_real_original_local_repository_observations() {
    let root = Root::new();
    let mut prepared = Vec::new();
    let mut urls = vec![
        IvyJson::from(""),
        IvyJson::from("https://fixture:secret@example.invalid/path.git"),
    ];
    for variant in [
        "missing",
        "file",
        "empty",
        "unborn",
        "clean",
        "dirty",
        "detached",
        "credentials",
        "confirmed",
        "local-only",
        "ancestor",
        "missing-ref",
        "unavailable",
        "detached-origin",
    ] {
        let input = git_oracle(IvyJson::object([
            ("action", "prepare".into()),
            ("root", value(&root.path.join(variant))),
            ("variant", variant.into()),
        ]));
        urls.push(value(&root.path.join(variant).join("origin.git")));
        urls.push(value(&root.path.join(variant).join("missing-origin.git")));
        prepared.push((variant, input));
    }
    // This synthetic pair table qualifies only our Git consumer. The production
    // URL operation is supplied by the shared contracts owner, never this table.
    let pairs = git_oracle(IvyJson::object([
        ("action", "redactions".into()),
        ("urls", IvyJson::Array(urls)),
    ]));
    let pairs = field(&IvyJson::object([("pairs", pairs)]), "pairs")
        .as_array()
        .unwrap()
        .clone();
    let git = WorkspaceGit {
        bootstrap_root: root.path.clone(),
        job_launcher: job(),
        redact_origin: Arc::new(move |input| {
            let row = pairs
                .iter()
                .find(|row| optional_text(row, "input") == Some(input))
                .expect("Unexpected URL in synthetic fixture");
            optional_text(row, "result")
                .cloned()
                .ok_or_else(IvyError::internal)
        }),
        clock: Arc::new(|| "2026-10-18T00:00:00.000Z".into()),
    };
    for (variant, input) in &prepared {
        let path = text(input, "path");
        let verify = field(input, "verifyOrigin") == &IvyJson::Bool(true);
        let actual = git
            .inspect(&path, verify)
            .await
            .unwrap()
            .unwrap_or(IvyJson::Null);
        let expected = git_oracle(input.clone());
        compare(&actual, &expected, variant);
        if ["confirmed", "local-only", "ancestor", "detached-origin"].contains(variant) {
            assert_eq!(
                git.common(&path, &Context::current().unwrap())
                    .await
                    .unwrap(),
                node_path::from_native(
                    &paths::realpath(&node_path::to_native(&path).join(".git")).unwrap()
                )
            );
        }
    }
    let source = text(
        &prepared
            .iter()
            .find(|(variant, _)| *variant == "confirmed")
            .unwrap()
            .1,
        "path",
    );
    for verify in [false, true] {
        for use_worktree in [false, true] {
            for intended in [
                source.clone(),
                node_path::from_native(&root.path.join("absent-worktree")),
            ] {
                let expected = git_oracle(IvyJson::object([
                    ("action", "inspectWorkspace".into()),
                    ("path", IvyJson::String(intended.clone())),
                    ("source", IvyJson::String(source.clone())),
                    ("useWorktree", use_worktree.into()),
                    ("verifyOrigin", verify.into()),
                ]));
                let actual = git
                    .inspect_workspace(&intended, Some(&source), use_worktree, verify)
                    .await
                    .unwrap()
                    .unwrap_or(IvyJson::Null);
                compare(
                    &actual,
                    &expected,
                    "original workspace source/intended selection and fallback",
                );
            }
        }
    }
}
