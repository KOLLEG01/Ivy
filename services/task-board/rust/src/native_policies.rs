//! Concrete policy composition. Native prompt selection stays an explicit input
//! until its producer and retained execution contract are qualified together.
use crate::{
    category_projection::CategoryLocale,
    deliveries::DeliveryPolicies,
    engine_checks::CheckPolicy,
    engine_factory::{CurrentPolicies, PolicyContext, PolicyFactory},
    journal_store::Contracts,
    lifecycle_journal::LifecyclePages,
    native_intent::TaskPrompts,
    native_journal::NativeJournal,
    native_publication::NativePublicationPolicies,
    prompts,
    scheduler_journal::SchedulerJournalBindings,
    schema::RuntimeContracts,
    task_written::{TaskCommentNotices, TaskNoticePolicy},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::collation::LocaleComparator;
use std::cmp::Ordering;

/// The executable owns these native resources for every borrowed generation.
/// Prompt and Chat reader policies must be supplied; neither has a default.
pub struct NativePolicyFactory<P> {
    pub journal: NativeJournal,
    pub locale: LocaleComparator,
    pub prompts: P,
    pub chat_readable_versions: Vec<JsString>,
}

pub struct NativePolicies<'a, C, I, K, P> {
    context: PolicyContext<'a, RuntimeContracts, C, I, K>,
    owner: &'a NativePolicyFactory<P>,
    scheduler_enabled: bool,
    scheduler_interval_ms: f64,
    page_size: f64,
    phone_target: &'a Json,
}
impl<C: Fn() -> String, I: Fn() -> JsString, K, P: TaskPrompts>
    PolicyFactory<RuntimeContracts, C, I, K> for NativePolicyFactory<P>
{
    type Policies<'a>
        = NativePolicies<'a, C, I, K, P>
    where
        Self: 'a,
        C: 'a,
        I: 'a,
        K: 'a;

    fn bind<'a>(
        &'a self,
        context: PolicyContext<'a, RuntimeContracts, C, I, K>,
    ) -> IvyResult<Self::Policies<'a>> {
        // EngineFactory has admitted Settings before opening the journal.
        let scheduler = context.settings.required("scheduler")?;
        Ok(NativePolicies {
            scheduler_enabled: scheduler
                .required("enabled")?
                .as_bool()
                .ok_or_else(IvyError::internal)?,
            scheduler_interval_ms: scheduler
                .required("intervalMs")?
                .as_f64()
                .ok_or_else(IvyError::internal)?,
            page_size: scheduler
                .required("pageSize")?
                .as_f64()
                .ok_or_else(IvyError::internal)?,
            phone_target: context.settings.required("phoneTarget")?,
            context,
            owner: self,
        })
    }
}
impl<C, I, K, P> CheckPolicy for NativePolicies<'_, C, I, K, P> {
    async fn aggregate(&self, task: &Json, _: &JsString) -> IvyResult<()> {
        self.context
            .journal
            .contracts
            .validate_definition("Task", task)
    }
    async fn fields(&self, fields: &Json, _: Option<&JsString>) -> IvyResult<()> {
        self.context
            .journal
            .contracts
            .validate_definition("TaskFields", fields)
    }
    async fn creation(&self, request: &Json, _: &Json, _: &Json, _: &JsString) -> IvyResult<()> {
        self.context
            .journal
            .contracts
            .validate_definition("CreateRequest", request)
    }
}
impl<C, I, K, P: TaskPrompts> TaskPrompts for NativePolicies<'_, C, I, K, P> {
    fn task_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        coordination: Option<&Json>,
    ) -> IvyResult<JsString> {
        self.owner.prompts.task_prompt(task, owner, coordination)
    }
    fn task_update_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        coordination: bool,
    ) -> IvyResult<JsString> {
        self.owner
            .prompts
            .task_update_prompt(task, owner, coordination)
    }
}
impl<C: Fn() -> String, I, K, P> NativePublicationPolicies for NativePolicies<'_, C, I, K, P> {
    fn now(&self) -> JsString {
        (self.context.now)().into()
    }
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()> {
        CheckPolicy::aggregate(self, task, id).await
    }
}
impl<C: Fn() -> String, I, K, P> DeliveryPolicies for NativePolicies<'_, C, I, K, P> {
    fn now(&self) -> JsString {
        (self.context.now)().into()
    }
    fn phone_target(&self) -> &Json {
        self.phone_target
    }
    fn chat_readable_version(&self, version: &JsString) -> bool {
        self.owner.chat_readable_versions.contains(version)
    }
    fn handoff(&self, task: &Json, comment: &Json) -> IvyResult<JsString> {
        prompts::handoff(task, comment)
    }
    fn voice_prompt(&self, task: &Json, comment: &Json) -> IvyResult<JsString> {
        prompts::voice_prompt(task, comment)
    }
}
impl<C, I, K, P> TaskNoticePolicy for NativePolicies<'_, C, I, K, P> {
    fn comment(&self, task: &Json, operation: &JsString) -> IvyResult<Option<Json>> {
        TaskCommentNotices.comment(task, operation)
    }
}
impl<C, I, K, P> CategoryLocale for NativePolicies<'_, C, I, K, P> {
    fn lowercase_default(&self, value: &JsString) -> IvyResult<JsString> {
        self.owner.locale.lowercase(value)
    }
    fn compare_default(&self, left: &JsString, right: &JsString) -> Ordering {
        self.owner.locale.compare(left, right)
    }
}
impl<C: Fn() -> String, I: Fn() -> JsString, K, P> LifecyclePages
    for NativePolicies<'_, C, I, K, P>
{
    async fn page(&self, key: &str, options: &Json) -> IvyResult<Json> {
        self.context
            .journal
            .page(
                key,
                options,
                self.context.journal.contracts.readable_versions(key)?,
            )
            .await
    }
}
impl<C: Fn() -> String, I: Fn() -> JsString, K, P> SchedulerJournalBindings
    for NativePolicies<'_, C, I, K, P>
{
    fn free_bytes(&self) -> IvyResult<Option<f64>> {
        self.owner.journal.free_bytes()
    }
}
impl<C: Fn() -> String, I: Fn() -> JsString, K, P: TaskPrompts> CurrentPolicies
    for NativePolicies<'_, C, I, K, P>
{
    fn scheduler_enabled(&self) -> bool {
        self.scheduler_enabled
    }
    fn scheduler_interval_ms(&self) -> f64 {
        self.scheduler_interval_ms
    }
    fn page_size(&self) -> f64 {
        self.page_size
    }
}
