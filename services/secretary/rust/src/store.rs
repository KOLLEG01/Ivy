//! Hive records and local journal routing for Secretary durable workflows.
use crate::{
    journal::{Journal, Pin},
    schema::{self, Schema},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::canonical,
    operation::{derive_operation_id_js, parse_operation_id_js},
};
use ivy_sdk::{
    clock::{Clock, SystemClock},
    operation_ids::scoped_operation_id,
    rpc::{RequestOptions, RpcClient},
};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Record {
    Item,
    Source,
    Operation,
    Configuration,
    Assignment,
    ScheduleProgress,
    EventProgress,
    Execution,
}
impl Record {
    pub fn key(self) -> &'static str {
        match self {
            Self::Item => "secretary/item",
            Self::Source => "secretary/source",
            Self::Operation => "secretary/operation",
            Self::Configuration => "secretary/configuration",
            Self::Assignment => "secretary/assignment",
            Self::ScheduleProgress => "secretary/schedule-progress",
            Self::EventProgress => "secretary/event-progress",
            Self::Execution => "secretary/execution",
        }
    }
    fn schema(self) -> Schema {
        match self {
            Self::Item => Schema::Item,
            Self::Source => Schema::Source,
            Self::Operation => Schema::Accepted,
            Self::Configuration => Schema::SecretaryConfiguration,
            Self::Assignment => Schema::Assignment,
            Self::ScheduleProgress => Schema::ScheduleProgress,
            Self::EventProgress => Schema::EventProgress,
            Self::Execution => Schema::Execution,
        }
    }
    fn local(self) -> bool {
        matches!(self, Self::Source | Self::Operation)
    }
    pub fn versions(self) -> &'static [&'static str] {
        match self {
            Self::Configuration => &["1.0.0", "1.1.0"],
            Self::Item => &["1.0.0", "1.1.0", "1.2.0", "1.3.0"],
            Self::Assignment => &["1.0.0", "1.1.0", "1.2.0", "1.3.0", "1.4.0"],
            Self::ScheduleProgress | Self::EventProgress => &["1.0.0", "1.1.0", "1.2.0"],
            Self::Execution => &[
                "1.0.0", "1.1.0", "1.2.0", "1.3.0", "1.4.0", "1.5.0", "1.6.0",
            ],
            _ => &["1.0.0"],
        }
    }
}
fn need(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn pin(value: &IvyJson) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: value.required("objectId")?.as_str()?,
        revision: value
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
fn pin_value(pin: &Pin) -> IvyJson {
    IvyJson::object([
        ("objectId", pin.object_id.clone().into()),
        ("revision", IvyJson::Number(pin.revision as f64)),
    ])
}
fn local_document(document: crate::journal::Document) -> IvyJson {
    IvyJson::object([("pin", pin_value(&document.pin)), ("value", document.value)])
}

pub fn record_references(key: Record, value: &IvyJson) -> IvyResult<IvyJson> {
    let mut refs = IvyJson::object(Vec::<(String, IvyJson)>::new());
    match key {
        Record::Item => {
            for (i, entry) in value
                .required("media")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .enumerate()
            {
                if let Some(pin) = entry.get("pin").filter(|v| !(v).is_null()) {
                    refs.insert(format!("media-{i}"), pin.clone());
                }
            }
        }
        Record::Execution => {
            refs.insert("assignment", value.required("assignment")?.clone());
            let p = value.required("trigger")?.required("payload")?;
            if p.get("kind") == Some(&IvyJson::from("secretary.object.batch"))
                && p.get("objectId").and_then(IvyJson::as_js_string).is_some()
                && p.get("revision").and_then(IvyJson::as_f64).is_some()
                && p.get("beforeRevision").and_then(IvyJson::as_f64).is_some()
            {
                for (name, revision) in [("before", "beforeRevision"), ("input", "revision")] {
                    refs.insert(
                        name,
                        IvyJson::object([
                            ("objectId", p.required("objectId")?.clone()),
                            ("revision", p.required(revision)?.clone()),
                        ]),
                    );
                }
            }
        }
        Record::ScheduleProgress | Record::EventProgress => {
            refs.insert("assignment", value.required("assignment")?.clone());
        }
        _ => (),
    }
    Ok(refs)
}
fn operation_identity(value: &IvyJson) -> Option<&JsString> {
    match value {
        IvyJson::String(s) => parse_operation_id_js(s)
            .ok()
            .filter(|id| id.issued_at_unix_ms >= 1_000_000_000_000)
            .map(|_| s),
        IvyJson::Array(a) => a.iter().find_map(operation_identity),
        IvyJson::Object(values) => values
            .iter()
            .find_map(|(_, value)| operation_identity(value)),
        _ => None,
    }
}
pub fn mutation(parts: &[IvyJson]) -> IvyResult<JsString> {
    let value = IvyJson::Array(parts.to_vec());
    match operation_identity(&value) {
        Some(parent) => derive_operation_id_js(parent, &value),
        None => Ok(format!("secretary:{}", &hash_json(&value)?[7..]).into()),
    }
}
pub fn conflict(error: &IvyError) -> bool {
    error.outcome == Outcome::NotExecuted
        && matches!(
            error.code.as_str(),
            "revision_conflict" | "mutation_conflict"
        )
}

pub struct Store<'a> {
    pub client: Arc<dyn RpcClient>,
    pub root: JsString,
    pub journal: &'a Journal,
}
impl Store<'_> {
    pub fn decode(&self, key: Record, read: &IvyJson) -> IvyResult<IvyJson> {
        let object = read.required("object")?;
        let revision = read.required("revision")?;
        let content = read.required("content")?;
        need(
            !key.local()
                && object.get("parentId") == Some(&IvyJson::String(self.root.clone()))
                && !object
                    .get("effectivelyArchived")
                    .is_some_and(|value| value == &IvyJson::Bool(true))
                && object.get("contractKey") == Some(&IvyJson::from(key.key()))
                && revision
                    .get("contractVersion")
                    .and_then(|v| v.as_str().ok())
                    .is_some_and(|v| key.versions().contains(&v.as_str()))
                && revision.get("mediaType") == Some(&IvyJson::from("application/json"))
                && content.get("encoding") == Some(&IvyJson::from("json")),
            "secretary_scope_conflict",
            "The Secretary record must remain in its original active root and exact contract.",
        )?;
        let value = content.required("value")?;
        need(
            revision.get("contentHash") == Some(&hash_json(value)?.into())
                && revision.get("byteLength").and_then(IvyJson::as_f64)
                    == Some(canonical(value, 33_554_432)?.len() as f64),
            "secretary_evidence_conflict",
            "The saved record does not match its declared bytes.",
        )?;
        schema::validate(key.schema(), value)?;
        need(
            same(
                revision.required("references")?,
                &record_references(key, value)?,
            )?,
            "secretary_evidence_conflict",
            "The Secretary record references must match its exact revision content.",
        )?;
        Ok(IvyJson::object([
            (
                "pin",
                IvyJson::object([
                    ("objectId", object.required("id")?.clone()),
                    ("revision", revision.required("revision")?.clone()),
                ]),
            ),
            ("value", value.clone()),
            ("writtenAt", revision.required("createdAt")?.clone()),
        ]))
    }
    pub async fn read(&self, key: Record, selected: &IvyJson) -> IvyResult<IvyJson> {
        if key.local() {
            let (id, revision) = if matches!(selected, IvyJson::String(_)) {
                (selected.as_str()?, None)
            } else {
                let p = pin(selected)?;
                (p.object_id, Some(p.revision))
            };
            let result = self.journal.read(key.key(), &id, revision, false)?;
            schema::validate(key.schema(), &result.value)?;
            return Ok(local_document(result));
        }
        let params = if matches!(selected, IvyJson::String(_)) {
            IvyJson::object([("objectId", selected.clone())])
        } else {
            selected.clone()
        };
        self.decode(
            key,
            &self
                .client
                .request("objects.read", params, RequestOptions::default())
                .await?,
        )
    }
    pub async fn named(&self, key: Record, name: &JsString) -> IvyResult<Option<IvyJson>> {
        if key.local() {
            return self
                .journal
                .named(key.key(), &String::from_utf16_lossy(&name.0), false)?
                .map(|d| {
                    schema::validate(key.schema(), &d.value)?;
                    Ok(local_document(d))
                })
                .transpose();
        }
        let eq = |field: &str, value: IvyJson| {
            IvyJson::object([
                ("op", "eq".into()),
                ("field", field.into()),
                ("value", value),
            ])
        };
        let page = self
            .client
            .request(
                "objects.query",
                IvyJson::object([
                    ("contractKey", key.key().into()),
                    ("includeArchived", true.into()),
                    ("limit", IvyJson::Number(2.0)),
                    (
                        "where",
                        IvyJson::object([
                            ("op", "and".into()),
                            (
                                "args",
                                IvyJson::Array(vec![
                                    eq("object.parentId", IvyJson::String(self.root.clone())),
                                    eq("object.name", IvyJson::String(name.clone())),
                                ]),
                            ),
                        ]),
                    ),
                ]),
                RequestOptions::default(),
            )
            .await?;
        let items = page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        need(
            items.len() <= 1
                && !page
                    .get("nextCursor")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|text| !text.0.is_empty()),
            "secretary_identity_conflict",
            "An original Secretary name must be unique.",
        )?;
        match items.first() {
            Some(row) => Ok(Some(self.read(key, row.required("objectId")?).await?)),
            None => Ok(None),
        }
    }
    pub fn object_write_request(
        &self,
        key: Record,
        value: &IvyJson,
        destination: &IvyJson,
        id: &JsString,
    ) -> IvyResult<IvyJson> {
        need(
            !key.local(),
            "invalid_arguments",
            "Local Secretary records do not have Hive write receipts.",
        )?;
        schema::validate(key.schema(), value)?;
        parse_operation_id_js(id)?;
        let mut request = IvyJson::object([
            ("mutationId", IvyJson::String(id.clone())),
            ("contractVersion", (*key.versions().last().unwrap()).into()),
            ("references", record_references(key, value)?),
        ]);
        if let Some(name) = destination.get("createName") {
            request.insert(
                "create",
                IvyJson::object([
                    ("contractKey", key.key().into()),
                    ("parentId", IvyJson::String(self.root.clone())),
                    ("name", name.clone()),
                    ("ownerObjectId", IvyJson::Null),
                ]),
            );
        } else {
            let p = destination.required("object")?;
            request.insert("objectId", p.required("objectId")?.clone());
            request.insert("expectedRevision", p.required("revision")?.clone());
        }
        request.insert(
            "content",
            IvyJson::object([("encoding", "json".into()), ("value", value.clone())]),
        );
        Ok(request)
    }
    pub async fn write_prepared(&self, request: IvyJson) -> IvyResult<IvyJson> {
        let result = self
            .client
            .request("objects.write", request, RequestOptions::default())
            .await?;
        Ok(IvyJson::object([
            (
                "objectId",
                result.required("object")?.required("id")?.clone(),
            ),
            (
                "revision",
                result.required("revision")?.required("revision")?.clone(),
            ),
        ]))
    }
    pub async fn write(
        &self,
        key: Record,
        value: &IvyJson,
        destination: &IvyJson,
        id: &JsString,
    ) -> IvyResult<IvyJson> {
        schema::validate(key.schema(), value)?;
        canonical(value, 33_554_432)?;
        if key.local() {
            let now = SystemClock.utc_millis();
            return Ok(pin_value(
                &if let Some(name) = destination.get("createName") {
                    self.journal
                        .create_local(key.key(), &name.as_str()?, value, now)?
                } else {
                    self.journal.amend_local(
                        key.key(),
                        &pin(destination.required("object")?)?,
                        value,
                        now,
                    )?
                },
            ));
        }
        self.write_prepared(self.object_write_request(key, value, destination, id)?)
            .await
    }
    pub async fn create(
        &self,
        key: Record,
        name: &JsString,
        value: &IvyJson,
        id: &JsString,
    ) -> IvyResult<IvyJson> {
        if let Some(existing) = self.named(key, name).await? {
            return Ok(existing);
        }
        // The original returns the read promise without awaiting it in the
        // write-recovery catch. A rejected read must not trigger winner lookup.
        let result = self
            .write(
                key,
                value,
                &IvyJson::object([("createName", IvyJson::String(name.clone()))]),
                id,
            )
            .await;
        match result {
            Ok(p) => self.read(key, &p).await,
            Err(error) => match self.named(key, name).await? {
                Some(winner) => Ok(winner),
                None => Err(error),
            },
        }
    }
    pub async fn amend(
        &self,
        current: &IvyJson,
        key: Record,
        value: &IvyJson,
    ) -> IvyResult<IvyJson> {
        if same(current.required("value")?, value)? {
            return Ok(current.clone());
        }
        let p = current.required("pin")?;
        let id = scoped_operation_id(
            self.client.clone(),
            &IvyJson::Array(vec![
                "secretary-amend".into(),
                IvyJson::String(self.root.clone()),
                key.key().into(),
                p.clone(),
                hash_json(value)?.into(),
            ]),
        )
        .await?;
        let result = self
            .write(key, value, &IvyJson::object([("object", p.clone())]), &id)
            .await;
        match result {
            Ok(next) => self.read(key, &next).await,
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "revision_conflict" | "mutation_conflict"
                ) =>
            {
                let saved = self.read(key, p.required("objectId")?).await?;
                if same(saved.required("value")?, value)? {
                    Ok(saved)
                } else {
                    Err(error)
                }
            }
            other => other,
        }
    }
    pub async fn original(&self, key: Record, current: &IvyJson) -> IvyResult<IvyJson> {
        let p = current.required("pin")?;
        if p.get("revision").and_then(IvyJson::as_f64) == Some(1.0) {
            Ok(current.clone())
        } else {
            self.read(
                key,
                &IvyJson::object([
                    ("objectId", p.required("objectId")?.clone()),
                    ("revision", IvyJson::Number(1.0)),
                ]),
            )
            .await
        }
    }
}
