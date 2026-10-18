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

#[derive(Default)]
struct State {
    deadline: Option<Instant>,
    cache: HashMap<String, IvyJson>,
    error: Option<IvyError>,
}
#[derive(Clone, Default)]
pub(super) struct QueryState(Arc<Mutex<State>>);
struct BudgetReset(QueryState);
impl Drop for BudgetReset {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap();
        state.deadline = None;
        state.cache.clear();
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
    fn value(&mut self, content: ValueRef<'_>, pointer: ValueRef<'_>) -> IvyResult<IvyJson> {
        self.check()?;
        let (ValueRef::Text(content), ValueRef::Text(pointer)) = (content, pointer) else {
            return Ok(IvyJson::Null);
        };
        let content = std::str::from_utf8(content).map_err(sql_error)?;
        let pointer = std::str::from_utf8(pointer).map_err(sql_error)?;
        if !self.cache.contains_key(content) {
            let Ok(root) = parse(content, usize::MAX) else {
                return Ok(IvyJson::Null);
            };
            if self.cache.len() >= 8 {
                self.cache.clear();
            }
            self.cache.insert(content.to_string(), root);
        }
        let mut value = &self.cache[content];
        for key in pointer_parts(&pointer.into())? {
            value = match value {
                IvyJson::Object(_) => match value.get_key(&key) {
                    Some(v) => v,
                    None => return Ok(IvyJson::Null),
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
                        None => return Ok(IvyJson::Null),
                    }
                }
                _ => return Ok(IvyJson::Null),
            };
        }
        Ok(value.clone())
    }
}

impl Store {
    pub(super) fn install_query_functions(&self) -> IvyResult<()> {
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
                    let value = match state.value(context.get_raw(0), context.get_raw(1)) {
                        Ok(value) => value,
                        Err(error) => {
                            state.error = Some(error.clone());
                            return Err(rusqlite::Error::UserFunctionError(Box::new(error)));
                        }
                    };
                    Ok(match name {
                        "ivy_type" => Value::Integer(match value {
                            IvyJson::Bool(_) => 1,
                            IvyJson::Number(_) => 2,
                            IvyJson::String(_) => 3,
                            _ => 9,
                        }),
                        "ivy_is_null" => Value::Integer(i64::from(value.is_null())),
                        _ => match value {
                            IvyJson::Bool(v) => Value::Integer(i64::from(v)),
                            IvyJson::Number(v) => Value::Real(v),
                            IvyJson::String(ref v) => Value::Text(text(v)),
                            _ => Value::Null,
                        },
                    })
                })
                .map_err(sql_error)?;
        }
        Ok(())
    }

    /// Preserve a callback's named error through SQLite's error boundary.
    pub fn query_error(&self, _: rusqlite::Error) -> IvyError {
        self.query_state
            .0
            .lock()
            .unwrap()
            .error
            .take()
            .unwrap_or_else(IvyError::internal)
    }

    pub fn check_budget(&self) -> IvyResult<()> {
        self.query_state.0.lock().unwrap().check()
    }

    fn start_budget(&self, duration: Duration) -> BudgetReset {
        {
            let mut state = self.query_state.0.lock().unwrap();
            state.deadline = Instant::now().checked_add(duration);
            state.cache.clear();
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
