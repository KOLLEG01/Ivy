//! Inventory and push metadata used to avoid duplicate internal-task notices.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::native_project_membership::{is_internal_project, native_project_for_thread};
use std::collections::{HashMap, HashSet};

static NULL: IvyJson = IvyJson::Null;

struct Thread {
    value: IvyJson,
    revision: f64,
}
#[derive(Default)]
pub struct NativeNoticePolicy {
    projects: Vec<IvyJson>,
    threads: HashMap<JsString, Thread>,
    revision: f64,
}
impl NativeNoticePolicy {
    pub fn revision(&self) -> f64 {
        self.revision
    }
    pub fn set_projects(&mut self, projects: Vec<IvyJson>) {
        self.projects = projects;
    }
    pub fn set_thread(&mut self, id: JsString, thread: &IvyJson, revision: Option<f64>) {
        let revision = revision.unwrap_or_else(|| {
            self.revision += 1.0;
            self.revision
        });
        if self
            .threads
            .get(&id)
            .is_some_and(|value| value.revision > revision)
        {
            return;
        }
        let mut value = IvyJson::object([] as [(&str, IvyJson); 0]);
        if let Some(cwd) = thread
            .get("cwd")
            .filter(|value| value.as_js_string().is_some())
        {
            value.insert("cwd", cwd.clone());
        }
        if let Some(project) = thread.get("projectId") {
            value.insert("projectId", project.clone());
        }
        self.threads.insert(id, Thread { value, revision });
    }
    pub fn set_threads(&mut self, entries: &[IvyJson], revision: f64) -> IvyResult<()> {
        let ids = entries
            .iter()
            .map(|entry| {
                entry
                    .required("nativeId")?
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)
            })
            .collect::<IvyResult<HashSet<_>>>()?;
        self.threads
            .retain(|id, value| ids.contains(id) || !(value.revision <= revision));
        for entry in entries {
            self.set_thread(
                entry.required("nativeId")?.as_js_string().unwrap().clone(),
                entry.get("summary").unwrap_or(&IvyJson::Null),
                Some(revision),
            );
        }
        Ok(())
    }
    pub fn observe(&mut self, event: &IvyJson) {
        let params = event.get("params").unwrap_or(&NULL);
        let thread = params.get("thread").unwrap_or(&NULL);
        let id = params.get("threadId").and_then(IvyJson::as_js_string);
        if event.get("method") == Some(&"thread/started".into()) {
            if let Some(id) = thread.get("id").and_then(IvyJson::as_js_string) {
                self.set_thread(id.clone(), thread, None);
            }
        }
        if event.get("method") == Some(&"thread/project/updated".into()) {
            if let (Some(id), Some(project)) = (id, params.get("projectId")) {
                let mut thread = self
                    .threads
                    .get(id)
                    .map(|value| value.value.clone())
                    .unwrap_or_else(|| IvyJson::object([] as [(&str, IvyJson); 0]));
                thread.insert("projectId", project.clone());
                self.set_thread(id.clone(), &thread, None);
            }
        }
        if event.get("method") == Some(&"thread/deleted".into()) {
            if let Some(id) = id {
                self.threads.remove(id);
            }
        }
    }
    pub fn is_internal(&self, id: &JsString) -> IvyResult<Option<bool>> {
        let Some(thread) = self.threads.get(id) else {
            return Ok(None);
        };
        Ok(Some(
            native_project_for_thread(&self.projects, &thread.value)?
                .as_ref()
                .is_some_and(is_internal_project),
        ))
    }
}
