//! Shared ownership of native read failures across in-flight requests.
use ivy_contracts::{IvyError, Outcome};
use std::{fmt, sync::Arc};
pub enum ReadCause<E> {
    Source(E),
    Ivy(IvyError),
}
pub struct ReadFailure<E>(Arc<ReadCause<E>>);
impl<E> ReadFailure<E> {
    pub fn source(error: E) -> Self {
        Self(Arc::new(ReadCause::Source(error)))
    }
    pub fn cause(&self) -> &ReadCause<E> {
        &self.0
    }
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl<E> Clone for ReadFailure<E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<E> fmt::Debug for ReadFailure<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.cause() {
            ReadCause::Source(_) => f.write_str("ReadFailure::Source(..)"),
            ReadCause::Ivy(error) => f.debug_tuple("ReadFailure::Ivy").field(error).finish(),
        }
    }
}
impl<E> From<IvyError> for ReadFailure<E> {
    fn from(error: IvyError) -> Self {
        Self(Arc::new(ReadCause::Ivy(error)))
    }
}
impl<E> crate::wake::WakeFailure for ReadFailure<E> {
    fn deleted(&self) -> bool {
        matches!(self.cause(), ReadCause::Ivy(error) if crate::wake::WakeFailure::deleted(error))
    }
}
pub fn provider_read_error<E>(error: ReadFailure<E>) -> IvyError {
    match error.cause() {
        ReadCause::Ivy(error) => error.clone(),
        ReadCause::Source(_) => IvyError::new("internal_error", "Service invocation failed.")
            .with_outcome(Outcome::Unknown),
    }
}
