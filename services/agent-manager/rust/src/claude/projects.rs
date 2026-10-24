//! Project identities, idempotent requests and durable/ephemeral membership.
//! Derived from the pinned adapter; see services/agent-manager/claude/LICENSE.adapter.
use super::{
    error::{AdapterError, Result},
    store::Clock,
    value::{self, Compare, IdSource, cursor_offset, field, nonempty_string, number, string},
};
use ivy_contracts::{IvyJson, JsString};
use ivy_host_runtime::node_path::{self, Style};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub type ThreadLookup<'a> = dyn Fn(&JsString) -> Result<Option<IvyJson>> + 'a;
pub struct ProjectStore {
    path: PathBuf,
    state: IvyJson,
    ephemeral: BTreeMap<JsString, JsString>,
    style: Style,
    compare: Compare,
    clock: Clock,
    ids: IdSource,
}
impl ProjectStore {
    pub fn open(
        home: &Path,
        style: Style,
        compare: Compare,
        clock: Clock,
        ids: IdSource,
    ) -> Result<Self> {
        let path = home.join("projects.json");
        let state = if path.exists() {
            value::decode(&fs::read(&path)?)?
        } else {
            IvyJson::object([
                ("projects", IvyJson::Array(vec![])),
                ("requests", IvyJson::object([] as [(&str, IvyJson); 0])),
                ("threads", IvyJson::object([] as [(&str, IvyJson); 0])),
            ])
        };
        Ok(Self {
            path,
            state,
            ephemeral: BTreeMap::new(),
            style,
            compare,
            clock,
            ids,
        })
    }
    fn projects(&self) -> Result<&[IvyJson]> {
        field(&self.state, "projects")
            .as_array()
            .ok_or_else(|| AdapterError::typed("invalid_data", "Stored projects must be an array."))
    }
    fn save(&mut self, next: IvyJson) -> Result<()> {
        value::save_json(&self.path, &next)?;
        self.state = next;
        Ok(())
    }
    fn summary(&self, project: &IvyJson, thread: &ThreadLookup<'_>) -> Result<IvyJson> {
        let mut recency: Option<f64> = None;
        let threads = field(&self.state, "threads");
        let threads = threads.as_object().ok_or_else(|| {
            AdapterError::typed(
                "invalid_data",
                "Stored project memberships must be an object.",
            )
        })?;
        for (id, project_id) in threads {
            if project_id != field(project, "id") {
                continue;
            }
            if let Some(t) = thread(id)? {
                if !field(&t, "archived").as_bool().unwrap_or(false)
                    && !field(&t, "ephemeral").as_bool().unwrap_or(false)
                {
                    recency = Some(value::maximum(
                        recency.unwrap_or(0.0),
                        number(field(&t, "updatedAt")),
                    ));
                }
            }
        }
        let mut result = project.clone();
        result.insert("recencyAt", recency.map_or(IvyJson::Null, IvyJson::Number));
        Ok(result)
    }
    pub fn read(&self, id: &IvyJson, thread: &ThreadLookup<'_>) -> Result<IvyJson> {
        let project = self
            .projects()?
            .iter()
            .find(|p| p.get("id") == Some(id))
            .ok_or_else(|| {
                let mut message = JsString::from("Unknown project: ");
                message.0.extend(string(id).0);
                AdapterError::new(message)
            })?;
        self.summary(project, thread)
    }
    pub fn list(&self, params: &IvyJson, thread: &ThreadLookup<'_>) -> Result<IvyJson> {
        if !field(params, "sortDirection").is_null() && field(params, "sortKey").is_null() {
            return Err(AdapterError::new(
                "Project sort direction requires a sort key",
            ));
        }
        let recency = field(params, "sortKey") == &IvyJson::from("recencyAt");
        let direction = if field(params, "sortDirection") == &IvyJson::from("asc") {
            1.0
        } else if field(params, "sortDirection") == &IvyJson::from("desc") || recency {
            -1.0
        } else {
            1.0
        };
        let mut projects = self
            .projects()?
            .iter()
            .map(|p| self.summary(p, thread))
            .collect::<Result<Vec<_>>>()?;
        projects.sort_by(|a, b| {
            let ar = field(a, "recencyAt");
            let br = field(b, "recencyAt");
            if recency && (ar.is_null() || br.is_null()) {
                if ar.is_null() != br.is_null() {
                    return if ar.is_null() {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Less
                    };
                }
                return (number(field(a, "position")) - number(field(b, "position")))
                    .partial_cmp(&0.0)
                    .unwrap_or(std::cmp::Ordering::Equal);
            }
            let key = if recency { "recencyAt" } else { "position" };
            let difference = direction * (number(field(a, key)) - number(field(b, key)));
            if difference != 0.0 && !difference.is_nan() {
                return difference.partial_cmp(&0.0).unwrap();
            }
            (self.compare)(&string(field(a, "id")), &string(field(b, "id")))
        });
        let offset = if field(params, "cursor").is_null() {
            0.0
        } else {
            cursor_offset(field(params, "cursor"))?
        };
        if !offset.is_finite()
            || offset.fract() != 0.0
            || !(0.0..=9007199254740991.0).contains(&offset)
        {
            return Err(AdapterError::new("Invalid project cursor"));
        }
        let limit = value::maximum(
            1.0,
            value::minimum(
                if field(params, "limit").is_null() {
                    100.0
                } else {
                    number(field(params, "limit"))
                },
                200.0,
            ),
        );
        Ok(IvyJson::object([
            (
                "data",
                IvyJson::Array(value::array_slice(&projects, offset, offset + limit)),
            ),
            (
                "nextCursor",
                if offset + limit < projects.len() as f64 {
                    IvyJson::String(JsString::from((offset + limit).to_string()))
                } else {
                    IvyJson::Null
                },
            ),
        ]))
    }
    fn name(value: &IvyJson) -> Result<IvyJson> {
        let name = value
            .as_js_string()
            .map(JsString::trim)
            .filter(|s| !s.0.is_empty())
            .ok_or_else(|| AdapterError::new("Project name is required"))?;
        Ok(IvyJson::String(name))
    }
    fn roots(&self, value: &IvyJson) -> Result<IvyJson> {
        let roots = value
            .as_array()
            .ok_or_else(|| AdapterError::new("Project roots must be an array"))?;
        Ok(IvyJson::Array(
            roots
                .iter()
                .map(|root| {
                    let path = root
                        .get("path")
                        .and_then(IvyJson::as_js_string)
                        .filter(|p| node_path::is_absolute(self.style, p))
                        .ok_or_else(|| AdapterError::new("Project roots must be absolute paths"))?;
                    let mut normalized = node_path::normalize(self.style, path);
                    let separator = if self.style == Style::Windows { 92 } else { 47 };
                    // Existing project request fingerprints retain a trailing directory separator.
                    if path.0.last().is_some_and(|u| {
                        *u == separator || self.style == Style::Windows && *u == 47
                    }) && normalized.0.last() != Some(&separator)
                    {
                        normalized.0.push(separator);
                    }
                    Ok(IvyJson::object([("path", IvyJson::String(normalized))]))
                })
                .collect::<Result<Vec<_>>>()?,
        ))
    }
    fn metadata(&self, value: &IvyJson) -> Result<IvyJson> {
        if value.is_null() {
            return Ok(IvyJson::object([] as [(&str, IvyJson); 0]));
        }
        let mut fields = value
            .as_object()
            .ok_or_else(|| AdapterError::new("Project metadata must contain strings"))?
            .iter()
            .collect::<Vec<_>>();
        if fields.iter().any(|(_, v)| v.as_js_string().is_none()) {
            return Err(AdapterError::new("Project metadata must contain strings"));
        }
        fields.sort_by(|(a, _), (b, _)| (self.compare)(a, b));
        Ok(IvyJson::object(
            fields.into_iter().map(|(k, v)| (k.clone(), v.clone())),
        ))
    }
    pub fn create(&mut self, params: &IvyJson, thread: &ThreadLookup<'_>) -> Result<IvyJson> {
        let key = field(params, "idempotencyKey")
            .as_js_string()
            .filter(|k| !k.0.is_empty())
            .ok_or_else(|| AdapterError::new("Project creation requires an idempotency key"))?;
        let input = IvyJson::object([
            ("name", Self::name(field(params, "name"))?),
            ("roots", self.roots(field(params, "roots"))?),
            ("metadata", self.metadata(field(params, "metadata"))?),
        ]);
        let fingerprint = IvyJson::from(value::encode(&input)?);
        if field(&self.state, "requests").as_object().is_none() {
            return Err(AdapterError::typed(
                "invalid_data",
                "Stored project requests must be an object.",
            ));
        }
        if let Some(previous) = field(&self.state, "requests").get_key(key) {
            if previous.as_object().is_some() {
                if previous.get("fingerprint") != Some(&fingerprint) {
                    return Err(AdapterError::new(
                        "Project idempotency key belongs to another request",
                    ));
                }
                return self.read(field(previous, "projectId"), thread);
            }
        }
        let now = IvyJson::Number((self.clock)());
        let id = (self.ids)();
        let position = self
            .projects()?
            .iter()
            .fold(-1.0, |n, p| value::maximum(n, number(field(p, "position"))))
            + 1.0;
        let mut project = input;
        project.insert("id", IvyJson::String(id));
        project.insert("position", IvyJson::Number(position));
        project.insert("createdAt", now.clone());
        project.insert("updatedAt", now);
        let mut next = self.state.clone();
        let mut projects = self.projects()?.to_vec();
        projects.push(project.clone());
        next.insert("projects", IvyJson::Array(projects));
        let mut requests = IvyJson::object(
            field(&next, "requests")
                .as_object()
                .unwrap_or(&[])
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        requests.insert(
            key.clone(),
            IvyJson::object([
                ("fingerprint", fingerprint),
                ("projectId", field(&project, "id").clone()),
            ]),
        );
        next.insert("requests", requests);
        self.save(next)?;
        self.summary(&project, thread)
    }
    pub fn update(&mut self, params: &IvyJson, thread: &ThreadLookup<'_>) -> Result<IvyJson> {
        let project = self.read(field(params, "projectId"), thread)?;
        let mut next = self.state.clone();
        let mut projects = next.get("projects").unwrap().as_array().unwrap().to_vec();
        let target = projects
            .iter_mut()
            .find(|p| p.get("id") == project.get("id"))
            .unwrap();
        for key in ["name", "roots", "metadata"] {
            if !field(params, key).is_null() {
                let value = match key {
                    "name" => Self::name(field(params, key))?,
                    "roots" => self.roots(field(params, key))?,
                    _ => self.metadata(field(params, key))?,
                };
                target.insert(key, value);
            }
        }
        target.insert("updatedAt", IvyJson::Number((self.clock)()));
        let result = target.clone();
        next.insert("projects", IvyJson::Array(projects));
        self.save(next)?;
        self.summary(&result, thread)
    }
    pub fn delete(&mut self, id: &IvyJson, thread: &ThreadLookup<'_>) -> Result<()> {
        let project = self.read(id, thread)?;
        let id = field(&project, "id");
        let mut next = self.state.clone();
        next.insert(
            "projects",
            IvyJson::Array(
                self.projects()?
                    .iter()
                    .filter(|p| p.get("id") != Some(id))
                    .cloned()
                    .collect(),
            ),
        );
        next.insert(
            "threads",
            IvyJson::object(
                field(&self.state, "threads")
                    .as_object()
                    .unwrap_or(&[])
                    .iter()
                    .filter(|(_, v)| v != id)
                    .map(|(k, v)| (k.clone(), v.clone())),
            ),
        );
        self.save(next)?;
        self.ephemeral
            .retain(|_, value| IvyJson::String(value.clone()) != *id);
        Ok(())
    }
    pub fn bind(
        &mut self,
        thread: &IvyJson,
        project: &IvyJson,
        lookup: &ThreadLookup<'_>,
    ) -> Result<()> {
        self.read(project, lookup)?;
        let id = string(field(thread, "id"));
        let project = string(project);
        if field(thread, "ephemeral").as_bool().unwrap_or(false) {
            self.ephemeral.insert(id, project);
            return Ok(());
        }
        let mut next = self.state.clone();
        let mut threads = field(&next, "threads").clone();
        threads.insert(id, IvyJson::String(project));
        next.insert("threads", threads);
        self.save(next)
    }
    pub fn project_id(&self, thread: &IvyJson, lookup: &ThreadLookup<'_>) -> Result<IvyJson> {
        let mut current = Some(thread.clone());
        let mut seen = BTreeSet::new();
        while let Some(thread) = current {
            let id = string(field(&thread, "id"));
            if !seen.insert(id.clone()) {
                break;
            }
            let project = self
                .ephemeral
                .get(&id)
                .map(|v| IvyJson::String(v.clone()))
                .or_else(|| field(&self.state, "threads").get_key(&id).cloned());
            if let Some(value) = project.filter(nonempty_string) {
                return Ok(value);
            }
            current = if nonempty_string(field(&thread, "forkedFromId")) {
                lookup(&string(field(&thread, "forkedFromId")))?
            } else {
                None
            };
        }
        Ok(IvyJson::Null)
    }
}
