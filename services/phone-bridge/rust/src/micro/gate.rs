//! Monitor-style reentry for Micro's caller predicate. Data locks are never
//! retained across it; other threads still serialize import and input writes.
use std::{cell::RefCell, sync::Mutex};
#[derive(Default)]
pub(super) struct Gate(Mutex<()>);
thread_local! { static ENTERED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) }; }
impl Gate {
    pub fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        let key = self as *const Self as usize;
        ENTERED.with(|entered| {
            if entered.borrow().contains(&key) {
                return work();
            }
            let _guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
            entered.borrow_mut().push(key);
            struct Reset<'a>(&'a RefCell<Vec<usize>>);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    self.0.borrow_mut().pop();
                }
            }
            let _reset = Reset(entered);
            work()
        })
    }
}
