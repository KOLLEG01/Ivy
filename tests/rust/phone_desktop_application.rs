use super::*;
use ivy_contracts::{IvyJson, json::stringify};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};
const APP: &str = "Phone.Fixture!App";
#[derive(Clone)]
struct Candidate {
    pid: u32,
    exited: bool,
    session: u32,
    user: bool,
    application: Option<Vec<u16>>,
    window: Raw,
    current: bool,
    fail: Option<&'static str>,
}
impl Candidate {
    fn new(pid: u32) -> Self {
        Self {
            pid,
            exited: false,
            session: 7,
            user: true,
            application: Some(APP.encode_utf16().collect()),
            window: 30,
            current: true,
            fail: None,
        }
    }
}
struct Opened {
    pid: u32,
    log: Arc<Mutex<Vec<String>>>,
}
impl Drop for Opened {
    fn drop(&mut self) {
        self.log.lock().unwrap().push(format!("{}:close", self.pid));
    }
}
struct Fake {
    rows: Vec<Candidate>,
    fail: Option<&'static str>,
    log: Arc<Mutex<Vec<String>>>,
}
impl Fake {
    fn new(rows: Vec<Candidate>) -> Self {
        Self {
            rows,
            fail: None,
            log: Arc::new(Mutex::new(vec![])),
        }
    }
    fn step(&self, pid: u32, name: &str) -> io::Result<Candidate> {
        self.log.lock().unwrap().push(format!("{pid}:{name}"));
        let row = self.rows.iter().find(|c| c.pid == pid).unwrap().clone();
        if row.fail == Some(name) {
            Err(unavailable())
        } else {
            Ok(row)
        }
    }
}
impl Source for Fake {
    type User = ();
    type Admission = Opened;
    fn candidates(&mut self) -> io::Result<Vec<u32>> {
        self.log.lock().unwrap().push("candidates".into());
        if self.fail == Some("candidates") {
            Err(unavailable())
        } else {
            Ok(self.rows.iter().map(|c| c.pid).collect())
        }
    }
    fn caller(&mut self) -> io::Result<(u32, ())> {
        self.log.lock().unwrap().push("caller".into());
        if self.fail == Some("caller") {
            Err(unavailable())
        } else {
            Ok((7, ()))
        }
    }
    fn exited(&mut self, pid: u32) -> io::Result<bool> {
        Ok(self.step(pid, "exited")?.exited)
    }
    fn session(&mut self, pid: u32) -> io::Result<u32> {
        Ok(self.step(pid, "session")?.session)
    }
    fn open(&mut self, pid: u32) -> io::Result<Opened> {
        self.step(pid, "open")?;
        Ok(Opened {
            pid,
            log: self.log.clone(),
        })
    }
    fn user_matches(&mut self, admitted: &Opened, _: &()) -> io::Result<bool> {
        Ok(self.step(admitted.pid, "user")?.user)
    }
    fn application(&mut self, admitted: &Opened) -> io::Result<Option<Vec<u16>>> {
        Ok(self.step(admitted.pid, "application")?.application)
    }
    fn window(&mut self, pid: u32) -> Raw {
        self.step(pid, "window").unwrap().window
    }
    fn identity(&mut self, pid: u32, application: &str) -> io::Result<AudioDesktopIdentity> {
        self.step(pid, "identity")?;
        AudioDesktopIdentity::new(
            pid as i32,
            504_911_232_000_001_234,
            Path::new(r"C:\App\ChatGPT.exe"),
            application,
        )
        .map_err(|_| unavailable())
    }
    fn current(&mut self, identity: &AudioDesktopIdentity) -> bool {
        self.step(identity.pid() as u32, "current").unwrap().current
    }
}
fn trace(name: &str, rows: &[IvyJson]) {
    if let Some(root) = std::env::var_os("IVY_PHONE_APPLICATION_TRACE") {
        fs::write(
            Path::new(&root).join(format!("{name}.json")),
            stringify(&IvyJson::Array(rows.to_vec()), 131072).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn candidate_admission_order_and_cleanup_preserve_original_observation() {
    assert_eq!(size_of::<Entry>(), 568);
    assert_eq!(std::mem::offset_of!(Entry, executable), 44);
    let mut rows = vec![];
    for (name, change, expected) in [
        (
            "ready",
            (|_: &mut Candidate| {}) as fn(&mut Candidate),
            "ready",
        ),
        ("exited", |c| c.exited = true, "absent"),
        ("other-session", |c| c.session = 8, "absent"),
        ("other-user", |c| c.user = false, "absent"),
        ("unpackaged", |c| c.application = None, "absent"),
        (
            "other-application",
            |c| c.application = Some("Other.Fixture!App".encode_utf16().collect()),
            "absent",
        ),
        (
            "application-case-sensitive",
            |c| c.application = Some(APP.to_lowercase().encode_utf16().collect()),
            "absent",
        ),
        (
            "unpaired-application",
            |c| c.application = Some(vec![0xd800]),
            "absent",
        ),
        ("no-window", |c| c.window = 0, "waiting"),
        ("owner-changed", |c| c.current = false, "unavailable"),
    ] {
        let mut c = Candidate::new(101);
        change(&mut c);
        let mut source = Fake::new(vec![c]);
        let observed = observe_with(APP, &mut source);
        assert_eq!(observed.state, expected, "{name}");
        let log = source.log.lock().unwrap().clone();
        let stage = |s: &str| log.contains(&format!("101:{s}"));
        assert_eq!(stage("open"), !matches!(name, "exited" | "other-session"));
        assert_eq!(stage("close"), stage("open"));
        assert_eq!(
            stage("application"),
            !matches!(name, "exited" | "other-session" | "other-user")
        );
        assert_eq!(stage("identity"), matches!(name, "ready" | "owner-changed"));
        if name == "ready" {
            assert_eq!(observed.identity.unwrap().pid(), 101);
            assert_eq!(
                log,
                vec![
                    "candidates",
                    "caller",
                    "101:exited",
                    "101:session",
                    "101:open",
                    "101:user",
                    "101:application",
                    "101:window",
                    "101:identity",
                    "101:current",
                    "101:close"
                ]
            );
        }
        rows.push(IvyJson::object([
            ("case", name.into()),
            ("state", observed.state.into()),
            (
                "trace",
                IvyJson::Array(log.into_iter().map(Into::into).collect()),
            ),
        ]));
    }
    for stage in [
        "exited",
        "session",
        "open",
        "user",
        "application",
        "identity",
    ] {
        let mut c = Candidate::new(101);
        c.fail = Some(stage);
        let mut source = Fake::new(vec![c]);
        let observed = observe_with(APP, &mut source);
        assert_eq!(observed.state, "unavailable");
        let log = source.log.lock().unwrap().clone();
        assert_eq!(
            log.iter().filter(|s| *s == "101:close").count(),
            usize::from(matches!(stage, "user" | "application" | "identity"))
        );
        rows.push(IvyJson::object([
            ("case", format!("failure:{stage}").into()),
            ("state", observed.state.into()),
            (
                "trace",
                IvyJson::Array(log.into_iter().map(Into::into).collect()),
            ),
        ]));
    }
    for stage in ["candidates", "caller"] {
        let mut source = Fake::new(vec![]);
        source.fail = Some(stage);
        assert_eq!(observe_with(APP, &mut source).state, "unavailable");
        rows.push(IvyJson::object([
            ("case", format!("failure:{stage}").into()),
            ("state", "unavailable".into()),
        ]));
    }
    for (name, cs, expected) in [
        ("empty", vec![], "absent"),
        (
            "two-windows",
            vec![Candidate::new(101), Candidate::new(102)],
            "ambiguous",
        ),
        (
            "window-and-waiting",
            {
                let mut c = Candidate::new(102);
                c.window = 0;
                vec![Candidate::new(101), c]
            },
            "ready",
        ),
        (
            "later-owner-lost",
            {
                let mut c = Candidate::new(102);
                c.current = false;
                vec![Candidate::new(101), c]
            },
            "unavailable",
        ),
        (
            "later-user-unavailable",
            {
                let mut c = Candidate::new(102);
                c.fail = Some("user");
                vec![Candidate::new(101), c]
            },
            "unavailable",
        ),
    ] {
        let mut source = Fake::new(cs);
        let observed = observe_with(APP, &mut source);
        assert_eq!(observed.state, expected);
        let log = source.log.lock().unwrap().clone();
        for pid in [101, 102] {
            assert_eq!(
                log.iter().filter(|s| **s == format!("{pid}:open")).count(),
                log.iter().filter(|s| **s == format!("{pid}:close")).count()
            );
        }
        rows.push(IvyJson::object([
            ("case", name.into()),
            ("state", observed.state.into()),
            (
                "trace",
                IvyJson::Array(log.into_iter().map(Into::into).collect()),
            ),
        ]));
    }
    for (name, expected) in [
        ("ChatGPT", true),
        ("CHATGPT.EXE", true),
        (r"C:\App\ChatGPT.exe", true),
        ("ChatGPT.foo", false),
        ("ChatGPT.exe.exe", false),
        ("notChatGPT.exe", false),
        ("", false),
    ] {
        assert_eq!(
            matches_name(&name.encode_utf16().collect::<Vec<_>>()),
            expected,
            "{name}"
        );
    }
    assert!(!matches_name(&[0xd800, 0x61]));
    trace("candidates", &rows);
    assert_eq!(rows.len(), 23);
}
