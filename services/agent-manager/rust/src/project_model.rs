//! One immutable native project set, shared by inventory and local consumers.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent};
use ivy_sdk::native_project_membership::normalized_project_path;
use std::{collections::HashSet, mem::size_of, sync::Arc};

#[derive(Debug)]
pub struct NativeProject {
    // Native identities, names and paths remain lossless at this boundary.
    pub native_id: JsString,
    pub name: JsString,
    pub paths: Box<[JsString]>,
    pub position: u64,
    pub recency_at: Option<u64>,
}
impl NativeProject {
    pub fn from_summary(value: &IvyJson) -> IvyResult<Self> {
        validate_agent("ProjectSummary", value)?;
        let text = |key| {
            value
                .required(key)?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)
        };
        Ok(Self {
            native_id: text("nativeId")?,
            name: text("name")?,
            paths: value
                .required("paths")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(|path| path.as_js_string().cloned().ok_or_else(IvyError::internal))
                .collect::<IvyResult<_>>()?,
            position: value
                .required("position")?
                .as_f64()
                .ok_or_else(IvyError::internal)? as u64,
            recency_at: value.required("recencyAt")?.as_f64().map(|v| v as u64),
        })
    }
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("nativeId", IvyJson::String(self.native_id.clone())),
            ("source", "native".into()),
            ("name", IvyJson::String(self.name.clone())),
            (
                "paths",
                IvyJson::Array(self.paths.iter().cloned().map(IvyJson::String).collect()),
            ),
            ("position", IvyJson::Number(self.position as f64)),
            (
                "recencyAt",
                self.recency_at
                    .map(|v| IvyJson::Number(v as f64))
                    .unwrap_or(IvyJson::Null),
            ),
        ])
    }
    pub(crate) fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + (self.native_id.0.capacity() + self.name.0.capacity()) * 2
            + self.paths.len() * size_of::<JsString>()
            + self
                .paths
                .iter()
                .map(|path| path.0.capacity() * 2)
                .sum::<usize>()
    }
}

#[derive(Debug, Default)]
pub struct NativeProjects {
    entries: Box<[NativeProject]>,
}
impl NativeProjects {
    pub fn from_summaries(values: &[IvyJson]) -> IvyResult<Arc<Self>> {
        let entries = values
            .iter()
            .map(NativeProject::from_summary)
            .collect::<IvyResult<Vec<_>>>()?;
        Self::new(entries)
    }
    pub(crate) fn new(entries: Vec<NativeProject>) -> IvyResult<Arc<Self>> {
        let mut ids = HashSet::new();
        if entries.len() > 256 {
            return Err(IvyError::new(
                "native_inventory_capacity",
                "Native project inventory exceeds its declared capacity.",
            ));
        }
        if !entries.iter().all(|project| ids.insert(&project.native_id)) {
            return Err(IvyError::new(
                "native_inventory_invalid",
                "Native project inventory contains duplicate IDs.",
            ));
        }
        let result = Arc::new(Self {
            entries: entries.into_boxed_slice(),
        });
        if result.retained_bytes() > 9 * 1024 * 1024 {
            return Err(IvyError::new(
                "native_inventory_capacity",
                "Native project inventory exceeds its retained-memory capacity.",
            ));
        }
        Ok(result)
    }
    pub fn entries(&self) -> &[NativeProject] {
        &self.entries
    }
    pub fn find(&self, id: &JsString) -> Option<&NativeProject> {
        self.entries.iter().find(|project| &project.native_id == id)
    }
    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + 2 * size_of::<usize>() // Arc strong/weak counters.
            + self
                .entries
                .iter()
                .map(NativeProject::retained_bytes)
                .sum::<usize>()
    }
    pub fn for_thread(&self, membership: &ProjectMembership) -> Option<&NativeProject> {
        if let Some(id) = &membership.project_id {
            return id.as_ref().and_then(|id| self.find(id));
        }
        let child = normalized_project_path(membership.cwd.as_ref()?);
        let mut selected = None;
        let mut length = 0;
        for project in &self.entries {
            for path in &project.paths {
                let root = normalized_project_path(path);
                let matches = child == root
                    || child.0.starts_with(&root.0) && child.0.get(root.0.len()) == Some(&47);
                if matches && (selected.is_none() || root.0.len() > length) {
                    selected = Some(project);
                    length = root.0.len();
                }
            }
        }
        selected
    }
}

#[derive(Default)]
pub struct ProjectMembership {
    pub cwd: Option<JsString>,
    // Missing is path inference; present null/empty/invalid explicitly has no project.
    pub project_id: Option<Option<JsString>>,
}
impl ProjectMembership {
    pub fn from_thread(thread: &IvyJson) -> Self {
        Self {
            cwd: thread.get("cwd").and_then(IvyJson::as_js_string).cloned(),
            project_id: thread.get("projectId").map(Self::project_id),
        }
    }
    pub fn project_id(value: &IvyJson) -> Option<JsString> {
        value.as_js_string().filter(|id| !id.0.is_empty()).cloned()
    }
}

#[derive(Clone)]
pub struct ProjectSnapshot {
    pub observed_at: JsString,
    pub projects: Arc<NativeProjects>,
}
impl ProjectSnapshot {
    pub fn projects_result(&self) -> IvyJson {
        IvyJson::object([
            ("source", "native".into()),
            ("observedAt", IvyJson::String(self.observed_at.clone())),
            (
                "projects",
                IvyJson::Array(
                    self.projects
                        .entries()
                        .iter()
                        .map(NativeProject::to_json)
                        .collect(),
                ),
            ),
        ])
    }
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("observedAt", IvyJson::String(self.observed_at.clone())),
            ("projects", self.projects_result()),
        ])
    }
}
