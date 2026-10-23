use super::*;
use ivy_contracts::IvyJson;
use std::{collections::VecDeque, path::Path, sync::Mutex};

struct State {
    trace: Vec<String>,
    waits: VecDeque<u32>,
    open_null: bool,
    parent: Option<u32>,
    failure: &'static str,
    created: u64,
    session: u32,
    close_failure: Option<u32>,
    opens: Vec<(u32, i32, u32)>,
    closes: Vec<u32>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            trace: vec![],
            waits: VecDeque::new(),
            open_null: false,
            parent: Some(101),
            failure: "",
            created: 1234,
            session: 7,
            close_failure: None,
            opens: vec![],
            closes: vec![],
        }
    }
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(STATE.lock().unwrap().as_mut().unwrap())
}
unsafe extern "system" fn open(access: u32, inherit: i32, pid: u32) -> HANDLE {
    with(|s| {
        s.trace.push(format!("open:{pid}"));
        s.opens.push((access, inherit, pid));
        if s.open_null {
            std::ptr::null_mut()
        } else {
            pid as usize as HANDLE
        }
    })
}
unsafe extern "system" fn wait(handle: HANDLE, timeout: u32) -> u32 {
    with(|s| {
        s.trace.push(format!("wait:{}:{timeout}", handle as usize));
        s.waits.pop_front().unwrap_or(WAIT_TIMEOUT)
    })
}
unsafe extern "system" fn close(handle: HANDLE) -> i32 {
    with(|s| {
        let pid = handle as usize as u32;
        s.trace.push(format!("close:{pid}"));
        s.closes.push(pid);
        i32::from(s.close_failure != Some(pid))
    })
}
fn failure() -> io::Error {
    io::Error::other("Inert process query failed")
}
fn parent(pid: u32, limit: usize) -> io::Result<Option<u32>> {
    with(|s| {
        s.trace.push(format!("parent:{pid}:{limit}"));
        if s.failure == "parent" {
            Err(failure())
        } else {
            Ok(s.parent)
        }
    })
}
struct Process(u32);
impl ProcessMetadata for Process {
    fn module(&self) -> io::Result<Option<PathBuf>> {
        let pid = self.0;
        with(|s| {
            s.trace.push(format!("module:{pid}"));
            match s.failure {
                "module" => Err(failure()),
                "module-null" => Ok(None),
                _ => Ok(Some(r"C:\Module\App.exe".into())),
            }
        })
    }
    fn session(&mut self) -> io::Result<u32> {
        let pid = self.0;
        with(|s| {
            s.trace.push(format!("session:{pid}"));
            if s.failure == "session" {
                Err(failure())
            } else {
                Ok(s.session)
            }
        })
    }
    fn created(&mut self) -> io::Result<u64> {
        let pid = self.0;
        with(|s| {
            s.trace.push(format!("created:{pid}"));
            if s.failure == "created" {
                Err(failure())
            } else {
                Ok(s.created)
            }
        })
    }
    fn exited(&mut self) -> io::Result<bool> {
        let pid = self.0;
        with(|s| {
            s.trace.push(format!("exited:{pid}"));
            if s.failure == "exit-error" {
                Err(failure())
            } else {
                Ok(s.failure == "exited")
            }
        })
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        with(|s| s.trace.push(format!("process-dispose:{}", self.0)));
    }
}
fn process(pid: u32) -> io::Result<Option<Box<dyn ProcessMetadata>>> {
    with(|s| {
        s.trace.push(format!("process:{pid}"));
        match s.failure {
            "process" => Err(failure()),
            "process-null" => Ok(None),
            _ => Ok(Some(Box::new(Process(pid)) as Box<dyn ProcessMetadata>)),
        }
    })
}
fn snapshot(state: State) -> WindowsAudioProcessSnapshot {
    *STATE.lock().unwrap() = Some(state);
    WindowsAudioProcessSnapshot {
        native: Native {
            open,
            wait,
            close,
            parent,
            process,
        },
        handles: HashMap::new(),
        order: vec![],
        closed: false,
    }
}
fn record_json(r: &AudioProcessRecord) -> IvyJson {
    IvyJson::object([
        ("pid", IvyJson::Number(r.pid.into())),
        ("parentPid", IvyJson::Number(r.parent_pid.into())),
        ("sessionId", IvyJson::Number(r.session_id.into())),
        ("startTicks", r.start_ticks.to_string().into()),
        (
            "imagePath",
            r.image_path.as_ref().unwrap().to_str().unwrap().into(),
        ),
    ])
}
fn row(name: &str, result: Option<AudioProcessRecord>) -> IvyJson {
    with(|s| {
        assert!(
            s.opens
                .iter()
                .all(|(flags, inherit, _)| *flags == 0x00101000 && *inherit == 0)
        );
        assert!(
            s.trace
                .iter()
                .filter(|v| v.starts_with("wait:"))
                .all(|v| v.ends_with(":0"))
        );
        assert!(
            s.trace
                .iter()
                .filter(|v| v.starts_with("parent:"))
                .all(|v| v.ends_with(":65536"))
        );
        let trace = if s.trace.len() > 64 {
            s.trace
                .iter()
                .take(12)
                .chain(s.trace.iter().skip(s.trace.len() - 6))
                .cloned()
                .collect()
        } else {
            s.trace.clone()
        };
        IvyJson::object([
            ("case", name.into()),
            ("result", result.as_ref().map_or(IvyJson::Null, record_json)),
            (
                "trace",
                IvyJson::Array(trace.into_iter().map(Into::into).collect()),
            ),
            ("events", IvyJson::Number(s.trace.len() as f64)),
            ("opens", IvyJson::Number(s.opens.len() as f64)),
            ("closes", IvyJson::Number(s.closes.len() as f64)),
        ])
    })
}
pub(crate) fn verify_audio_process_native() -> IvyJson {
    let mut rows = vec![];
    for name in [
        "success",
        "open-null",
        "pre-signaled",
        "pre-failed",
        "post-signaled",
        "parent-null",
        "parent-overflow",
        "parent-zero",
        "parent",
        "module",
        "module-null",
        "session",
        "process",
        "process-null",
        "created",
        "exit-error",
        "exited",
        "ticks-overflow",
        "ticks-range",
    ] {
        let mut state = State::default();
        match name {
            "open-null" => state.open_null = true,
            "pre-signaled" => state.waits.push_back(0),
            "pre-failed" => state.waits.push_back(u32::MAX),
            "post-signaled" => state.waits.extend([WAIT_TIMEOUT, 0]),
            "parent-null" => state.parent = None,
            "parent-overflow" => state.parent = Some(u32::MAX),
            "parent-zero" => state.parent = Some(0),
            "ticks-overflow" => state.created = u64::MAX,
            "ticks-range" => state.created = MAX_DATE_TICKS - FILETIME_TICKS + 1,
            "success" => {}
            _ => state.failure = name,
        }
        let mut snapshot = snapshot(state);
        let result = snapshot.read(202).unwrap();
        assert_eq!(
            result.is_some(),
            matches!(name, "success" | "parent-zero"),
            "{name}"
        );
        if let Some(r) = &result {
            assert_eq!(
                r.image_path.as_deref(),
                Some(Path::new(r"C:\Module\App.exe"))
            );
            assert_eq!(r.start_ticks, 504_911_232_000_001_234);
        }
        snapshot.close().unwrap();
        snapshot.close().unwrap();
        drop(snapshot);
        with(|s| assert_eq!(s.closes.len(), usize::from(name != "open-null"), "{name}"));
        if name == "success" {
            with(|s| {
                assert_eq!(
                    s.trace,
                    vec![
                        "open:202",
                        "wait:202:0",
                        "process:202",
                        "parent:202:65536",
                        "module:202",
                        "session:202",
                        "created:202",
                        "exited:202",
                        "wait:202:0",
                        "process-dispose:202",
                        "close:202"
                    ]
                )
            });
        }
        rows.push(row(name, result));
    }
    let mut s = snapshot(State::default());
    let first = s.read(202).unwrap().unwrap();
    let second = s.read(202).unwrap().unwrap();
    assert!(first.same(Some(&second), &image_equal));
    assert_eq!(s.handles.len(), 1);
    s.close().unwrap();
    with(|s| {
        assert_eq!(s.opens.len(), 1);
        assert_eq!(s.closes, vec![202]);
    });
    rows.push(row("retained-repeat", Some(second)));
    let mut s = snapshot(State::default());
    assert!(matches!(s.read(0), Err(DesktopActionError::Argument(_))));
    assert!(s.handles.is_empty());
    s.close().unwrap();
    assert_eq!(s.read(0).unwrap_err(), DesktopActionError::Unavailable);
    rows.push(row("disposed-before-pid-validation", None));
    let mut state = State::default();
    state.close_failure = Some(202);
    let mut s = snapshot(state);
    s.read(202).unwrap();
    s.read(101).unwrap();
    assert!(s.close().is_err());
    assert!(s.handles.is_empty());
    s.close().unwrap();
    drop(s);
    with(|s| assert_eq!(s.closes, vec![202, 101]));
    rows.push(row("all-close-after-failure", None));
    {
        let mut s = snapshot(State::default());
        s.read(202).unwrap();
    }
    with(|s| assert_eq!(s.closes, vec![202]));
    rows.push(row("drop-closes", None));
    let mut s = snapshot(State::default());
    for pid in 1..=4096 {
        assert!(s.read(pid).unwrap().is_some());
    }
    assert!(s.read(1).unwrap().is_some());
    assert_eq!(s.read(4097).unwrap_err(), DesktopActionError::Unavailable);
    s.close().unwrap();
    with(|s| {
        assert_eq!(s.opens.len(), 4096);
        assert_eq!(s.closes, (1..=4096).collect::<Vec<_>>());
    });
    rows.push(row("4096-pins-bound", None));
    IvyJson::Array(rows)
}
