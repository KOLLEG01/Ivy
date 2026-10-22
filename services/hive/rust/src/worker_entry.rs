//! Private process entry. The executable owner supplies qualified providers.
use crate::{
    browser_push_store::Crypto, kernel::StartupPorts, worker_kernel::KernelWorker, worker_server,
};
use std::{cell::RefCell, rc::Rc};

/// Run only in the owned storage-child mode of the service executable.
/// No provider, private flag, package dispatcher or network listener is chosen
/// here. In particular, this function must never run in the parent process.
pub fn run(ports: StartupPorts, crypto: Rc<dyn Crypto>) -> ! {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Storage process runtime");
    let failure = RefCell::new(None);
    let result = runtime.block_on(worker_server::serve(
        tokio::io::stdin(),
        tokio::io::stdout(),
        |data| match KernelWorker::open_worker_data(data, ports, crypto) {
            Ok(worker) => Ok(worker),
            Err(error) => {
                let wire = std::borrow::Borrow::<ivy_contracts::IvyError>::borrow(&error).clone();
                failure.replace(Some(error));
                Err(wire)
            }
        },
    ));
    let code = if result.is_err() || failure.borrow().is_some() {
        1
    } else {
        0
    };
    // Keep a failed constructor's same connection until OS exit, as in the
    // original uncaught worker error. Do not drop failure or wait for Tokio's
    // blocking stdin reader while the parent's input pipe is still open.
    std::process::exit(code)
}
