use ivy_contracts::{IvyError, IvyResult};

/// Keep an uncoded JavaScript exception distinct until the original catch site.
/// Public component helpers still normalize it through IvyError::from semantics.
#[derive(Debug)]
pub(crate) enum SchedulerFailure {
    Ivy(IvyError),
    Uncoded,
}
pub(crate) type SchedulerResult<T> = Result<T, SchedulerFailure>;
impl From<IvyError> for SchedulerFailure {
    fn from(error: IvyError) -> Self {
        Self::Ivy(error)
    }
}
impl From<SchedulerFailure> for IvyError {
    fn from(error: SchedulerFailure) -> Self {
        match error {
            SchedulerFailure::Ivy(error) => error,
            SchedulerFailure::Uncoded => IvyError::internal(),
        }
    }
}
impl SchedulerFailure {
    pub(crate) fn uncoded(_: IvyError) -> Self {
        Self::Uncoded
    }
    pub(crate) fn code(&self, fallback: &str) -> String {
        match self {
            Self::Ivy(error) => error.code.clone(),
            Self::Uncoded => fallback.into(),
        }
    }
}
pub(crate) fn untyped<T>(value: IvyResult<T>) -> SchedulerResult<T> {
    value.map_err(SchedulerFailure::uncoded)
}
