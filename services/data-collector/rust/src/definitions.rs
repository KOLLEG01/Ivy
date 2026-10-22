//! Central desired definitions and accepted local snapshots. Only control-plane
//! mutations serialize; peer handoff reads must remain possible during a write.
use crate::{
    adoption::{self, Adoption},
    definition_journal::{DefinitionJournal, content, id, pin, require, revision},
    engine::Engine,
    model, policy, schema,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, stringify},
};
use ivy_sdk::{rpc::RequestOptions, service::InvocationContext, tools};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub type Fingerprint = Arc<dyn Fn(&str) -> IvyResult<Option<String>> + Send + Sync>;
#[derive(Clone)]
pub struct Definitions(Arc<Inner>);
struct Inner {
    service_node_id: String,
    engine: Engine,
    fingerprint: Fingerprint,
    journal: Mutex<Option<DefinitionJournal>>,
    serial: tokio::sync::Mutex<()>,
    errors: Mutex<HashMap<String, String>>,
    observations: Mutex<Option<Vec<IvyJson>>>,
}
fn value(document: &IvyJson, name: &str) -> IvyResult<IvyJson> {
    schema::parse_schema(name, content(document)?)
}
fn js(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn number(value: f64) -> IvyJson {
    IvyJson::Number(value)
}
fn defined_assignment(mut value: IvyJson) -> IvyJson {
    if value.get("assignment").is_some_and(IvyJson::is_null) {
        value.remove("assignment");
    }
    value
}
fn nullish(value: Option<&IvyJson>) -> Option<IvyJson> {
    value.filter(|v| !v.is_null()).cloned()
}
fn append(operation: &JsString, suffix: &str) -> JsString {
    let mut v = operation.0.clone();
    v.extend(suffix.encode_utf16());
    JsString(v)
}
fn handoff_row(
    store: &crate::store::CollectorStore,
    engine: &Engine,
    job: &IvyJson,
    expected: f64,
) -> IvyResult<IvyJson> {
    let row = store
        .list(true)?
        .into_iter()
        .find(|r| r.get("definition").and_then(|d| d.get("jobId")) == Some(job));
    let valid = if let Some(row) = &row {
        let task = model::field(row, "task")?;
        let task_id = model::text(task, "id")?;
        let unknown = match model::optional_sqlite_key(row, "lastRunId") {
            Some(id) => store
                .run(&id)?
                .is_some_and(|r| r.get("error") == Some(&"outcome_unknown".into())),
            None => false,
        };
        row.get("definition")
            .and_then(|d| d.get("assignmentRevision"))
            .and_then(IvyJson::as_f64)
            == Some(expected)
            && !model::truth(task, "enabled")
            && !store.busy(&task_id)?
            && !engine.active(&task_id)
            && !unknown
    } else {
        false
    };
    require(
        valid,
        "outcome_unknown",
        "Handoff requires verified quiescence and a drained publication queue.",
    )?;
    Ok(row.unwrap())
}
impl Definitions {
    pub fn new(service_node_id: String, engine: Engine, fingerprint: Fingerprint) -> Self {
        Self(Arc::new(Inner {
            service_node_id,
            engine,
            fingerprint,
            journal: Mutex::new(None),
            serial: tokio::sync::Mutex::new(()),
            errors: Mutex::new(HashMap::new()),
            observations: Mutex::new(None),
        }))
    }
    fn journal(&self) -> IvyResult<DefinitionJournal> {
        let client = self.0.engine.client().map_err(|_| {
            IvyError::new(
                "service_not_ready",
                "Hive is required to edit definitions; accepted jobs continue locally.",
            )
        })?;
        let mut journal = self.0.journal.lock().unwrap();
        let original = journal
            .get_or_insert_with(|| DefinitionJournal::new(self.0.engine.store(), client.clone()));
        Ok(original.with_client(client))
    }
    fn error(&self, job: &str) -> IvyJson {
        self.0
            .errors
            .lock()
            .unwrap()
            .get(job)
            .cloned()
            .map_or(IvyJson::Null, IvyJson::from)
    }
    fn no_secrets(&self, value: &IvyJson) -> IvyResult<()> {
        for (_, secret) in model::field(self.0.engine.settings(), "secrets")?
            .as_object()
            .ok_or_else(IvyError::internal)?
        {
            // The policy checks keys as well as values (approved DC02).
            let secret = secret.as_js_string().ok_or_else(IvyError::internal)?;
            if !secret.0.is_empty() {
                require(
                    !policy::contains_configured_secret_json(value, secret),
                    "invalid_arguments",
                    "Use secret references; configured secret values cannot enter collector history.",
                )?;
            }
        }
        Ok(())
    }
    async fn assignment(&self, job: &IvyJson) -> IvyResult<Option<IvyJson>> {
        let mut found = Vec::new();
        for document in self.journal()?.all("assignment").await? {
            if value(&document, "assignmentDocument")?.get("jobId") == Some(job) {
                found.push(document);
            }
        }
        require(
            found.len() <= 1,
            "target_conflict",
            "Collector assignment is ambiguous.",
        )?;
        Ok(found.pop())
    }
    async fn observation(&self, job: &IvyJson, node: &IvyJson) -> IvyResult<Option<IvyJson>> {
        let cached = self.0.observations.lock().unwrap().clone();
        let items = match cached {
            Some(items) => items,
            None => self.journal()?.all("observation").await?,
        };
        for document in items {
            let content = value(&document, "observationDocument")?;
            if content.get("jobId") == Some(job) && content.get("serviceNodeId") == Some(node) {
                return Ok(Some(document));
            }
        }
        Ok(None)
    }
    async fn row(&self, id: &str, deleted: bool) -> IvyResult<Option<IvyJson>> {
        let id = id.to_owned();
        self.0
            .engine
            .store()
            .call(move |s| s.get(&id, deleted))
            .await
    }
    async fn rows(&self, deleted: bool) -> IvyResult<Vec<IvyJson>> {
        self.0.engine.store().call(move |s| s.list(deleted)).await
    }
    async fn find(&self, task_id: &str) -> IvyResult<Option<IvyJson>> {
        if let Some(definition) = self
            .row(task_id, true)
            .await?
            .and_then(|r| r.get("definition").cloned())
        {
            return self
                .journal()?
                .document(
                    "definition",
                    model::field(&definition, "jobId")?.clone(),
                    None,
                )
                .await
                .map(Some);
        }
        let assignments = self.journal()?.all("assignment").await?;
        for document in self.journal()?.all("definition").await? {
            let definition = value(&document, "definitionDocument")?;
            if model::text(model::field(&definition, "task")?, "id")? != task_id {
                continue;
            }
            let mut matches =
                definition.get("homeServiceNodeId") == Some(&self.0.service_node_id.clone().into());
            for assignment in &assignments {
                let release = value(assignment, "assignmentDocument")?;
                matches |= release.get("jobId") == Some(id(&document)?)
                    && release.get("serviceNodeId") == Some(&self.0.service_node_id.clone().into());
            }
            if matches {
                return Ok(Some(document));
            }
        }
        Ok(None)
    }
    async fn task(&self, document: &IvyJson) -> IvyResult<IvyJson> {
        let definition = value(document, "definitionDocument")?;
        let reference = model::field(&definition, "script")?;
        let source = self
            .journal()?
            .document(
                "script",
                model::field(reference, "objectId")?.clone(),
                Some(model::field(reference, "revision")?.clone()),
            )
            .await?;
        let mut task = model::field(&definition, "task")?.clone();
        task.insert("enabled", false.into());
        task.insert(
            "script",
            model::field(&value(&source, "scriptDocument")?, "script")?.clone(),
        );
        schema::parse_schema("taskSchema", &task)
    }
    async fn save(
        &self,
        task: IvyJson,
        expected: f64,
        operation: &JsString,
        job: Option<IvyJson>,
        script_ref: Option<&IvyJson>,
    ) -> IvyResult<IvyJson> {
        self.no_secrets(&task)?;
        let journal = self.journal()?;
        if journal.retained(operation, "definition").await? {
            let saved = journal
                .write(
                    "definition",
                    model::object(),
                    operation,
                    "definition",
                    None,
                    model::object(),
                )
                .await?;
            return Ok(defined_assignment(IvyJson::object([
                ("jobId", id(&saved)?.clone()),
                ("revision", revision(&saved)?.clone()),
                (
                    "scriptRef",
                    model::field(&value(&saved, "definitionDocument")?, "script")?.clone(),
                ),
                ("task", task),
                (
                    "assignment",
                    self.assignment(id(&saved)?).await?.unwrap_or(IvyJson::Null),
                ),
            ])));
        }
        let explicit = job.is_some();
        let mut previous = if let Some(job) = job {
            Some(journal.document("definition", job, None).await?)
        } else {
            self.find(&model::text(&task, "id")?).await?
        };
        if !explicit && expected == 0.0 {
            if let Some(prior) = &previous {
                if let Some(assigned) = self.assignment(id(prior)?).await? {
                    if model::truth(&value(&assigned, "assignmentDocument")?, "deleted") {
                        previous = None;
                    }
                }
            }
        }
        require(
            previous
                .as_ref()
                .map(revision)
                .transpose()?
                .and_then(IvyJson::as_f64)
                .unwrap_or(0.0)
                == expected,
            "revision_conflict",
            "Reload the central definition before saving.",
        )?;
        let prior = previous
            .as_ref()
            .map(|p| value(p, "definitionDocument"))
            .transpose()?;
        require(
            prior
                .as_ref()
                .is_none_or(|p| p.get("task").and_then(|t| t.get("id")) == task.get("id")),
            "target_conflict",
            "A job's task identifier cannot change.",
        )?;
        let source = if let Some(reference) = script_ref {
            let source = journal
                .document(
                    "script",
                    model::field(reference, "objectId")?.clone(),
                    Some(model::field(reference, "revision")?.clone()),
                )
                .await?;
            require(
                value(&source, "scriptDocument")?.get("script") == task.get("script"),
                "revision_conflict",
                "The supplied script must match its exact revision.",
            )?;
            source
        } else {
            let old = if let Some(prior) = &prior {
                let reference = model::field(prior, "script")?;
                Some(
                    journal
                        .document(
                            "script",
                            model::field(reference, "objectId")?.clone(),
                            Some(model::field(reference, "revision")?.clone()),
                        )
                        .await?,
                )
            } else {
                None
            };
            if let Some(old) = old.as_ref().filter(|p| {
                value(p, "scriptDocument").is_ok_and(|v| v.get("script") == task.get("script"))
            }) {
                old.clone()
            } else {
                journal
                    .write(
                        "script",
                        IvyJson::object([
                            ("schemaVersion", number(1.0)),
                            ("script", model::field(&task, "script")?.clone()),
                        ]),
                        operation,
                        "script",
                        old.as_ref(),
                        model::object(),
                    )
                    .await?
            }
        };
        let mut configuration = task.clone();
        configuration.remove("script");
        configuration.remove("enabled");
        let saved = journal
            .write(
                "definition",
                IvyJson::object([
                    ("schemaVersion", number(1.0)),
                    (
                        "homeServiceNodeId",
                        prior
                            .as_ref()
                            .and_then(|p| p.get("homeServiceNodeId"))
                            .cloned()
                            .unwrap_or(self.0.service_node_id.clone().into()),
                    ),
                    ("task", configuration),
                    ("script", pin(&source)?),
                ]),
                operation,
                "definition",
                previous.as_ref(),
                IvyJson::object([("script", pin(&source)?)]),
            )
            .await?;
        Ok(defined_assignment(IvyJson::object([
            ("jobId", id(&saved)?.clone()),
            ("revision", revision(&saved)?.clone()),
            ("scriptRef", pin(&source)?),
            ("task", task),
            (
                "assignment",
                self.assignment(id(&saved)?).await?.unwrap_or(IvyJson::Null),
            ),
        ])))
    }
    async fn unknown(&self, row: &IvyJson) -> IvyResult<bool> {
        let id = model::optional_sqlite_key(row, "lastRunId");
        self.0
            .engine
            .store()
            .call(move |s| {
                Ok(match id {
                    Some(id) => s
                        .run(&id)?
                        .is_some_and(|r| r.get("error") == Some(&"outcome_unknown".into())),
                    None => false,
                })
            })
            .await
    }
    async fn accept(&self, document: &IvyJson) -> IvyResult<()> {
        let desired = value(document, "assignmentDocument")?;
        if let Err(error) = self.adopt(document, false).await {
            self.0
                .errors
                .lock()
                .unwrap()
                .insert(model::text(&desired, "jobId")?, error.code);
        }
        Ok(())
    }
    async fn adopt(&self, document: &IvyJson, confirmed: bool) -> IvyResult<()> {
        let desired = value(document, "assignmentDocument")?;
        let job = model::field(&desired, "jobId")?.clone();
        require(
            desired.get("serviceNodeId") == Some(&self.0.service_node_id.clone().into()),
            "target_conflict",
            "Only the assigned instance can adopt an execution definition.",
        )?;
        let definition = self
            .journal()?
            .document(
                "definition",
                job.clone(),
                Some(model::field(&desired, "definitionRevision")?.clone()),
            )
            .await?;
        let task = self.task(&definition).await?;
        let task_id = model::text(&task, "id")?;
        let row = self.row(&task_id, true).await?;
        require(
            row.as_ref().is_none_or(|r| {
                r.get("definition")
                    .is_none_or(|d| d.get("jobId") == Some(&job))
                    || model::truth(r, "deleted")
            }),
            "target_conflict",
            "Another job already owns this local task ID.",
        )?;
        if (!model::truth(&desired, "enabled") || model::truth(&desired, "deleted"))
            && row
                .as_ref()
                .is_some_and(|r| r.get("task").is_some_and(|t| model::truth(t, "enabled")))
        {
            self.0
                .engine
                .update(
                    IvyJson::object([
                        ("action", "disable".into()),
                        ("id", task_id.clone().into()),
                        (
                            "expectedRevision",
                            model::field(row.as_ref().unwrap(), "revision")?.clone(),
                        ),
                        (
                            "operationId",
                            format!(
                                "assignment-stop:{}:{}",
                                model::sqlite_key(id(document)?)?,
                                revision(document)?
                                    .as_f64()
                                    .ok_or_else(IvyError::internal)?
                            )
                            .into(),
                        ),
                    ]),
                    None,
                )
                .await?;
        }
        let engine = self.0.engine.clone();
        let check_id = task_id.clone();
        if engine
            .store()
            .call(move |s| Ok(s.executing(&check_id)? || engine.active(&check_id)))
            .await?
        {
            return Ok(());
        }
        if let Some(row) = &row {
            require(
                !self.unknown(row).await?,
                "outcome_unknown",
                "Verify the interrupted worker before accepting another execution definition.",
            )?;
        }
        let ever = if !confirmed && row.as_ref().is_none_or(|r| r.get("definition").is_none()) {
            self.observation(&job, &self.0.service_node_id.clone().into())
                .await?
                .map(|o| value(&o, "observationDocument").map(|v| model::truth(&v, "everAccepted")))
                .transpose()?
                .unwrap_or(false)
        } else {
            false
        };
        let this = self.clone();
        let assignment_id = model::sqlite_key(id(document)?)?;
        let assignment_revision = revision(document)?
            .as_f64()
            .ok_or_else(IvyError::internal)?;
        let deleted = model::truth(&desired, "deleted");
        let job_key = model::sqlite_key(&job)?;
        let auth_id = task_id.clone();
        let changed = self
            .0
            .engine
            .store()
            .call(move |store| {
                adoption::adopt(
                    store,
                    Adoption {
                        task: &task,
                        desired: &desired,
                        assignment_id: &assignment_id,
                        assignment_revision,
                        service_node_id: &this.0.service_node_id,
                        settings: this.0.engine.settings(),
                        confirmed_stopped: confirmed,
                        ever_accepted: ever,
                        active: this.0.engine.active(&task_id),
                        now_ms: this.0.engine.now() as f64,
                    },
                )
            })
            .await?;
        if changed {
            if deleted {
                let runner = self.0.engine.runner();
                self.0
                    .engine
                    .store()
                    .call(move |_| runner.clear_authentication(&auth_id))
                    .await?;
            }
            self.0.errors.lock().unwrap().remove(&job_key);
        }
        Ok(())
    }
    async fn peer(
        &self,
        node: &IvyJson,
        name: &str,
        args: IvyJson,
        operation: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        let clients = tools::ToolClients::new(self.journal()?.client);
        clients
            .service_tools(
                js(node)?,
                &[tools::InterfaceRequirement {
                    namespace: "data-collector".into(),
                    interface_version: "1.0.0".into(),
                }],
            )?
            .call(&name.into(), args, operation, Default::default())
            .await
    }
    async fn handoff(&self, job: &IvyJson, expected: f64) -> IvyResult<IvyJson> {
        let assignment = self.assignment(job).await?;
        require(
            assignment
                .as_ref()
                .map(revision)
                .transpose()?
                .and_then(IvyJson::as_f64)
                == Some(expected),
            "revision_conflict",
            "Reload the handoff assignment.",
        )?;
        let desired = value(assignment.as_ref().unwrap(), "assignmentDocument")?;
        require(
            desired.get("serviceNodeId") == Some(&self.0.service_node_id.clone().into())
                && !model::truth(&desired, "enabled"),
            "target_conflict",
            "Handoff belongs to the disabled source instance.",
        )?;
        let row = self
            .rows(true)
            .await?
            .into_iter()
            .find(|r| r.get("definition").and_then(|d| d.get("jobId")) == Some(job));
        let valid = if let Some(row) = &row {
            let task_id = model::text(model::field(row, "task")?, "id")?;
            let this = self.clone();
            row.get("definition")
                .and_then(|d| d.get("assignmentRevision"))
                .and_then(IvyJson::as_f64)
                == Some(expected)
                && !model::truth(model::field(row, "task")?, "enabled")
                && !self
                    .0
                    .engine
                    .store()
                    .call(move |s| Ok(s.busy(&task_id)? || this.0.engine.active(&task_id)))
                    .await?
                && !self.unknown(row).await?
        } else {
            false
        };
        require(
            valid,
            "outcome_unknown",
            "Handoff requires verified quiescence and a drained publication queue.",
        )?;
        Ok(row.unwrap())
    }
    async fn freeze(&self, job: &IvyJson, expected: f64) -> IvyResult<IvyJson> {
        self.handoff(job, expected).await?;
        let engine = self.0.engine.clone();
        let job_copy = job.clone();
        self.0
            .engine
            .store()
            .call(move |s| {
                s.transaction(|s| {
                    handoff_row(s, &engine, &job_copy, expected)?;
                    s.set_metadata(
                        &format!("handoff-lock:{}", model::sqlite_key(&job_copy)?),
                        &expected.to_string(),
                    )
                })
            })
            .await?;
        Ok(IvyJson::object([
            ("frozen", job.clone()),
            ("revision", number(expected)),
        ]))
    }
    async fn prepare(&self, job: &IvyJson, expected: f64) -> IvyResult<IvyJson> {
        let assignment = self.assignment(job).await?;
        require(
            assignment
                .as_ref()
                .map(revision)
                .transpose()?
                .and_then(IvyJson::as_f64)
                == Some(expected),
            "revision_conflict",
            "Reload the handoff assignment.",
        )?;
        let assignment = assignment.unwrap();
        let desired = value(&assignment, "assignmentDocument")?;
        require(
            desired.get("serviceNodeId") != Some(&self.0.service_node_id.clone().into())
                && !model::truth(&desired, "enabled"),
            "target_conflict",
            "Prepare handoff on its new target.",
        )?;
        let mut payload = self
            .peer(
                model::field(&desired, "serviceNodeId")?,
                "data-collector.read",
                IvyJson::object([
                    ("view", "handoff".into()),
                    ("jobId", job.clone()),
                    ("expectedRevision", number(expected)),
                ]),
                None,
            )
            .await?;
        payload.insert("assignmentId", id(&assignment)?.clone());
        payload.insert("assignmentRevision", number(expected + 1.0));
        let job_copy = job.clone();
        let this = self.clone();
        self.0.engine.store().call(move|store|store.transaction(|store|{
            let task_id=model::text(&payload,"taskId")?;let local=store.get(&task_id,true)?;
            require(local.as_ref().is_none_or(|r|model::truth(r,"deleted")||(r.get("definition").and_then(|d|d.get("jobId"))==Some(&job_copy)&&r.get("task").is_some_and(|t|!model::truth(t,"enabled")))),"target_conflict","Another job owns the handoff task ID.")?;
            require(!store.executing(&task_id)?&&!this.0.engine.active(&task_id),"task_busy","The target has an active worker.")?;
            let fingerprint=(this.0.fingerprint)(&task_id)?.map_or(IvyJson::Null,IvyJson::from);
            require(payload.get("authenticationFingerprint")==Some(&fingerprint),"authentication_required","Private authentication caches must be securely prepared on the target; collector handoff never transfers secret values.")?;
            let job_key=model::sqlite_key(&job_copy)?;
            store.set_metadata(&format!("handoff-state:{job_key}"),&stringify(&payload,usize::MAX)?)?;
            store.set_metadata(&format!("handoff-consumed:{job_key}"),"false")
        })).await?;
        Ok(IvyJson::object([("prepared", job.clone())]))
    }
    async fn activate(
        &self,
        job: IvyJson,
        definition_revision: f64,
        expected: f64,
        enabled: bool,
        operation: &JsString,
        target: Option<IvyJson>,
    ) -> IvyResult<IvyJson> {
        let journal = self.journal()?;
        if journal.retained(operation, "assignment").await? {
            let saved = journal
                .write(
                    "assignment",
                    model::object(),
                    operation,
                    "assignment",
                    None,
                    model::object(),
                )
                .await?;
            let desired = value(&saved, "assignmentDocument")?;
            let current = self.assignment(&job).await?;
            if desired.get("serviceNodeId") == Some(&self.0.service_node_id.clone().into())
                && current.as_ref().map(revision).transpose()? == Some(revision(&saved)?)
            {
                self.accept(&saved).await?;
            }
            return Ok(IvyJson::object([
                ("jobId", job.clone()),
                ("revision", number(definition_revision)),
                ("assignment", pin(&saved)?),
                ("desired", desired),
                ("adoptionError", self.error(&model::sqlite_key(&job)?)),
            ]));
        }
        let definition = journal
            .document("definition", job.clone(), Some(number(definition_revision)))
            .await?;
        let task = self.task(&definition).await?;
        let previous = self.assignment(&job).await?;
        let before = previous
            .as_ref()
            .map(|p| value(p, "assignmentDocument"))
            .transpose()?;
        require(
            previous
                .as_ref()
                .map(revision)
                .transpose()?
                .and_then(IvyJson::as_f64)
                .unwrap_or(0.0)
                == expected,
            "revision_conflict",
            "Reload the current assignment before activating.",
        )?;
        let node = target
            .or_else(|| {
                before
                    .as_ref()
                    .and_then(|b| b.get("serviceNodeId"))
                    .cloned()
            })
            .unwrap_or(self.0.service_node_id.clone().into());
        if let Some(before) = &before {
            if before.get("serviceNodeId") != Some(&node) {
                let source = model::field(before, "serviceNodeId")?;
                let observed = self.observation(&job, source).await?;
                let status = observed
                    .as_ref()
                    .map(|o| value(o, "observationDocument"))
                    .transpose()?;
                require(
                    !model::truth(before, "enabled")
                        && status.as_ref().is_some_and(|s| {
                            s.get("assignmentRevision").and_then(IvyJson::as_f64) == Some(expected)
                                && model::truth(s, "quiesced")
                        }),
                    "outcome_unknown",
                    "The previous instance must acknowledge this disabled assignment and verified worker termination before reassignment.",
                )?;
                for (selected, action) in [(source, "freezeHandoff"), (&node, "prepareHandoff")] {
                    if selected == &self.0.service_node_id.clone().into() {
                        if action == "freezeHandoff" {
                            self.freeze(&job, expected).await?;
                        } else {
                            self.prepare(&job, expected).await?;
                        }
                    } else {
                        let id = journal.mutation(operation, action).await?;
                        self.peer(
                            selected,
                            "data-collector.update",
                            IvyJson::object([
                                ("action", action.into()),
                                ("operationId", id.clone()),
                                ("jobId", job.clone()),
                                ("expectedRevision", number(expected)),
                            ]),
                            Some(js(&id)?),
                        )
                        .await?;
                    }
                }
            }
        }
        let local = self.row(&model::text(&task, "id")?, true).await?;
        let prior_value = |key| before.as_ref().and_then(|v| nullish(v.get(key)));
        let local_value = |key| local.as_ref().and_then(|v| nullish(v.get(key)));
        let desired = IvyJson::object([
            ("schemaVersion", number(1.0)),
            ("jobId", job.clone()),
            ("definitionRevision", number(definition_revision)),
            ("serviceNodeId", node.clone()),
            ("enabled", enabled.into()),
            ("deleted", false.into()),
            (
                "rootObjectId",
                prior_value("rootObjectId")
                    .or_else(|| local_value("resultRootId"))
                    .unwrap_or(self.0.engine.root_id().into()),
            ),
            (
                "resultObjectId",
                prior_value("resultObjectId")
                    .or_else(|| local_value("resultObjectId"))
                    .unwrap_or(IvyJson::Null),
            ),
        ]);
        let released = journal
            .write(
                "assignment",
                desired,
                operation,
                "assignment",
                previous.as_ref(),
                IvyJson::object([(
                    "definition",
                    IvyJson::object([
                        ("objectId", job.clone()),
                        ("revision", number(definition_revision)),
                    ]),
                )]),
            )
            .await?;
        if node == self.0.service_node_id.clone().into() {
            self.accept(&released).await?;
        }
        Ok(IvyJson::object([
            ("jobId", job.clone()),
            ("revision", number(definition_revision)),
            ("assignment", pin(&released)?),
            ("desired", content(&released)?.clone()),
            ("adoptionError", self.error(&model::sqlite_key(&job)?)),
        ]))
    }
    async fn report(&self, document: &IvyJson) -> IvyResult<()> {
        let desired = value(document, "assignmentDocument")?;
        let job = model::field(&desired, "jobId")?;
        let row = self
            .rows(true)
            .await?
            .into_iter()
            .find(|r| r.get("definition").and_then(|d| d.get("jobId")) == Some(job));
        let accepted = row.as_ref().is_some_and(|r| {
            r.get("definition")
                .and_then(|d| d.get("assignmentRevision"))
                == revision(document).ok()
        });
        let previous = self
            .observation(job, &self.0.service_node_id.clone().into())
            .await?;
        let prior = previous
            .as_ref()
            .map(|p| value(p, "observationDocument"))
            .transpose()?;
        let quiesced = if let Some(row) = &row {
            let task = model::field(row, "task")?;
            let task_id = model::text(task, "id")?;
            let this = self.clone();
            !model::truth(task, "enabled")
                && !self
                    .0
                    .engine
                    .store()
                    .call(move |s| Ok(s.busy(&task_id)? || this.0.engine.active(&task_id)))
                    .await?
                && !self.unknown(row).await?
        } else {
            false
        };
        let status = IvyJson::object([
            ("schemaVersion", number(1.0)),
            ("jobId", job.clone()),
            ("serviceNodeId", self.0.service_node_id.clone().into()),
            ("assignmentRevision", revision(document)?.clone()),
            (
                "definitionRevision",
                row.as_ref()
                    .and_then(|r| r.get("definition"))
                    .and_then(|d| d.get("revision"))
                    .cloned()
                    .unwrap_or(model::field(&desired, "definitionRevision")?.clone()),
            ),
            ("accepted", accepted.into()),
            (
                "everAccepted",
                (accepted
                    || prior
                        .as_ref()
                        .is_some_and(|p| model::truth(p, "everAccepted")))
                .into(),
            ),
            ("quiesced", quiesced.into()),
            ("error", self.error(&model::sqlite_key(job)?)),
            ("observedAt", self.0.engine.now_iso()?.into()),
        ]);
        if let Some(mut prior) = prior {
            let mut current = status.clone();
            prior.insert("observedAt", IvyJson::Null);
            current.insert("observedAt", IvyJson::Null);
            if canonical(&prior, usize::MAX)? == canonical(&current, usize::MAX)? {
                return Ok(());
            }
        }
        let saved = self
            .journal()?
            .write(
                "observation",
                status,
                &format!("observation:{}", uuid::Uuid::new_v4()).into(),
                "status",
                previous.as_ref(),
                model::object(),
            )
            .await?;
        if let Some(observations) = &mut *self.0.observations.lock().unwrap() {
            observations.retain(|d| id(d).ok() != id(&saved).ok());
            observations.push(saved);
        }
        Ok(())
    }
    pub async fn sync(&self) -> IvyResult<()> {
        let _serial = self.0.serial.lock().await;
        let journal = self.journal()?;
        journal.catalog().await?;
        let assignments = journal.all("assignment").await?;
        *self.0.observations.lock().unwrap() = Some(journal.all("observation").await?);
        struct Reset(Definitions);
        impl Drop for Reset {
            fn drop(&mut self) {
                *self.0.0.observations.lock().unwrap() = None;
            }
        }
        let _reset = Reset(self.clone());
        for document in &assignments {
            let desired = value(document, "assignmentDocument")?;
            if desired.get("serviceNodeId") != Some(&self.0.service_node_id.clone().into()) {
                continue;
            }
            let job = model::field(&desired, "jobId")?;
            let job_key = model::sqlite_key(job)?;
            let result: IvyResult<()> = async {
                let mut row = self
                    .rows(true)
                    .await?
                    .into_iter()
                    .find(|r| r.get("definition").and_then(|d| d.get("jobId")) == Some(job));
                if row.as_ref().is_none_or(|r| {
                    r.get("definition")
                        .and_then(|d| d.get("assignmentRevision"))
                        != revision(document).ok()
                }) || self.0.errors.lock().unwrap().contains_key(&job_key)
                {
                    self.accept(document).await?;
                    row =
                        self.rows(true).await?.into_iter().find(|r| {
                            r.get("definition").and_then(|d| d.get("jobId")) == Some(job)
                        });
                }
                if desired.get("resultObjectId").is_none_or(IvyJson::is_null) {
                    if let Some(result) =
                        row.as_ref().and_then(|r| nullish(r.get("resultObjectId")))
                    {
                        let mut bound = desired.clone();
                        bound.insert("resultObjectId", result.clone());
                        journal
                            .write(
                                "assignment",
                                bound,
                                &format!(
                                    "result-binding:{}:{}",
                                    model::sqlite_key(id(document)?)?,
                                    model::sqlite_key(&result)?
                                )
                                .into(),
                                "binding",
                                Some(document),
                                model::field(model::field(document, "revision")?, "references")?
                                    .clone(),
                            )
                            .await?;
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                self.0.errors.lock().unwrap().insert(job_key, error.code);
            }
            self.report(document).await?;
        }
        for row in self.rows(false).await? {
            let Some(definition) = row.get("definition") else {
                continue;
            };
            let Some(assignment) = assignments
                .iter()
                .find(|a| id(a).ok() == definition.get("assignmentId"))
            else {
                continue;
            };
            let task = model::field(&row, "task")?;
            if value(assignment, "assignmentDocument")?.get("serviceNodeId")
                != Some(&self.0.service_node_id.clone().into())
                && model::truth(task, "enabled")
            {
                self.0
                    .engine
                    .update(
                        IvyJson::object([
                            ("action", "disable".into()),
                            ("id", model::field(task, "id")?.clone()),
                            ("expectedRevision", model::field(&row, "revision")?.clone()),
                            (
                                "operationId",
                                format!(
                                    "unassigned:{}:{}",
                                    model::sqlite_key(id(assignment)?)?,
                                    revision(assignment)?
                                        .as_f64()
                                        .ok_or_else(IvyError::internal)?
                                )
                                .into(),
                            ),
                        ]),
                        None,
                    )
                    .await?;
            }
        }
        Ok(())
    }
    pub async fn read(&self, args: IvyJson) -> IvyResult<IvyJson> {
        let input = schema::parse_schema("readSchema", &args)?;
        let view = model::text(&input, "view")?;
        match view.as_str() {
            "handoff" => {
                let job = model::field(&input, "jobId")?;
                let expected = model::number(&input, "expectedRevision")?;
                self.handoff(job, expected).await?;
                let job = job.clone();
                let this = self.clone();
                self.0
                    .engine
                    .store()
                    .call(move |store| {
                        let row = handoff_row(store, &this.0.engine, &job, expected)?;
                        require(
                            store
                                .metadata(&format!("handoff-lock:{}", model::sqlite_key(&job)?))?
                                .as_deref()
                                == Some(&expected.to_string()),
                            "task_busy",
                            "Freeze manual starts before handoff.",
                        )?;
                        let state = model::field(&row, "state")?;
                        this.no_secrets(state)?;
                        let task_id = model::text(model::field(&row, "task")?, "id")?;
                        let fingerprint = (this.0.fingerprint)(&task_id)?;
                        Ok(IvyJson::object([
                            ("taskId", task_id.into()),
                            ("state", state.clone()),
                            ("nextAt", model::field(&row, "nextAt")?.clone()),
                            (
                                "authenticationFingerprint",
                                fingerprint.map_or(IvyJson::Null, IvyJson::from),
                            ),
                        ]))
                    })
                    .await
            }
            "script" => {
                self.journal()?
                    .document(
                        "script",
                        model::field(&input, "objectId")?.clone(),
                        input.get("revision").cloned(),
                    )
                    .await
            }
            "definitionHistory" => {
                let mut params = IvyJson::object([
                    ("objectId", model::field(&input, "jobId")?.clone()),
                    ("limit", number(50.0)),
                ]);
                if let Some(cursor) = input
                    .get("cursor")
                    .filter(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty()))
                {
                    params.insert("cursor", cursor.clone());
                }
                self.journal()?
                    .client
                    .request("objects.history", params, RequestOptions::default())
                    .await
            }
            "definition" => {
                let job = model::field(&input, "jobId")?;
                let document = self
                    .journal()?
                    .document("definition", job.clone(), input.get("revision").cloned())
                    .await?;
                let assignment = self.assignment(job).await?;
                let observation = if let Some(assignment) = &assignment {
                    self.observation(
                        job,
                        model::field(&value(assignment, "assignmentDocument")?, "serviceNodeId")?,
                    )
                    .await?
                } else {
                    None
                };
                Ok(defined_assignment(IvyJson::object([
                    ("jobId", job.clone()),
                    ("revision", revision(&document)?.clone()),
                    ("task", self.task(&document).await?),
                    ("assignment", assignment.unwrap_or(IvyJson::Null)),
                    ("observation", observation.unwrap_or(IvyJson::Null)),
                ])))
            }
            "definitions" => {
                let journal = self.journal()?;
                Ok(IvyJson::object([
                    ("items", IvyJson::Array(journal.all("definition").await?)),
                    (
                        "assignments",
                        IvyJson::Array(journal.all("assignment").await?),
                    ),
                    (
                        "observations",
                        IvyJson::Array(journal.all("observation").await?),
                    ),
                ]))
            }
            "tasks" | "task" => {
                let definitions = self.journal()?.all("definition").await?;
                let assignments = self.journal()?.all("assignment").await?;
                let mut runtime = self
                    .0
                    .engine
                    .read(IvyJson::object([("view", "tasks".into())]))
                    .await?;
                let locals = model::field(&runtime, "items")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .to_vec();
                let mut items = Vec::new();
                for document in definitions {
                    let definition = value(&document, "definitionDocument")?;
                    let mut assignment = None;
                    for candidate in &assignments {
                        if value(candidate, "assignmentDocument")?.get("jobId")
                            == Some(id(&document)?)
                        {
                            assignment = Some(candidate);
                            break;
                        }
                    }
                    let release = assignment
                        .map(|a| value(a, "assignmentDocument"))
                        .transpose()?;
                    let owner = release
                        .as_ref()
                        .and_then(|r| r.get("serviceNodeId"))
                        .or_else(|| definition.get("homeServiceNodeId"));
                    if owner != Some(&self.0.service_node_id.clone().into())
                        || release.as_ref().is_some_and(|r| model::truth(r, "deleted"))
                    {
                        continue;
                    }
                    let task_id = model::text(model::field(&definition, "task")?, "id")?;
                    let local = locals.iter().find(|r| {
                        r.get("task").and_then(|t| t.get("id")) == Some(&task_id.clone().into())
                    });
                    let row = self.row(&task_id, false).await?;
                    let adopted = row.as_ref().and_then(|r| r.get("definition"));
                    let mut item = local.cloned().unwrap_or_else(model::object);
                    let mut task = model::field(&definition, "task")?.clone();
                    task.insert(
                        "enabled",
                        (local.is_some()
                            && row
                                .as_ref()
                                .and_then(|r| r.get("task"))
                                .is_some_and(|t| model::truth(t, "enabled")))
                        .into(),
                    );
                    if view == "task" {
                        task.insert(
                            "script",
                            model::field(&self.task(&document).await?, "script")?.clone(),
                        );
                    }
                    item.insert("task", task);
                    item.insert("jobId", id(&document)?.clone());
                    item.insert("revision", revision(&document)?.clone());
                    item.insert(
                        "assignmentRevision",
                        assignment
                            .map(revision)
                            .transpose()?
                            .cloned()
                            .unwrap_or(number(0.0)),
                    );
                    item.insert(
                        "desiredRevision",
                        release
                            .as_ref()
                            .and_then(|r| r.get("definitionRevision"))
                            .cloned()
                            .unwrap_or(IvyJson::Null),
                    );
                    item.insert(
                        "adoptedRevision",
                        adopted
                            .and_then(|d| d.get("revision"))
                            .cloned()
                            .unwrap_or(IvyJson::Null),
                    );
                    item.insert(
                        "desiredEnabled",
                        release
                            .as_ref()
                            .is_some_and(|r| model::truth(r, "enabled"))
                            .into(),
                    );
                    item.insert(
                        "adoptionError",
                        self.error(&model::sqlite_key(id(&document)?)?),
                    );
                    let count = self
                        .0
                        .engine
                        .store()
                        .call(move |s| {
                            Ok(s.runs("publishing")?
                                .iter()
                                .filter(|r| r.get("taskId") == Some(&task_id.clone().into()))
                                .count())
                        })
                        .await?;
                    item.insert("pendingPublications", number(count as f64));
                    for (key, default) in [
                        ("status", "draft".into()),
                        ("error", IvyJson::Null),
                        ("lastRunAt", IvyJson::Null),
                        ("retentionWarning", IvyJson::Null),
                    ] {
                        item.insert(
                            key,
                            local.and_then(|l| nullish(l.get(key))).unwrap_or(default),
                        );
                    }
                    item.insert(
                        "resultObjectId",
                        local
                            .and_then(|l| nullish(l.get("resultObjectId")))
                            .or_else(|| {
                                release
                                    .as_ref()
                                    .and_then(|r| nullish(r.get("resultObjectId")))
                            })
                            .unwrap_or(IvyJson::Null),
                    );
                    items.push(item);
                }
                for local in locals {
                    let task_id = model::text(model::field(&local, "task")?, "id")?;
                    if self
                        .row(&task_id, false)
                        .await?
                        .is_none_or(|r| r.get("definition").is_none())
                        && !items.iter().any(|r| {
                            r.get("task").and_then(|t| t.get("id")) == Some(&task_id.clone().into())
                        })
                    {
                        items.push(local);
                    }
                }
                if view == "tasks" {
                    runtime.insert("items", IvyJson::Array(items));
                    runtime.insert(
                        "pendingPublications",
                        number(self.0.engine.pending_publications().await? as f64),
                    );
                    Ok(runtime)
                } else {
                    let item = items
                        .into_iter()
                        .find(|r| r.get("task").and_then(|t| t.get("id")) == input.get("id"))
                        .ok_or_else(|| IvyError::new("not_found", "Collector task not found."))?;
                    if item.get("jobId").is_some() {
                        Ok(item)
                    } else {
                        self.0.engine.read(input).await
                    }
                }
            }
            "result" | "history" => {
                let task_id = model::text(&input, "id")?;
                if self.row(&task_id, false).await?.is_none()
                    && self.find(&task_id).await?.is_some()
                {
                    Ok(IvyJson::Null)
                } else {
                    self.0.engine.read(input).await
                }
            }
            _ => self.0.engine.read(input).await,
        }
    }
    pub async fn update(
        &self,
        args: IvyJson,
        context: Option<InvocationContext>,
    ) -> IvyResult<IvyJson> {
        let input = schema::parse_schema("updateSchema", &args)?;
        let operation = js(model::field(&input, "operationId")?)?;
        let action = model::text(&input, "action")?;
        require(
            context
                .as_ref()
                .and_then(|c| c.operation_id.as_ref())
                .filter(|s| !s.0.is_empty())
                .is_none_or(|id| id == &operation),
            "operation_conflict",
            "Routed and inner operation IDs must match.",
        )?;
        if action == "save" {
            self.no_secrets(model::field(&input, "task")?)?;
        }
        if action == "saveScript" {
            self.no_secrets(model::field(&input, "script")?)?;
        }
        if action == "run" {
            let task_id = model::text(&input, "id")?;
            let check = input.clone();
            // Keep freeze admission and enqueue in the same store command. The
            // engine's runtime mutation performs the identical frozen check.
            if let Some(definition) = self
                .row(&task_id, false)
                .await?
                .and_then(|r| r.get("definition").cloned())
            {
                let job = model::text(&definition, "jobId")?;
                require(
                    self.0
                        .engine
                        .store()
                        .call(move |s| {
                            Ok(s.metadata(&format!("handoff-lock:{job}"))?
                                .and_then(|v| v.parse::<f64>().ok())
                                .unwrap_or(0.0)
                                == 0.0)
                        })
                        .await?,
                    "task_busy",
                    "This task is frozen for handoff; activate a new assignment before manual starts.",
                )?;
            }
            return self.0.engine.run_definition(check, context).await;
        }
        let _serial = self.0.serial.lock().await;
        let request = canonical(
            &IvyJson::object([
                (
                    "caller",
                    context
                        .as_ref()
                        .map(|c| IvyJson::String(c.caller_principal_id.clone()))
                        .unwrap_or("local".into()),
                ),
                ("input", input.clone()),
            ]),
            usize::MAX,
        )?;
        if let Some(result) = DefinitionJournal::begin_local(
            self.0.engine.store(),
            &operation,
            &request,
            self.0.engine.now(),
        )
        .await?
        {
            return Ok(result);
        }
        let journal = self.journal()?;
        let result = match action.as_str() {
            "freezeHandoff" => {
                self.freeze(
                    model::field(&input, "jobId")?,
                    model::number(&input, "expectedRevision")?,
                )
                .await?
            }
            "prepareHandoff" => {
                self.prepare(
                    model::field(&input, "jobId")?,
                    model::number(&input, "expectedRevision")?,
                )
                .await?
            }
            "save" => {
                self.save(
                    model::field(&input, "task")?.clone(),
                    model::number(&input, "expectedRevision")?,
                    &operation,
                    input.get("jobId").cloned(),
                    input.get("scriptRef"),
                )
                .await?
            }
            "saveScript" => {
                if journal.retained(&operation, "script").await? {
                    journal
                        .write(
                            "script",
                            model::object(),
                            &operation,
                            "script",
                            None,
                            model::object(),
                        )
                        .await?
                } else {
                    let prior = if let Some(id) = input.get("objectId") {
                        Some(journal.document("script", id.clone(), None).await?)
                    } else {
                        None
                    };
                    require(
                        prior
                            .as_ref()
                            .map(revision)
                            .transpose()?
                            .and_then(IvyJson::as_f64)
                            .unwrap_or(0.0)
                            == model::number(&input, "expectedRevision")?,
                        "revision_conflict",
                        "Reload the script revision.",
                    )?;
                    journal
                        .write(
                            "script",
                            IvyJson::object([
                                ("schemaVersion", number(1.0)),
                                ("script", model::field(&input, "script")?.clone()),
                            ]),
                            &operation,
                            "script",
                            prior.as_ref(),
                            model::object(),
                        )
                        .await?
                }
            }
            "activate" => {
                self.activate(
                    model::field(&input, "jobId")?.clone(),
                    model::number(&input, "definitionRevision")?,
                    model::number(&input, "expectedRevision")?,
                    model::truth(&input, "enabled"),
                    &operation,
                    input.get("serviceNodeId").cloned(),
                )
                .await?
            }
            "import" => {
                let mut imported = Vec::new();
                for row in self.rows(false).await? {
                    if let Some(definition) = row.get("definition") {
                        imported.push(definition.clone());
                        continue;
                    }
                    let task = model::field(&row, "task")?;
                    let task_id = model::text(task, "id")?;
                    let saved = if let Some(existing) = self.find(&task_id).await? {
                        IvyJson::object([
                            ("jobId", id(&existing)?.clone()),
                            ("revision", revision(&existing)?.clone()),
                        ])
                    } else {
                        self.save(
                            task.clone(),
                            0.0,
                            &append(&operation, &format!(":{task_id}")),
                            None,
                            None,
                        )
                        .await?
                    };
                    let job = model::field(&saved, "jobId")?;
                    let mut assignment = self.assignment(job).await?;
                    if assignment.is_none() {
                        self.activate(
                            job.clone(),
                            model::number(&saved, "revision")?,
                            0.0,
                            model::truth(task, "enabled"),
                            &append(&operation, &format!(":{task_id}:activate")),
                            None,
                        )
                        .await?;
                        assignment = self.assignment(job).await?;
                    }
                    self.adopt(assignment.as_ref().ok_or_else(IvyError::internal)?, true)
                        .await?;
                    imported.push(IvyJson::object([
                        ("id", task_id.into()),
                        ("jobId", job.clone()),
                    ]));
                }
                IvyJson::object([("imported", IvyJson::Array(imported))])
            }
            "recover" => {
                let job = model::field(&input, "jobId")?;
                let assignment = self.assignment(job).await?;
                require(
                    assignment.as_ref().map(revision).transpose()? == input.get("expectedRevision"),
                    "revision_conflict",
                    "Reload the assignment before recovery.",
                )?;
                let assignment = assignment.ok_or_else(IvyError::internal)?;
                require(
                    value(&assignment, "assignmentDocument")?.get("serviceNodeId")
                        == Some(&self.0.service_node_id.clone().into()),
                    "target_conflict",
                    "Recover on the assigned instance.",
                )?;
                self.adopt(&assignment, true).await?;
                IvyJson::object([("recovered", job.clone())])
            }
            _ => {
                let task_id = model::text(&input, "id")?;
                let definition = self.find(&task_id).await?;
                require(
                    definition.as_ref().map(revision).transpose()? == input.get("expectedRevision"),
                    "revision_conflict",
                    "Reload the central definition revision.",
                )?;
                let definition = definition.ok_or_else(IvyError::internal)?;
                let job = id(&definition)?;
                let assignment = self.assignment(job).await?;
                let desired = assignment
                    .as_ref()
                    .map(|a| value(a, "assignmentDocument"))
                    .transpose()?;
                require(
                    desired.as_ref().is_none_or(|d| {
                        d.get("serviceNodeId") == Some(&self.0.service_node_id.clone().into())
                    }),
                    "target_conflict",
                    "Change the task on its assigned instance.",
                )?;
                if action == "delete" {
                    let task_id = task_id.clone();
                    require(
                        !self
                            .0
                            .engine
                            .store()
                            .call(move |s| s.busy(&task_id))
                            .await?,
                        "task_busy",
                        "Publish pending work and stop workers before deleting a task.",
                    )?;
                }
                if action == "disable"
                    && input
                        .get("confirmedStoppedRunId")
                        .is_some_and(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty()))
                {
                    let row = self
                        .row(&task_id, false)
                        .await?
                        .ok_or_else(IvyError::internal)?;
                    let mut confirm = input.clone();
                    let mut id = JsString::from("definition-confirm:");
                    id.0.extend_from_slice(&operation.0);
                    confirm.insert("operationId", IvyJson::String(id));
                    confirm.insert("expectedRevision", model::field(&row, "revision")?.clone());
                    self.0.engine.update(confirm, None).await?;
                }
                let selected = if action == "enable" {
                    revision(&definition)?.as_f64()
                } else {
                    desired
                        .as_ref()
                        .and_then(|d| d.get("definitionRevision"))
                        .and_then(IvyJson::as_f64)
                        .or_else(|| revision(&definition).ok().and_then(IvyJson::as_f64))
                }
                .ok_or_else(IvyError::internal)?;
                let result = self
                    .activate(
                        job.clone(),
                        selected,
                        assignment
                            .as_ref()
                            .map(revision)
                            .transpose()?
                            .and_then(IvyJson::as_f64)
                            .unwrap_or(0.0),
                        action == "enable",
                        &operation,
                        None,
                    )
                    .await?;
                if action == "delete" {
                    let this = self.clone();
                    require(
                        !self
                            .0
                            .engine
                            .store()
                            .call(move |s| {
                                Ok(s.executing(&task_id)? || this.0.engine.active(&task_id))
                            })
                            .await?,
                        "task_busy",
                        "Wait for worker termination before deleting.",
                    )?;
                    let released = self.assignment(job).await?.ok_or_else(IvyError::internal)?;
                    let mut desired = value(&released, "assignmentDocument")?;
                    desired.insert("deleted", true.into());
                    let deleted = journal
                        .write(
                            "assignment",
                            desired,
                            &operation,
                            "delete",
                            Some(&released),
                            model::field(model::field(&released, "revision")?, "references")?
                                .clone(),
                        )
                        .await?;
                    self.adopt(&deleted, true).await?;
                }
                result
            }
        };
        journal
            .finish(&operation, &request, result, self.0.engine.now())
            .await
    }
}
