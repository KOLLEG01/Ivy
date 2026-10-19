//! An operation may journal a database cause without adding it to a wire error.
use super::*;
use std::cell::RefCell;

#[derive(Default)]
struct State {
    active: bool,
    message: Option<JsString>,
}
#[derive(Clone, Default)]
pub(super) struct FailureCause(Rc<RefCell<State>>);
struct Capture {
    state: Rc<RefCell<State>>,
    previous: State,
}
impl FailureCause {
    fn capture(&self) -> Capture {
        let previous = self.0.replace(State {
            active: true,
            message: None,
        });
        Capture {
            state: self.0.clone(),
            previous,
        }
    }
    pub(super) fn record(&self, message: impl FnOnce() -> JsString) {
        let mut state = self.0.borrow_mut();
        if state.active {
            // A rollback failure supersedes the work/COMMIT failure in the source.
            state.message = Some(message());
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.state.replace(std::mem::take(&mut self.previous));
    }
}
impl Store {
    /// Capture an original SQLite message for a local failure journal. The IvyError
    /// and its wire serialization remain unchanged. Named failures without a raw
    /// database cause use the error's own message at the journal's calling boundary.
    /// Nested captures and unwinding restore the enclosing observation scope.
    pub fn capture_failure<T>(
        &mut self,
        work: impl FnOnce(&mut Self) -> IvyResult<T>,
    ) -> (IvyResult<T>, Option<JsString>) {
        let capture = self.failure.capture();
        let result = work(self);
        let message = capture.state.borrow_mut().message.take();
        let failed = result.is_err();
        (result, message.filter(|_| failed))
    }
}
