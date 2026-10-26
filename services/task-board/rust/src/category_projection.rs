//! Disposable category projection over authoritative Tasks and their event journal.
//! Alignment is serialized on the same scoped owner executor as reconciliation.
use crate::{
    journal_store::Contracts,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    operations::Store,
    reconciler::ReconcileExecutor,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::digest};
use ivy_sdk::{
    clock::parse_date,
    rpc::{RequestOptions, RpcClient},
};
use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

/// Use the owning host's default locale for category casing and ordering.
/// Identifier ordering is separate from this user-facing display policy.
pub trait CategoryLocale {
    fn lowercase_default(&self, value: &JsString) -> IvyResult<JsString>;
    fn compare_default(&self, left: &JsString, right: &JsString) -> Ordering;
}
impl CategoryLocale for ivy_sdk::collation::LocaleComparator {
    fn lowercase_default(&self, value: &JsString) -> IvyResult<JsString> {
        self.lowercase(value)
    }
    fn compare_default(&self, left: &JsString, right: &JsString) -> Ordering {
        self.compare(left, right)
    }
}
#[allow(async_fn_in_trait)]
pub trait CategoryStore {
    fn root_object_id(&self) -> Option<JsString>;
    fn client(&self) -> &dyn RpcClient;
    async fn task_page(&self, options: &Json) -> IvyResult<Json>;
}
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: LifecyclePages,
> CategoryStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    fn root_object_id(&self) -> Option<JsString> {
        Store::root_object_id(self)
    }
    fn client(&self) -> &dyn RpcClient {
        self.journal.client.as_ref()
    }
    async fn task_page(&self, o: &Json) -> IvyResult<Json> {
        self.pages.page("task-board/task", o).await
    }
}
#[derive(Clone)]
struct Entry {
    revision: f64,
    created_at: JsString,
    archived: bool,
    category: Option<JsString>,
}
/// Keep deterministic insertion order, including delete/reinsert, without a
/// linear lookup or accumulating deleted slots in this long-lived cache.
#[derive(Default)]
struct Entries {
    positions: HashMap<JsString, usize>,
    ordered: BTreeMap<usize, (JsString, Entry)>,
    next: usize,
}
impl Entries {
    fn get(&self, id: &JsString) -> Option<&Entry> {
        self.positions
            .get(id)
            .and_then(|p| self.ordered.get(p))
            .map(|(_, v)| v)
    }
    fn insert(&mut self, id: JsString, value: Entry) {
        let position = if let Some(p) = self.positions.get(&id) {
            *p
        } else {
            let p = self.next;
            self.next += 1;
            self.positions.insert(id.clone(), p);
            p
        };
        self.ordered.insert(position, (id, value));
    }
    fn remove(&mut self, id: &JsString) {
        if let Some(p) = self.positions.remove(id) {
            self.ordered.remove(&p);
        }
    }
}
#[derive(Default)]
struct Completion {
    result: Option<IvyResult<()>>,
    waiters: Vec<Waker>,
}
struct Job {
    immediate: bool,
    completion: Rc<RefCell<Completion>>,
}
#[derive(Default)]
struct State {
    entries: Entries,
    ready: bool,
    pending: usize,
    running: bool,
    jobs: VecDeque<Job>,
    waiters: Vec<Waker>,
}
pub struct CategoryProjection<'a, 'x, S, L, X> {
    store: &'a S,
    locale: &'a L,
    executor: &'x X,
    subscription: Json,
    state: Rc<RefCell<State>>,
}
impl<S, L, X> Clone for CategoryProjection<'_, '_, S, L, X> {
    fn clone(&self) -> Self {
        Self {
            store: self.store,
            locale: self.locale,
            executor: self.executor,
            subscription: self.subscription.clone(),
            state: self.state.clone(),
        }
    }
}
enum ResultKind {
    Categories,
    Normalize(JsString),
    Null,
}
/// Admission is eager; dropping this result never drops admitted alignment work.
pub struct CategoryRequest<'a, 'x, S, L, X> {
    projection: CategoryProjection<'a, 'x, S, L, X>,
    completion: Rc<RefCell<Completion>>,
    kind: ResultKind,
}
impl<S, L: CategoryLocale, X> Future for CategoryRequest<'_, '_, S, L, X> {
    type Output = IvyResult<Json>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let result = {
            let mut c = self.completion.borrow_mut();
            match &c.result {
                Some(r) => Some(r.clone()),
                None => {
                    if !c.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                        c.waiters.push(cx.waker().clone());
                    }
                    None
                }
            }
        };
        match result {
            None => Poll::Pending,
            Some(Err(e)) => Poll::Ready(Err(e)),
            Some(Ok(())) => Poll::Ready(match &self.kind {
                ResultKind::Null => Ok(Json::Null),
                ResultKind::Categories => self.projection.values().map(|v| {
                    Json::object([(
                        "categories",
                        Json::Array(v.into_iter().map(Json::String).collect()),
                    )])
                }),
                ResultKind::Normalize(v) => self.projection.normalized(v).map(Json::String),
            }),
        }
    }
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn number(v: &Json, k: &str) -> f64 {
    f(v, k).as_f64().unwrap_or(f64::NAN)
}
fn array(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn pending(message: &str) -> IvyError {
    IvyError::new("task_board_category_projection_pending", message)
}
fn entry(item: &Json) -> IvyResult<Entry> {
    let values = f(item, "values");
    let created = f(values, "data:/createdAt").as_js_string();
    require(
        created.is_some_and(|v| parse_date(v).is_finite()),
        "task_board_category_projection_invalid",
        "A projected Task needs its canonical creation time.",
    )?;
    Ok(Entry {
        revision: number(item, "revision"),
        created_at: created.unwrap().clone(),
        archived: false,
        category: f(values, "data:/fields/category")
            .as_js_string()
            .filter(|v| !v.0.is_empty())
            .cloned(),
    })
}
fn page_options(limit: f64) -> Json {
    Json::object([
        ("limit", Json::Number(limit)),
        (
            "select",
            Json::Array(vec![
                "data:/fields/category".into(),
                "data:/createdAt".into(),
            ]),
        ),
        (
            "orderBy",
            Json::Array(vec![Json::object([
                ("field", "object.id".into()),
                ("direction", "asc".into()),
            ])]),
        ),
    ])
}
impl<S, L: CategoryLocale, X> CategoryProjection<'_, '_, S, L, X> {
    fn values(&self) -> IvyResult<Vec<JsString>> {
        let entries = {
            let state = self.state.borrow();
            require(
                state.ready,
                "task_board_category_projection_pending",
                "The Task category projection is rebuilding.",
            )?;
            state.entries.ordered.values().cloned().collect::<Vec<_>>()
        };
        let mut positions = HashMap::<JsString, usize>::new();
        let mut first: Vec<(JsString, JsString, JsString)> = vec![];
        for (id, entry) in entries {
            if entry.archived {
                continue;
            }
            let Some(category) = entry.category.filter(|v| !v.0.is_empty()) else {
                continue;
            };
            let key = self.locale.lowercase_default(&category)?;
            if let Some(index) = positions.get(&key) {
                let prior = &first[*index];
                if entry.created_at < prior.1 || entry.created_at == prior.1 && id < prior.2 {
                    first[*index] = (category, entry.created_at, id);
                }
            } else {
                positions.insert(key, first.len());
                first.push((category, entry.created_at, id));
            }
        }
        let mut values = first.into_iter().map(|v| v.0).collect::<Vec<_>>();
        values.sort_by(|a, b| self.locale.compare_default(a, b));
        values.truncate(256);
        Ok(values)
    }
    fn normalized(&self, category: &JsString) -> IvyResult<JsString> {
        for value in self.values()? {
            if self.locale.lowercase_default(&value)? == self.locale.lowercase_default(category)? {
                return Ok(value);
            }
        }
        Ok(category.clone())
    }
    pub fn observe(&self, id: &JsString, revision: f64, task: &Json) {
        let mut state = self.state.borrow_mut();
        if !state.ready || state.entries.get(id).is_some_and(|v| v.revision > revision) {
            return;
        }
        state.entries.insert(
            id.clone(),
            Entry {
                revision,
                created_at: f(task, "createdAt")
                    .as_js_string()
                    .cloned()
                    .unwrap_or_default(),
                archived: false,
                category: f(f(task, "fields"), "category").as_js_string().cloned(),
            },
        );
    }
    /// Stop admitting owner requests before awaiting this drain during shutdown.
    pub async fn settled(&self) {
        poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if state.pending == 0 && !state.running {
                Poll::Ready(())
            } else {
                if !state.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                    state.waiters.push(cx.waker().clone());
                }
                Poll::Pending
            }
        })
        .await
    }
}
impl<'a, 'x, S: CategoryStore + 'a, L: CategoryLocale + 'a, X: ReconcileExecutor<'a>>
    CategoryProjection<'a, 'x, S, L, X>
{
    pub fn new(store: &'a S, principal: &JsString, locale: &'a L, executor: &'x X) -> Self {
        let mut scope = principal.0.clone();
        scope.push(u16::from(b':'));
        scope.extend(store.root_object_id().unwrap_or_else(|| "root".into()).0);
        let hash = digest(String::from_utf16_lossy(&scope));
        Self {
            store,
            locale,
            executor,
            subscription: Json::object([
                (
                    "name",
                    format!("task-board-categories-{}", &hash[7..31]).into(),
                ),
                (
                    "filter",
                    Json::object([("topics", Json::Array(vec!["hive.object.changed".into()]))]),
                ),
                ("initialSequence", Json::Number(0.0)),
                ("limit", Json::Number(100.0)),
            ]),
            state: Rc::new(RefCell::new(State::default())),
        }
    }
    fn request(&self, kind: ResultKind, immediate: bool) -> CategoryRequest<'a, 'x, S, L, X> {
        let completion = Rc::new(RefCell::new(Completion::default()));
        if matches!(kind, ResultKind::Null) {
            completion.borrow_mut().result = Some(Ok(()));
        } else {
            let spawn = {
                let mut state = self.state.borrow_mut();
                if immediate && state.pending != 0 {
                    completion.borrow_mut().result = Some(Err(pending(
                        "The Task category projection is already aligning outside the publication gate.",
                    )));
                    false
                } else {
                    state.pending += 1;
                    state.jobs.push_back(Job {
                        immediate,
                        completion: completion.clone(),
                    });
                    if state.running {
                        false
                    } else {
                        state.running = true;
                        true
                    }
                }
            };
            if spawn {
                let worker = CategoryWorker {
                    store: self.store,
                    subscription: self.subscription.clone(),
                    state: self.state.clone(),
                };
                self.executor
                    .spawn(Box::pin(async move { worker.run().await }));
            }
        }
        CategoryRequest {
            projection: self.clone(),
            completion,
            kind,
        }
    }
    pub fn categories(&self) -> CategoryRequest<'a, 'x, S, L, X> {
        self.request(ResultKind::Categories, false)
    }
    pub fn normalize(&self, category: Option<&JsString>) -> CategoryRequest<'a, 'x, S, L, X> {
        self.request(
            category
                .map(|v| ResultKind::Normalize(v.clone()))
                .unwrap_or(ResultKind::Null),
            false,
        )
    }
    pub fn revalidate(&self, category: &JsString) -> CategoryRequest<'a, 'x, S, L, X> {
        self.request(ResultKind::Normalize(category.clone()), true)
    }
}
struct CategoryWorker<'a, S> {
    store: &'a S,
    subscription: Json,
    state: Rc<RefCell<State>>,
}
impl<S: CategoryStore> CategoryWorker<'_, S> {
    async fn run(&self) {
        loop {
            let job = {
                let mut state = self.state.borrow_mut();
                match state.jobs.pop_front() {
                    Some(job) => Some(job),
                    None => {
                        state.running = false;
                        None
                    }
                }
            };
            let Some(job) = job else {
                let waiters = std::mem::take(&mut self.state.borrow_mut().waiters);
                for w in waiters {
                    w.wake();
                }
                return;
            };
            let result = if job.immediate {
                if !self.state.borrow().ready {
                    Err(pending(
                        "The Task category projection needs an out-of-gate rebuild.",
                    ))
                } else {
                    self.catch_up(None, false, Some(1), false).await
                }
            } else if self.state.borrow().ready {
                self.catch_up(None, false, None, true).await
            } else {
                self.warm().await
            };
            let waiters = {
                let mut completion = job.completion.borrow_mut();
                completion.result = Some(result);
                std::mem::take(&mut completion.waiters)
            };
            let drained = {
                let mut state = self.state.borrow_mut();
                state.pending -= 1;
                if state.pending == 0 {
                    std::mem::take(&mut state.waiters)
                } else {
                    vec![]
                }
            };
            for w in waiters.into_iter().chain(drained) {
                w.wake();
            }
            // Permit resolved callers and other admitted owner tasks to advance.
            let mut yielded = false;
            poll_fn(|cx| {
                if yielded {
                    Poll::Ready(())
                } else {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
        }
    }
    async fn rpc(&self, method: &str, args: Json) -> IvyResult<Json> {
        self.store
            .client()
            .request(method, args, RequestOptions::default())
            .await
    }
    async fn rebuild(&self) -> IvyResult<()> {
        let mut replacement = Entries::default();
        let mut cursor: Option<JsString> = None;
        loop {
            let mut options = page_options(200.0);
            if let Some(cursor) = &cursor {
                options.insert("cursor", Json::String(cursor.clone()));
            }
            let page = self.store.task_page(&options).await?;
            for item in array(f(&page, "items")) {
                replacement.insert(s(item, "objectId")?.clone(), entry(item)?);
            }
            cursor = match f(&page, "nextCursor") {
                Json::Null => None,
                Json::String(value) if !value.0.is_empty() => Some(value.clone()),
                _ => {
                    return Err(IvyError::new(
                        "task_board_category_projection_invalid",
                        "A projected Task page needs a nullable nonempty cursor.",
                    ));
                }
            };
            if cursor.is_none() {
                break;
            }
        }
        let mut state = self.state.borrow_mut();
        state.entries = replacement;
        state.ready = true;
        Ok(())
    }
    async fn apply(&self, batch: &Json) -> IvyResult<()> {
        if f(batch, "gap").as_object().is_some() {
            self.state.borrow_mut().ready = false;
            self.rebuild().await?;
            return Ok(());
        }
        let mut seen = HashSet::new();
        let mut ids = vec![];
        for event in array(f(batch, "items")) {
            if f(event, "topic") == &Json::from("hive.object.changed") {
                if let Some(id) = f(f(event, "payload"), "objectId").as_js_string() {
                    if seen.insert(id.clone()) {
                        ids.push(id.clone());
                    }
                }
            }
        }
        if ids.is_empty() {
            return Ok(());
        }
        let mut found = HashSet::new();
        for selected in ids.chunks(100) {
            let mut options = page_options(selected.len() as f64);
            options.insert(
                "where",
                Json::object([
                    ("op", "in".into()),
                    ("field", "object.id".into()),
                    (
                        "value",
                        Json::Array(selected.iter().cloned().map(Json::String).collect()),
                    ),
                ]),
            );
            let page = self.store.task_page(&options).await?;
            require(
                f(&page, "nextCursor").is_null(),
                "task_board_category_projection_invalid",
                "A bounded Task event lookup must fit one page.",
            )?;
            for item in array(f(&page, "items")) {
                let id = s(item, "objectId")?;
                found.insert(id.clone());
                if self
                    .state
                    .borrow()
                    .entries
                    .get(id)
                    .is_some_and(|v| v.revision > number(item, "revision"))
                {
                    continue;
                }
                let value = entry(item)?;
                self.state.borrow_mut().entries.insert(id.clone(), value);
            }
        }
        for id in ids {
            if found.contains(&id) || self.state.borrow().entries.get(&id).is_none() {
                continue;
            }
            match self
                .rpc(
                    "objects.stat",
                    Json::object([("objectId", Json::String(id.clone()))]),
                )
                .await
            {
                Ok(metadata) => {
                    let mut state = self.state.borrow_mut();
                    if f(&metadata, "contractKey") == &Json::from("task-board/task")
                        && f(&metadata, "parentId")
                            == &self
                                .store
                                .root_object_id()
                                .map(Json::String)
                                .unwrap_or(Json::Null)
                        && f(&metadata, "effectivelyArchived").as_bool() == Some(true)
                    {
                        let mut prior = state.entries.get(&id).unwrap().clone();
                        prior.revision = number(&metadata, "currentRevision");
                        prior.archived = true;
                        state.entries.insert(id, prior);
                    } else {
                        state.entries.remove(&id);
                    }
                }
                Err(e) if e.code == "not_found" => self.state.borrow_mut().entries.remove(&id),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    async fn catch_up(
        &self,
        initial: Option<&Json>,
        snapshot_covers_gap: bool,
        maximum: Option<usize>,
        rebuild_gaps: bool,
    ) -> IvyResult<()> {
        let head = self
            .rpc("events.head", Json::object(Vec::<(&str, Json)>::new()))
            .await?;
        let target = number(&head, "throughSequence");
        let mut previous = -1.0;
        let mut count = 0;
        while maximum.is_none_or(|max| count < max) {
            let batch = if count == 0 && initial.is_some() {
                initial.unwrap().clone()
            } else {
                self.rpc("events.subscribe", self.subscription.clone())
                    .await?
            };
            let gap = f(&batch, "gap");
            require(
                gap.is_null() || gap.as_object().is_some(),
                "task_board_category_projection_invalid",
                "A Task event batch needs a nullable gap object.",
            )?;
            let has_gap = !gap.is_null();
            if has_gap && !rebuild_gaps {
                return Err(pending(
                    "The Task category projection needs an out-of-gate rebuild.",
                ));
            }
            if !(count == 0 && snapshot_covers_gap && has_gap) {
                self.apply(&batch).await?;
            }
            let mut args = Json::object([
                ("name", f(&self.subscription, "name").clone()),
                ("throughSequence", f(&batch, "throughSequence").clone()),
            ]);
            if has_gap {
                args.insert(
                    "gapThroughSequence",
                    f(gap, "prunedThroughSequence").clone(),
                );
            }
            self.rpc("events.ack", args).await?;
            let through = number(&batch, "throughSequence");
            if through >= target {
                return Ok(());
            }
            require(
                through > previous,
                "task_board_cursor_regression",
                "The Task category journal did not advance toward its captured boundary.",
            )?;
            previous = through;
            count += 1;
        }
        Err(pending(
            "The Task category projection could not reach its finite replay boundary within this request.",
        ))
    }
    async fn warm(&self) -> IvyResult<()> {
        let first = self
            .rpc("events.subscribe", self.subscription.clone())
            .await?;
        self.rebuild().await?;
        self.catch_up(Some(&first), true, None, true).await
    }
}
