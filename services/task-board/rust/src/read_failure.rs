//! Lossless owner read failures. Cloning an in-flight result retains the same
//! source payload; the caller still chooses its original catch boundary.
use ivy_contracts::{
    IvyError,
    error::{JavaScriptException, JavaScriptExceptionKind},
};
use ivy_sdk::task_comments::CommentException;
use std::{fmt, sync::Arc};

pub enum ReadCause<E> {
    Source(E),
    Ivy(IvyError),
    JavaScript(CommentException),
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
            ReadCause::JavaScript(error) => f
                .debug_tuple("ReadFailure::JavaScript")
                .field(error)
                .finish(),
        }
    }
}
impl<E> From<IvyError> for ReadFailure<E> {
    fn from(error: IvyError) -> Self {
        Self(Arc::new(ReadCause::Ivy(error)))
    }
}
impl<E> From<CommentException> for ReadFailure<E> {
    fn from(error: CommentException) -> Self {
        Self(Arc::new(ReadCause::JavaScript(error)))
    }
}
impl<E> From<JavaScriptException> for ReadFailure<E> {
    fn from(error: JavaScriptException) -> Self {
        CommentException {
            kind: match error.kind {
                JavaScriptExceptionKind::TypeError => "TypeError",
                JavaScriptExceptionKind::RangeError => "RangeError",
            },
            message: error.message,
        }
        .into()
    }
}
