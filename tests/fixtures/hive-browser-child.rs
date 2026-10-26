//! The production Hive entry, with only its external push sender supplied by the test.
#[path = "../../services/hive/rust/src/native/mod.rs"]
mod native;

use ivy_contracts::{
    IvyError, IvyJson,
    json::{parse, stringify},
};
use ivy_hive::browser_push::{SendFuture, Sender, VapidDetails};
use std::{fs::OpenOptions, io::Write, path::PathBuf, sync::Arc};

struct PushSender(PathBuf);
impl Sender for PushSender {
    fn send(&self, subscription: IvyJson, payload: String, _: VapidDetails) -> SendFuture {
        let result = (|| {
            let status: u16 = std::fs::read_to_string(self.0.join("push-status"))
                .map_err(native::failure)?
                .trim()
                .parse()
                .map_err(native::failure)?;
            let value = IvyJson::object([
                ("endpoint", subscription.required("endpoint")?.clone()),
                ("payload", parse(&payload, 1024 * 1024)?),
            ]);
            let mut log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.0.join("push-deliveries.jsonl"))
                .map_err(native::failure)?;
            log.write_all(format!("{}\n", stringify(&value, 1024 * 1024)?).as_bytes())
                .map_err(native::failure)?;
            Ok(status)
        })();
        Box::pin(async { result })
    }
}
fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--storage-worker")
    {
        ivy_hive::worker_entry::run(
            ivy_hive::native_runtime::startup_ports(),
            std::rc::Rc::new(ivy_hive::browser_push_native::NativeCrypto),
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = tokio::task::LocalSet::new().block_on(
        &runtime,
        native::run(|| {
            let directory = PathBuf::from(
                std::env::var_os("IVY_TEST_BROWSER_PUSH_ROOT").ok_or_else(IvyError::internal)?,
            );
            if !directory.is_absolute() {
                return Err(IvyError::internal());
            }
            Ok(Arc::new(PushSender(directory)))
        }),
    );
    if let Err(error) = result {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}
