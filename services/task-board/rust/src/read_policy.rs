//! Shared interfaces for task values and the current question resolver.
use ivy_contracts::{IvyError, IvyJson as Json, JsString};

#[allow(async_fn_in_trait)]
pub trait TaskReadStore<E = IvyError> {
    /// Primary reads include archived records. Header-only reads skip hydration
    /// and upgrades; full reads materialize retained conversations.
    async fn read(&self, key: &str, address: &Json, header_only: bool) -> Result<Json, E>;
}
pub trait QuestionPolicies<E = IvyError> {
    fn open_questions(&self, task: &Json) -> Result<Vec<JsString>, E>;
}
pub struct IvyPolicies<'a, P: ?Sized>(pub &'a P);
impl<E: From<IvyError>, P: QuestionPolicies + ?Sized> QuestionPolicies<E> for IvyPolicies<'_, P> {
    fn open_questions(&self, task: &Json) -> Result<Vec<JsString>, E> {
        self.0.open_questions(task).map_err(E::from)
    }
}
