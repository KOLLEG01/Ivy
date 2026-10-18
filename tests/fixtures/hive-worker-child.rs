//! Synthetic worker actions exercise the real framed entry/SQLite/OS boundary.
//! This executable is never a deployed Hive backend.
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::stringify};
use ivy_hive::{
    store::Store,
    worker_protocol::Notification,
    worker_server::{self, BackupTask, Progress, WorkerService},
};
use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
};
use std::{fs, io::Write, path::PathBuf, rc::Rc, time::Duration};

struct Service {
    store: Rc<Store>,
    root: PathBuf,
    notifications: Vec<Notification>,
}
impl WorkerService for Service {
    fn handle(&mut self, payload: IvyJson) -> IvyResult<IvyJson> {
        let action = payload.required("action")?.as_str()?;
        match action.as_str() {
            "health" => Ok(IvyJson::object([("ready", true.into())])),
            "echo" => {
                self.notifications.extend([
                    Notification::Routing(IvyJson::object([("node", "fixture".into())])),
                    Notification::EventsAvailable(IvyJson::object([(
                        "throughSequence",
                        IvyJson::Number(7.0),
                    )])),
                    Notification::Changes(vec!["objects".into()]),
                ]);
                Ok(payload.required("value")?.clone())
            }
            "hold" => {
                self.store.set_metadata("posted", "yes")?;
                fs::write(self.root.join("blocked"), b"started").unwrap();
                while !self.root.join("release").is_file() {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(true.into())
            }
            "write" => {
                self.store
                    .connection()
                    .execute(
                        "UPDATE metadata SET value=CAST(value AS INTEGER)+1 WHERE key='calls'",
                        [],
                    )
                    .unwrap();
                self.store
                    .connection()
                    .execute("UPDATE credentials SET revoked=1", [])
                    .unwrap();
                Ok(self.store.metadata("calls")?.unwrap().into())
            }
            "calls" => Ok(self.store.metadata("calls")?.unwrap().into()),
            "malformed" => {
                std::io::stdout()
                    .write_all(&u32::MAX.to_be_bytes())
                    .unwrap();
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(60));
                Ok(IvyJson::Null)
            }
            "exit" => std::process::exit(7),
            "error" => Err(IvyError::new_js(
                "fixture_error".into(),
                ivy_contracts::JsString(vec![0xd800, 120, 0xdc00]),
            )
            .with_outcome(ivy_contracts::Outcome::Completed)
            .with_details(payload.required("value")?.clone())),
            "close" => Ok(IvyJson::object([("closed", true.into())])),
            _ => panic!("Unknown synthetic worker action"),
        }
    }
    fn backup(&mut self, payload: IvyJson, progress: Progress) -> IvyResult<BackupTask> {
        let store = self.store.clone();
        let destination = PathBuf::from(payload.required("destination")?.as_str()?);
        let delay = payload
            .get("delayMs")
            .and_then(IvyJson::as_f64)
            .unwrap_or(0.0) as u64;
        Ok(Box::pin(async move {
            let mut target = Connection::open(destination).unwrap();
            let backup = Backup::new(store.connection(), &mut target).unwrap();
            let until = tokio::time::Instant::now() + Duration::from_millis(delay);
            while tokio::time::Instant::now() < until {
                progress();
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            loop {
                let step = backup.step(1).unwrap();
                progress();
                match step {
                    StepResult::Done => break,
                    StepResult::More => tokio::time::sleep(Duration::from_millis(2)).await,
                    other => panic!("Unexpected test backup state: {other:?}"),
                }
            }
            let pages = backup.progress().pagecount;
            drop(backup);
            Ok(IvyJson::object([("pages", IvyJson::Number(pages as f64))]))
        }))
    }
    fn notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.notifications)
    }
}
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(worker_server::serve(tokio::io::stdin(), tokio::io::stdout(), |options| {
        let root = PathBuf::from(options.required("root")?.as_str()?);
        let store = Store::open(&root.join("hive.sqlite"))?;
        store.set_metadata("calls", &store.metadata("calls")?.unwrap_or_else(|| "0".into()))?;
        store.connection().execute_batch("CREATE TABLE IF NOT EXISTS fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); INSERT OR IGNORE INTO fixture_payload SELECT value,zeroblob(4096) FROM json_each('[1,2,3,4,5,6,7,8]');").unwrap();
        fs::write(root.join("pid"), std::process::id().to_string()).unwrap();
        if options.get("neverReady") == Some(&IvyJson::Bool(true)) { loop { std::thread::park(); } }
        Ok(Service { store: Rc::new(store), root, notifications: vec![] })
    }));
    // Dropped kernel/backup closes SQLite before exit. Tokio stdin uses a
    // blocking reader; private worker exit must not wait for another input byte.
    if let Err(error) = &result {
        eprintln!(
            "{}",
            stringify(&IvyJson::from(error.kind().to_string()), 1024).unwrap()
        );
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}
