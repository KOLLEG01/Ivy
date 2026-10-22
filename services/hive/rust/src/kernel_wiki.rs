use super::*;
use crate::wiki::{self, Action, Requests};

pub(super) const METHODS: &[&str] = &[
    "wiki.search",
    "wiki.list",
    "wiki.read",
    "wiki.create",
    "wiki.update",
    "wiki.history",
    "wiki.move",
    "wiki.archive",
];

struct Backend<'a, 'b> {
    call: &'a mut Call<'b>,
    context: &'a IvyJson,
}
impl Requests for Backend<'_, '_> {
    fn request(&mut self, method: &'static str, parameters: &IvyJson) -> IvyResult<IvyJson> {
        let request = IvyJson::object([
            ("jsonrpc", "2.0".into()),
            ("id", "wiki".into()),
            ("method", method.into()),
            ("params", parameters.clone()),
        ]);
        field(&self.call.execute(self.context, &request)?, "value").cloned()
    }
    fn update(&mut self, parameters: &IvyJson) -> IvyResult<IvyJson> {
        let params = IvyJson::object([
            ("mutationId", field(parameters, "mutationId")?.clone()),
            ("objectId", field(parameters, "objectId")?.clone()),
            (
                "expectedRevision",
                field(parameters, "expectedRevision")?.clone(),
            ),
            ("contractVersion", "1.0.0".into()),
            (
                "references",
                IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
            ),
            (
                "content",
                IvyJson::object([
                    ("encoding", "text".into()),
                    ("value", field(parameters, "markdown")?.clone()),
                ]),
            ),
        ]);
        let now = (self.call.clock)();
        objects::write(
            self.call.store,
            &principal(self.context)?,
            &params,
            true,
            now as u64,
            &self.call.runtime.iso_date(now)?,
        )
    }
}
pub(super) fn handle(
    call: &mut Call<'_>,
    context: &IvyJson,
    params: &IvyJson,
    method: &str,
) -> IvyResult<IvyJson> {
    let action = match method {
        "wiki.search" => Action::Search,
        "wiki.list" => Action::List,
        "wiki.read" => Action::Read,
        "wiki.create" => Action::Create,
        "wiki.update" => Action::Update,
        "wiki.history" => Action::History,
        "wiki.move" => Action::Move,
        "wiki.archive" => Action::Archive,
        _ => return Err(IvyError::internal()),
    };
    wiki::execute(&mut Backend { call, context }, action, params)
}
