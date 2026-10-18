//! Project allocation policy. Filesystem identity and Git effects are separate.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_host_runtime::node_path::{self, Context, Style};

pub struct ProjectPolicy {
    pub project_root: JsString,
    pub internal_project_root: JsString,
    pub protected_roots: Vec<JsString>,
    pub paths: Context,
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn paths(project: &IvyJson) -> IvyResult<Vec<&JsString>> {
    project
        .required("paths")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|value| value.as_js_string().ok_or_else(IvyError::internal))
        .collect()
}
fn truthy_id(value: Option<&JsString>) -> Option<&JsString> {
    value.filter(|value| !value.0.is_empty())
}

impl ProjectPolicy {
    pub fn new(
        data_root: JsString,
        settings: &IvyJson,
        protected_roots: &[JsString],
        paths: Context,
        account_home: &JsString,
    ) -> IvyResult<Self> {
        if settings.is_null() {
            return Err(IvyError::internal());
        }
        let select = |key: &str, components: &[JsString]| -> IvyResult<JsString> {
            match settings.get(key).filter(|value| !value.is_null()) {
                Some(value) => value.as_js_string().cloned().ok_or_else(IvyError::internal),
                None => Ok(node_path::join(paths.style, components)),
            }
        };
        let project_root = select("projectRoot", &[account_home.clone(), "projects".into()])?;
        let internal_project_root = select(
            "internalProjectRoot",
            &[account_home.clone(), ".ivy".into(), "codex-projects".into()],
        )?;
        require(
            [&project_root, &internal_project_root]
                .into_iter()
                .all(|value| node_path::is_absolute(paths.style, value)),
            "invalid_arguments",
            "Project defaults must be absolute directories on the target host.",
        )?;
        Ok(Self {
            project_root,
            internal_project_root,
            protected_roots: std::iter::once(data_root)
                .chain(protected_roots.iter().cloned())
                .collect(),
            paths,
        })
    }
    pub fn defaults(&self) -> IvyJson {
        IvyJson::object([
            ("projectRoot", IvyJson::String(self.project_root.clone())),
            (
                "internalProjectRoot",
                IvyJson::String(self.internal_project_root.clone()),
            ),
        ])
    }
    pub fn same_path(&self, left: &JsString, right: &JsString) -> bool {
        let left = self.paths.resolve(std::slice::from_ref(left));
        let right = self.paths.resolve(std::slice::from_ref(right));
        if self.paths.style == Style::Windows {
            left.to_english_lowercase() == right.to_english_lowercase()
        } else {
            left == right
        }
    }
    pub fn projectless_path(&self, principal: &JsString, key: &IvyJson) -> IvyResult<JsString> {
        let hash = hash_json(&IvyJson::object([
            ("principal", IvyJson::String(principal.clone())),
            ("key", key.clone()),
        ]))?;
        Ok(node_path::join(
            self.paths.style,
            &[
                self.internal_project_root.clone(),
                "unassigned".into(),
                hash[7..39].into(),
            ],
        ))
    }
    pub fn task_key(key: &JsString) -> IvyResult<()> {
        let alnum = |unit: &u16| matches!(*unit, 48..=57 | 65..=90 | 97..=122);
        require(
            (1..=128).contains(&key.0.len())
                && key.0.first().is_some_and(alnum)
                && key
                    .0
                    .iter()
                    .all(|unit| alnum(unit) || matches!(*unit, 45 | 46 | 95)),
            "invalid_arguments",
            "Task directory key must be one safe relative directory name.",
        )
    }
    pub fn workspace_key(key: &JsString) -> IvyResult<()> {
        require(
            key.0.starts_with(&[84, 65, 83, 75, 45])
                && key.0.len() >= 9
                && key.0[5..].iter().all(|unit| (48..=57).contains(unit)),
            "invalid_arguments",
            "Task workspace requires its immutable Task key.",
        )
    }
    pub fn project_name(&self, name: &JsString) -> IvyResult<()> {
        let first: Vec<_> = name
            .0
            .iter()
            .copied()
            .take_while(|unit| *unit != 46)
            .map(|unit| {
                if (65..=90).contains(&unit) {
                    unit + 32
                } else {
                    unit
                }
            })
            .collect();
        let reserved = ["con", "prn", "aux", "nul"]
            .iter()
            .any(|value| first.iter().copied().eq(value.encode_utf16()))
            || first.len() == 4
                && (first.starts_with(&[99, 111, 109]) || first.starts_with(&[108, 112, 116]))
                && (49..=57).contains(&first[3]);
        require(
            !name.trim().0.is_empty()
                && !name.0.iter().any(|unit| matches!(*unit, 0..=31 | 47 | 92))
                && name != &JsString::from(".")
                && name != &JsString::from("..")
                && (self.paths.style != Style::Windows
                    || !name
                        .0
                        .iter()
                        .any(|unit| matches!(*unit, 34 | 42 | 58 | 60 | 62 | 63 | 124))
                        && !matches!(name.0.last(), Some(32 | 46))
                        && !reserved),
            "invalid_arguments",
            "Project name must be one valid directory name on this host.",
        )
    }
    pub fn workspace_folder(name: &JsString) -> IvyResult<()> {
        require(
            !name.0.iter().any(|unit| matches!(*unit, 47 | 92))
                && name != &JsString::from(".")
                && name != &JsString::from(".."),
            "invalid_arguments",
            "Project folder must be one relative directory name.",
        )
    }
    pub fn allocation_key(principal: &JsString, selection: &IvyJson) -> IvyResult<String> {
        hash_json(&IvyJson::object([
            ("principal", IvyJson::String(principal.clone())),
            ("selection", selection.clone()),
        ]))
    }
    pub fn internal_project(
        &self,
        projects: &[IvyJson],
        expected: Option<&JsString>,
    ) -> IvyResult<IvyJson> {
        for project in projects {
            if paths(project)?
                .into_iter()
                .any(|path| self.same_path(path, &self.internal_project_root))
            {
                if truthy_id(expected).is_none_or(|expected| {
                    project.get("nativeId").and_then(IvyJson::as_js_string) == Some(expected)
                }) {
                    return Ok(project.clone());
                }
                break;
            }
        }
        Err(IvyError::new(
            "native_internal_project_missing",
            "Register the internal working directory as a Codex project before starting service tasks.",
        ))
    }
    pub fn existing_project(
        &self,
        projects: &[IvyJson],
        cwd: &JsString,
        expected: Option<&JsString>,
    ) -> IvyResult<Option<IvyJson>> {
        let expected = truthy_id(expected);
        for project in projects {
            if expected.is_none_or(|expected| {
                project.get("nativeId").and_then(IvyJson::as_js_string) == Some(expected)
            }) && paths(project)?
                .into_iter()
                .any(|path| self.same_path(path, cwd))
            {
                return Ok(Some(project.clone()));
            }
        }
        require(
            expected.is_none(),
            "target_conflict",
            "The selected Codex project no longer belongs to this working directory.",
        )?;
        Ok(None)
    }
    /// Exact workspace rule: longest *resolved* UTF-16 path, stable on ties.
    /// This is intentionally different from thread membership normalization.
    pub fn workspace_project_id(
        &self,
        projects: &[IvyJson],
        cwd: &JsString,
        explicit: Option<&JsString>,
    ) -> IvyResult<Option<JsString>> {
        if let Some(explicit) = explicit {
            return Ok(Some(explicit.clone()));
        }
        let mut selected = None;
        let mut length = 0;
        for project in projects {
            for path in paths(project)? {
                let path = self.paths.resolve(std::slice::from_ref(path));
                if self.paths.inside(&path, cwd) && (selected.is_none() || path.0.len() > length) {
                    selected = Some(
                        project
                            .required("nativeId")?
                            .as_js_string()
                            .ok_or_else(IvyError::internal)?
                            .clone(),
                    );
                    length = path.0.len();
                }
            }
        }
        Ok(selected)
    }
}
