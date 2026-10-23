use super::*;
use ivy_contracts::{IvyJson, json::stringify};
use std::{collections::VecDeque, fs, sync::Mutex};
const PID: u32 = 44444444;
const CREATED: u64 = 1234;
struct State {
    absent: bool,
    metadata_error: bool,
    exited: bool,
    exit_error: bool,
    sessions: [Option<u32>; 2],
    created: u64,
    image: PathBuf,
    window: isize,
    window_sequence: VecDeque<isize>,
    foreground: isize,
    iconic: bool,
    restore: bool,
    activate: bool,
    processed: bool,
    lose_after: usize,
    reads: usize,
    trace: Vec<&'static str>,
    windows: Vec<(isize, u32, isize, bool)>,
    enumerate_failure: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            absent: false,
            metadata_error: false,
            exited: false,
            exit_error: false,
            sessions: [Some(7), Some(7)],
            created: CREATED,
            image: PathBuf::from(r"C:\APP\ChatGPT.exe"),
            window: 30,
            window_sequence: VecDeque::new(),
            foreground: 40,
            iconic: false,
            restore: true,
            activate: true,
            processed: true,
            lose_after: usize::MAX,
            reads: 0,
            trace: Vec::new(),
            windows: vec![],
            enumerate_failure: false,
        }
    }
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(STATE.lock().unwrap().as_mut().unwrap())
}
fn unavailable() -> io::Error {
    io::Error::other("Synthetic observation unavailable")
}
fn process(pid: u32) -> io::Result<Option<ProcessIdentity>> {
    assert_eq!(pid, PID);
    with(|s| {
        s.trace.push("process");
        if s.metadata_error {
            Err(unavailable())
        } else if s.absent {
            Ok(None)
        } else {
            Ok(Some(ProcessIdentity {
                pid,
                created_filetime: s.created,
                image: s.image.clone(),
            }))
        }
    })
}
fn exited(pid: u32, created: u64) -> io::Result<bool> {
    assert_eq!(pid, PID);
    with(|s| {
        assert_eq!(created, s.created);
        s.trace.push("exit");
        s.reads += 1;
        if s.exit_error {
            Err(unavailable())
        } else {
            Ok(s.exited || s.reads > s.lose_after)
        }
    })
}
fn read_session(pid: u32) -> io::Result<u32> {
    with(|s| {
        let i = if pid == PID {
            0
        } else {
            assert_eq!(pid, std::process::id());
            1
        };
        s.trace.push(if i == 0 {
            "target_session"
        } else {
            "caller_session"
        });
        s.sessions[i].ok_or_else(unavailable)
    })
}
fn window(pid: u32) -> isize {
    assert_eq!(pid, PID);
    with(|s| {
        s.trace.push("window");
        s.window_sequence.pop_front().unwrap_or(s.window)
    })
}
fn foreground() -> isize {
    with(|s| {
        s.trace.push("foreground");
        s.foreground
    })
}
fn iconic(window: isize) -> bool {
    assert_eq!(window, 30);
    with(|s| {
        s.trace.push("iconic");
        s.iconic
    })
}
fn restore(window: isize) -> bool {
    assert_eq!(window, 30);
    with(|s| {
        s.trace.push("restore");
        s.restore
    })
}
fn activate(window: isize) -> bool {
    assert_eq!(window, 30);
    with(|s| {
        s.trace.push("activate");
        if s.activate {
            s.foreground = window
        };
        s.activate
    })
}
fn processed(window: isize) -> bool {
    assert_eq!(window, 30);
    with(|s| {
        s.trace.push("processed");
        s.processed
    })
}
fn owner(state: State) -> WindowsDesktopProcessOwner {
    *STATE.lock().unwrap() = Some(state);
    let mut owner = WindowsDesktopProcessOwner::new(
        PID as i32,
        (FILETIME_TICKS + CREATED) as i64,
        Path::new(r"C:\app\ChatGPT.exe"),
    )
    .unwrap();
    owner.native = Native {
        process,
        exited,
        session: read_session,
        window,
        foreground,
        iconic,
        restore,
        activate,
        processed,
    };
    owner
}
fn trace(name: &str, rows: &[IvyJson]) {
    if let Some(root) = std::env::var_os("IVY_PHONE_DESKTOP_PROCESS_TRACE") {
        fs::write(
            Path::new(&root).join(format!("{name}.json")),
            stringify(&IvyJson::Array(rows.to_vec()), 131072).unwrap(),
        )
        .unwrap();
    }
}
unsafe extern "system" fn enumerate(visit: WindowCallback, data: isize) -> i32 {
    let (windows, fail) = with(|s| (s.windows.clone(), s.enumerate_failure));
    if fail {
        return 0;
    }
    for (window, _, _, _) in windows {
        if unsafe { visit(window, data) } == 0 {
            return 0;
        }
    }
    1
}
unsafe extern "system" fn window_pid(window: isize, pid: *mut u32) -> u32 {
    let value = with(|s| {
        s.trace.push("window_pid");
        s.windows.iter().find(|r| r.0 == window).unwrap().1
    });
    unsafe { *pid = value };
    1
}
unsafe extern "system" fn window_owner(window: isize, command: u32) -> isize {
    assert_eq!(command, 4);
    with(|s| s.windows.iter().find(|r| r.0 == window).unwrap().2)
}
unsafe extern "system" fn visible(window: isize) -> i32 {
    with(|s| s.windows.iter().find(|r| r.0 == window).unwrap().3 as i32)
}
#[test]
fn process_identity_window_and_focus_callbacks_preserve_original_checks() {
    let mut rows = Vec::new();
    let cases: [(&str, fn(&mut State), bool); 14] = [
        ("matching", |_| {}, true),
        ("absent", |s| s.absent = true, false),
        ("metadata-error", |s| s.metadata_error = true, false),
        ("exited", |s| s.exited = true, false),
        ("exit-unavailable", |s| s.exit_error = true, false),
        ("foreign-session", |s| s.sessions[0] = Some(8), false),
        (
            "target-session-unavailable",
            |s| s.sessions[0] = None,
            false,
        ),
        (
            "caller-session-unavailable",
            |s| s.sessions[1] = None,
            false,
        ),
        ("pid-reused", |s| s.created += 1, false),
        ("creation-overflow", |s| s.created = u64::MAX, false),
        ("window-missing", |s| s.window = 0, false),
        (
            "foreign-image",
            |s| s.image = PathBuf::from(r"C:\other\ChatGPT.exe"),
            false,
        ),
        (
            "normalized-image",
            |s| s.image = PathBuf::from(r"C:\app\..\APP\ChatGPT.exe"),
            true,
        ),
        (
            "ordinal-image",
            |s| s.image = PathBuf::from(r"c:\app\chatgpt.EXE"),
            true,
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut s = State::default();
        mutate(&mut s);
        let mut target = owner(s);
        assert_eq!(target.is_current().unwrap(), expected, "{name}");
        assert!(!with(|s| s
            .trace
            .iter()
            .any(|c| matches!(*c, "restore" | "activate" | "processed"))));
        rows.push(IvyJson::object([
            ("case", name.into()),
            ("current", expected.into()),
        ]));
    }
    // Focused observations must use the fresh main window after currentness.
    let mut target = owner(State {
        foreground: 30,
        ..State::default()
    });
    assert!(target.is_focused().unwrap());
    let mut target = owner(State {
        foreground: 30,
        window_sequence: VecDeque::from([30, 31]),
        ..State::default()
    });
    assert!(!target.is_focused().unwrap());
    for (name, mutate, expected, restores, activations, processed) in [
        (
            "already-foreground",
            (|s: &mut State| s.foreground = 30) as fn(&mut State),
            true,
            0,
            0,
            0,
        ),
        ("normal-activation", |_| {}, true, 0, 1, 1),
        ("minimized", |s| s.iconic = true, true, 1, 1, 1),
        (
            "restore-failed",
            |s| {
                s.iconic = true;
                s.restore = false
            },
            false,
            1,
            0,
            0,
        ),
        ("activation-failed", |s| s.activate = false, false, 0, 1, 0),
        ("processing-failed", |s| s.processed = false, false, 0, 1, 1),
        (
            "owner-lost-before-entry",
            |s| s.lose_after = 0,
            false,
            0,
            0,
            0,
        ),
        (
            "owner-lost-before-confirm",
            |s| s.lose_after = 1,
            false,
            0,
            0,
            0,
        ),
        (
            "owner-lost-before-activation",
            |s| s.lose_after = 2,
            false,
            0,
            0,
            0,
        ),
        (
            "owner-lost-after-processing",
            |s| s.lose_after = 3,
            false,
            0,
            1,
            1,
        ),
    ] {
        let mut s = State::default();
        mutate(&mut s);
        let mut target = owner(s);
        assert_eq!(target.try_focus().unwrap(), expected, "{name}");
        let calls = with(|s| s.trace.clone());
        for (call, count) in [
            ("restore", restores),
            ("activate", activations),
            ("processed", processed),
        ] {
            assert_eq!(
                calls.iter().filter(|x| **x == call).count(),
                count,
                "{name}:{call}"
            );
        }
        rows.push(IvyJson::object([
            ("case", name.into()),
            ("focused", expected.into()),
            (
                "trace",
                IvyJson::Array(calls.into_iter().map(Into::into).collect()),
            ),
        ]));
    }
    let mut s = State::default();
    s.windows = vec![
        (1, PID + 1, 0, true),
        (2, PID, 99, true),
        (3, PID, 0, false),
        (4, PID, 0, true),
        (5, PID, 0, true),
    ];
    owner(s);
    let native = Windows {
        enumerate,
        pid: window_pid,
        owner: window_owner,
        visible,
    };
    assert_eq!(main_window_with(PID, native), 4);
    assert_eq!(
        with(|s| s.trace.iter().filter(|c| **c == "window_pid").count()),
        4
    );
    with(|s| s.enumerate_failure = true);
    assert_eq!(main_window_with(PID, native), 0);
    assert!(WindowsDesktopProcessOwner::new(0, 1, Path::new(r"C:\a.exe")).is_err());
    assert!(WindowsDesktopProcessOwner::new(1, 0, Path::new(r"C:\a.exe")).is_err());
    assert!(WindowsDesktopProcessOwner::new(1, 1, Path::new("a.exe")).is_err());
    trace("callbacks", &rows);
    assert_eq!(rows.len(), 24);
}
