use super::*;
use ivy_contracts::schema::pointer_parts;
use rusqlite::{
    functions::FunctionFlags,
    types::{Value, ValueRef},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

const JSON_CACHE_BYTES: usize = 4 * 1024 * 1024;
const JSON_CACHE_ENTRY_BYTES: usize = 1024 * 1024;

#[cfg(test)]
#[path = "../../../../tests/rust/hive_query_cache.rs"]
mod cache_tests;

/// The process owner supplies the exact JavaScript runtime primitives. SQL
/// policy and pointer lookup remain in Hive; no locale/date fallback is used.
pub trait QueryRuntime: Send + Sync {
    fn parse_date(&self, value: &JsString) -> f64;
    fn locale_lowercase(&self, value: &JsString) -> IvyResult<JsString>;
}

#[derive(Default)]
struct State {
    deadline: Option<Instant>,
    cache: HashMap<String, IvyJson>,
    cache_bytes: usize,
    error: Option<IvyError>,
    runtime: Option<Arc<dyn QueryRuntime>>,
}
#[derive(Clone, Default)]
pub(super) struct QueryState(Arc<Mutex<State>>);
struct BudgetReset(QueryState);
impl Drop for BudgetReset {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap();
        state.deadline = None;
        state.cache.clear();
        state.cache_bytes = 0;
        state.error = None;
    }
}

impl State {
    fn check(&self) -> IvyResult<()> {
        require(
            !self.deadline.is_some_and(|d| Instant::now() > d),
            "limit_exceeded",
            "Database operation exceeded its work budget.",
        )
    }
    // Project while the cached or temporary tree is borrowed, so scalar SQL
    // functions never need to copy a selected string, object or array first.
    fn project<T>(
        &mut self,
        content: ValueRef<'_>,
        pointer: ValueRef<'_>,
        project: impl FnOnce(&IvyJson) -> IvyResult<T>,
    ) -> IvyResult<T> {
        self.check()?;
        let (ValueRef::Text(content), ValueRef::Text(pointer)) = (content, pointer) else {
            return project(&IvyJson::Null);
        };
        let content = std::str::from_utf8(content).map_err(sql_error)?;
        let pointer = std::str::from_utf8(pointer).map_err(sql_error)?;
        let mut uncached = None;
        if !self.cache.contains_key(content) {
            let Ok(root) = parse(content, usize::MAX) else {
                return project(&IvyJson::Null);
            };
            let bytes = content.len() + std::mem::size_of::<String>() + root.retained_bytes();
            if bytes > JSON_CACHE_ENTRY_BYTES {
                uncached = Some(root);
            } else {
                if self.cache.len() >= 8 || self.cache_bytes + bytes > JSON_CACHE_BYTES {
                    self.cache.clear();
                    self.cache_bytes = 0;
                }
                self.cache.insert(content.to_owned(), root);
                self.cache_bytes += bytes;
            }
        }
        let mut value = uncached.as_ref().unwrap_or_else(|| &self.cache[content]);
        for key in pointer_parts(&pointer.into())? {
            value = match value {
                IvyJson::Object(_) => match value.get_key(&key) {
                    Some(v) => v,
                    None => return project(&IvyJson::Null),
                },
                IvyJson::Array(items) => {
                    let ascii = key.to_utf8().unwrap_or_default();
                    let canonical_index = ascii == "0"
                        || (ascii
                            .as_bytes()
                            .first()
                            .is_some_and(|v| (b'1'..=b'9').contains(v))
                            && ascii.bytes().all(|b| b.is_ascii_digit()));
                    match canonical_index
                        .then(|| ascii.parse::<usize>().ok())
                        .flatten()
                        .and_then(|i| items.get(i))
                    {
                        Some(v) => v,
                        None => return project(&IvyJson::Null),
                    }
                }
                _ => return project(&IvyJson::Null),
            };
        }
        project(value)
    }
}

impl Store {
    pub fn bind_query_runtime(&self, runtime: Arc<dyn QueryRuntime>) {
        self.query_state.0.lock().unwrap().runtime = Some(runtime);
    }

    pub fn locale_lowercase(&self, value: &JsString) -> IvyResult<JsString> {
        let runtime = self
            .query_state
            .0
            .lock()
            .unwrap()
            .runtime
            .clone()
            .ok_or_else(IvyError::internal)?;
        runtime.locale_lowercase(value)
    }

    pub(super) fn install_query_functions(&self) -> IvyResult<()> {
        crate::contract_catalog::install_functions(&self.db).map_err(sql_error)?;
        let state = self.query_state.0.clone();
        self.db
            .create_scalar_function("ivy_budget", 0, FunctionFlags::SQLITE_UTF8, move |_| {
                let mut state = state.lock().unwrap();
                if let Err(error) = state.check() {
                    state.error = Some(error.clone());
                    return Err(rusqlite::Error::UserFunctionError(Box::new(error)));
                }
                Ok(1)
            })
            .map_err(sql_error)?;
        for name in ["ivy_type", "ivy_value", "ivy_is_null"] {
            let state = self.query_state.0.clone();
            self.db
                .create_scalar_function(name, 2, FunctionFlags::SQLITE_UTF8, move |context| {
                    let mut state = state.lock().unwrap();
                    let result = state.project(context.get_raw(0), context.get_raw(1), |value| {
                        Ok(match name {
                            "ivy_type" => Value::Integer(match value {
                                IvyJson::Bool(_) => 1,
                                IvyJson::Number(_) => 2,
                                IvyJson::String(_) => 3,
                                _ => 9,
                            }),
                            "ivy_is_null" => Value::Integer(i64::from(value.is_null())),
                            _ => match value {
                                IvyJson::Bool(v) => Value::Integer(i64::from(*v)),
                                IvyJson::Number(v) => Value::Real(*v),
                                IvyJson::String(v) => Value::Text(text(v)),
                                _ => Value::Null,
                            },
                        })
                    });
                    match result {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            state.error = Some(error.clone());
                            Err(rusqlite::Error::UserFunctionError(Box::new(error)))
                        }
                    }
                })
                .map_err(sql_error)?;
        }
        for (name, arguments) in [("ivy_contains", 3), ("ivy_recency", 2)] {
            let state = self.query_state.0.clone();
            self.db
                .create_scalar_function(
                    name,
                    arguments,
                    FunctionFlags::SQLITE_UTF8,
                    move |context| {
                        let mut state = state.lock().unwrap();
                        let runtime = state.runtime.clone();
                        let result =
                            state.project(context.get_raw(0), context.get_raw(1), |value| {
                                if name == "ivy_contains" {
                                    let (IvyJson::String(value), ValueRef::Text(needle)) =
                                        (value, context.get_raw(2))
                                    else {
                                        return Ok(0.0);
                                    };
                                    let lowered = runtime
                                        .as_ref()
                                        .ok_or_else(IvyError::internal)?
                                        .locale_lowercase(value)?;
                                    let needle = JsString::from(
                                        std::str::from_utf8(needle).map_err(sql_error)?,
                                    );
                                    return Ok(u8::from(
                                        needle.0.is_empty()
                                            || lowered
                                                .0
                                                .windows(needle.0.len())
                                                .any(|part| part == needle.0),
                                    ) as f64);
                                }
                                Ok(match value {
                                    IvyJson::Number(v)
                                        if v.is_finite()
                                            && v.fract() == 0.0
                                            && v.abs() <= 9_007_199_254_740_991.0 =>
                                    {
                                        *v
                                    }
                                    IvyJson::String(value) => {
                                        let parsed = runtime
                                            .as_ref()
                                            .ok_or_else(IvyError::internal)?
                                            .parse_date(value);
                                        if parsed.is_finite() { parsed } else { 0.0 }
                                    }
                                    _ => 0.0,
                                })
                            });
                        result.map_err(|error| {
                            state.error = Some(error.clone());
                            rusqlite::Error::UserFunctionError(Box::new(error))
                        })
                    },
                )
                .map_err(sql_error)?;
        }
        Ok(())
    }

    /// Preserve a callback's named error through SQLite's error boundary. Raw
    /// SQLite causes are observed only by an explicitly scoped local journal.
    pub fn query_error(&self, cause: rusqlite::Error) -> IvyError {
        let named = self.query_state.0.lock().unwrap().error.take();
        self.failure.record(|| {
            if let Some(error) = &named {
                return error.message_js();
            }
            match &cause {
                rusqlite::Error::SqliteFailure(_, Some(message))
                | rusqlite::Error::SqlInputError { msg: message, .. } => message.as_str().into(),
                _ => cause.to_string().into(),
            }
        });
        named.unwrap_or_else(IvyError::internal)
    }

    pub fn check_budget(&self) -> IvyResult<()> {
        self.query_state.0.lock().unwrap().check()
    }

    fn start_budget(&self, duration: Duration) -> BudgetReset {
        {
            let mut state = self.query_state.0.lock().unwrap();
            state.deadline = Instant::now().checked_add(duration);
            state.cache.clear();
            state.cache_bytes = 0;
            state.error = None;
        }
        BudgetReset(self.query_state.clone())
    }
    pub fn with_budget<T>(
        &self,
        duration: Duration,
        work: impl FnOnce(&Store) -> IvyResult<T>,
    ) -> IvyResult<T> {
        let _reset = self.start_budget(duration);
        work(self)
    }
    pub fn with_budget_mut<T>(
        &mut self,
        duration: Duration,
        work: impl FnOnce(&mut Store) -> IvyResult<T>,
    ) -> IvyResult<T> {
        let _reset = self.start_budget(duration);
        work(self)
    }
}
