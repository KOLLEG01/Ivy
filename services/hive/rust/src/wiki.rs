//! Wiki page policy; the admitted request owner retains Object authorization,
//! mutation receipts and the reference-preserving content update callback.
use ivy_contracts::{IvyError, IvyJson, IvyResult};

pub trait Requests {
    fn request(&mut self, method: &'static str, parameters: &IvyJson) -> IvyResult<IvyJson>;
    fn update(&mut self, parameters: &IvyJson) -> IvyResult<IvyJson>;
}

#[derive(Clone, Copy)]
pub enum Action {
    Search,
    List,
    Read,
    Create,
    Update,
    History,
    Move,
    Archive,
}

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn selected(value: Option<&IvyJson>) -> bool {
    value.is_some_and(|v| v.as_js_string().is_some_and(|v| !v.0.is_empty()))
}
fn page(requests: &mut dyn Requests, selector: &IvyJson) -> IvyResult<IvyJson> {
    let object = requests.request("objects.stat", selector)?;
    if object.get("contractKey") != Some(&"wiki/page".into()) {
        return Err(IvyError::new(
            "invalid_arguments",
            "Select a Wiki page. Use Hive object tools for other data.",
        ));
    }
    Ok(object)
}
fn parent(requests: &mut dyn Requests, id: Option<&IvyJson>) -> IvyResult<()> {
    if selected(id) {
        page(
            requests,
            &IvyJson::object([("objectId", id.unwrap().clone())]),
        )?;
    }
    Ok(())
}

/// Parameters have already passed the owning operation's schema admission.
pub fn execute(
    requests: &mut dyn Requests,
    action: Action,
    parameters: &IvyJson,
) -> IvyResult<IvyJson> {
    match action {
        Action::Search => {
            parent(requests, parameters.get("rootId"))?;
            let mut query = parameters.clone();
            query.insert("contractKey", "wiki/page".into());
            requests.request("objects.search", &query)
        }
        Action::List => {
            let parent_id = parameters.get("parentId");
            parent(requests, parent_id)?;
            let mut query = parameters.clone();
            query.remove("parentId");
            query.insert("contractKey", "wiki/page".into());
            query.insert(
                "where",
                if selected(parent_id) {
                    IvyJson::object([
                        ("op", "eq".into()),
                        ("field", "object.parentId".into()),
                        ("value", parent_id.unwrap().clone()),
                    ])
                } else {
                    IvyJson::object([("op", "isNull".into()), ("field", "object.parentId".into())])
                },
            );
            query.insert(
                "select",
                IvyJson::Array(
                    ["object.name", "object.path", "object.effectivelyArchived"]
                        .into_iter()
                        .map(IvyJson::from)
                        .collect(),
                ),
            );
            query.insert(
                "orderBy",
                IvyJson::Array(vec![IvyJson::object([
                    ("field", "object.name".into()),
                    ("direction", "asc".into()),
                ])]),
            );
            requests.request("objects.query", &query)
        }
        Action::Read => {
            let selector = if selected(parameters.get("objectId")) {
                IvyJson::object([("objectId", field(parameters, "objectId")?.clone())])
            } else {
                IvyJson::object([("path", field(parameters, "path")?.clone())])
            };
            page(requests, &selector)?;
            requests.request("objects.read", parameters)
        }
        Action::Create => {
            parent(requests, parameters.get("parentId"))?;
            requests.request(
                "objects.write",
                &IvyJson::object([
                    ("mutationId", field(parameters, "mutationId")?.clone()),
                    ("contractVersion", "1.0.0".into()),
                    (
                        "references",
                        IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
                    ),
                    (
                        "create",
                        IvyJson::object([
                            ("contractKey", "wiki/page".into()),
                            ("name", field(parameters, "title")?.clone()),
                            (
                                "parentId",
                                parameters.get("parentId").cloned().unwrap_or(IvyJson::Null),
                            ),
                            ("ownerObjectId", IvyJson::Null),
                        ]),
                    ),
                    (
                        "content",
                        IvyJson::object([
                            ("encoding", "text".into()),
                            ("value", field(parameters, "markdown")?.clone()),
                        ]),
                    ),
                ]),
            )
        }
        Action::Update => {
            let current = page(
                requests,
                &IvyJson::object([("objectId", field(parameters, "objectId")?.clone())]),
            )?;
            if current.get("contractVersion") != Some(&"1.0.0".into()) {
                return Err(IvyError::new(
                    "contract_version_conflict",
                    "This Wiki page version is not writable.",
                ));
            }
            requests.update(parameters)
        }
        Action::History | Action::Move | Action::Archive => {
            page(
                requests,
                &IvyJson::object([("objectId", field(parameters, "objectId")?.clone())]),
            )?;
            let method = match action {
                Action::History => "objects.history",
                Action::Move => {
                    parent(requests, parameters.get("parentId"))?;
                    "objects.move"
                }
                Action::Archive => "objects.archive",
                _ => unreachable!(),
            };
            requests.request(method, parameters)
        }
    }
}
