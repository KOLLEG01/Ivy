use super::*;
use crate::desktop::{
    AudioProcessOwnership, AudioProcessRecord, AudioProcessSnapshot, CaptureObservation,
    CaptureSession, DesktopCaptureCorrelation,
};

#[derive(Clone)]
enum Value {
    Record(AudioProcessRecord),
    Null,
    Error,
}
#[derive(Clone)]
struct Script {
    name: String,
    kind: &'static str,
    pid: i32,
    reads: Vec<(i32, Value)>,
    current: Vec<bool>,
    captures: Vec<Option<CaptureObservation>>,
    open: &'static str,
    close_error: bool,
    expected: &'static str,
}
fn record(pid: i32, parent: i32, ticks: i64) -> AudioProcessRecord {
    AudioProcessRecord {
        pid,
        parent_pid: parent,
        session_id: 2,
        start_ticks: ticks,
        image_path: Some(r"C:\Fixture\App.exe".into()),
    }
}
fn root() -> AudioProcessRecord {
    record(101, 0, 100)
}
fn child() -> AudioProcessRecord {
    record(202, 101, 200)
}
fn value(v: &Value) -> IvyJson {
    match v {
        Value::Null => IvyJson::Null,
        Value::Error => "error".into(),
        Value::Record(v) => IvyJson::object([
            ("pid", IvyJson::Number(v.pid.into())),
            ("parent", IvyJson::Number(v.parent_pid.into())),
            ("session", IvyJson::Number(v.session_id.into())),
            ("ticks", v.start_ticks.to_string().into()),
            (
                "image",
                v.image_path
                    .as_ref()
                    .map_or(IvyJson::Null, |p| p.to_str().unwrap().into()),
            ),
        ]),
    }
}
impl Script {
    fn process(name: &str, records: Vec<AudioProcessRecord>, expected: &'static str) -> Self {
        Self {
            name: name.into(),
            kind: "process",
            pid: 202,
            reads: records
                .into_iter()
                .map(|r| (r.pid, Value::Record(r)))
                .collect(),
            current: vec![true, true],
            captures: vec![],
            open: "ok",
            close_error: false,
            expected,
        }
    }
    fn json(&self) -> IvyJson {
        IvyJson::object([
            ("name", self.name.clone().into()),
            ("kind", self.kind.into()),
            ("pid", IvyJson::Number(self.pid.into())),
            ("open", self.open.into()),
            ("closeError", self.close_error.into()),
            (
                "current",
                IvyJson::Array(self.current.iter().map(|b| (*b).into()).collect()),
            ),
            (
                "reads",
                IvyJson::Array(
                    self.reads
                        .iter()
                        .map(|(pid, v)| {
                            IvyJson::object([
                                ("pid", IvyJson::Number((*pid).into())),
                                ("value", value(v)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "captures",
                IvyJson::Array(
                    self.captures
                        .iter()
                        .map(|v| {
                            v.as_ref()
                                .map_or(IvyJson::Null, CaptureObservation::to_json)
                        })
                        .collect(),
                ),
            ),
            ("expected", self.expected.into()),
        ])
    }
}
#[derive(Default)]
struct State {
    trace: Vec<String>,
    reads: usize,
    current: usize,
    capture: usize,
    open: usize,
    close: usize,
}
#[derive(Clone)]
struct Source {
    script: Script,
    state: Arc<Mutex<State>>,
}
impl AudioProcessSnapshot for Source {
    fn read(&mut self, pid: i32) -> ActionResult<Option<AudioProcessRecord>> {
        let mut state = self.state.lock().unwrap();
        let index = state.reads;
        state.reads += 1;
        state.trace.push(format!("read:{pid}"));
        let Some((expected, value)) = self.script.reads.get(index) else {
            return Err(DesktopActionError::Unavailable);
        };
        if pid != *expected {
            return Err(DesktopActionError::Unavailable);
        }
        match value {
            Value::Null => Ok(None),
            Value::Error => Err(DesktopActionError::Unavailable),
            Value::Record(v) => Ok(Some(v.clone())),
        }
    }
    fn close(&mut self) -> ActionResult<()> {
        let mut state = self.state.lock().unwrap();
        state.close += 1;
        state.trace.push("dispose".into());
        if self.script.close_error {
            Err(DesktopActionError::Unavailable)
        } else {
            Ok(())
        }
    }
}
fn run(script: &Script) -> IvyJson {
    let state = Arc::new(Mutex::new(State::default()));
    let current = || {
        let mut s = state.lock().unwrap();
        let n = s.current;
        s.current += 1;
        s.trace.push("current".into());
        Ok(script.current.get(n).copied().unwrap_or(true))
    };
    let open = || {
        let mut s = state.lock().unwrap();
        s.open += 1;
        s.trace.push("open".into());
        match script.open {
            "error" => Err(DesktopActionError::Unavailable),
            "null" => Ok(None),
            _ => Ok(Some(Source {
                script: script.clone(),
                state: state.clone(),
            })),
        }
    };
    let identity =
        AudioDesktopIdentity::new(101, 100, Path::new(r"C:\Fixture\App.exe"), APP).unwrap();
    let result = if script.kind == "process" {
        AudioProcessOwnership::read(
            &identity,
            script.pid,
            &current,
            open,
            &crate::media::image_equal,
        )
        .map(|v| v.to_json())
    } else {
        DesktopCaptureCorrelation::read(
            &identity,
            &"endpoint".into(),
            &current,
            |_| {
                let mut s = state.lock().unwrap();
                let n = s.capture;
                s.capture += 1;
                s.trace.push("capture".into());
                script
                    .captures
                    .get(n)
                    .cloned()
                    .ok_or(DesktopActionError::Unavailable)
            },
            open,
            &crate::media::image_equal,
        )
        .map(|v| v.to_json())
    };
    let (result, error) = match result {
        Ok(v) => (v, IvyJson::Null),
        Err(e) => (IvyJson::Null, error_code(e).into()),
    };
    let s = state.lock().unwrap();
    IvyJson::object([
        ("result", result),
        ("error", error),
        (
            "trace",
            IvyJson::Array(s.trace.iter().cloned().map(Into::into).collect()),
        ),
        ("reads", IvyJson::Number(s.reads as f64)),
        ("current", IvyJson::Number(s.current as f64)),
        ("capture", IvyJson::Number(s.capture as f64)),
        ("open", IvyJson::Number(s.open as f64)),
        ("close", IvyJson::Number(s.close as f64)),
    ])
}
fn ownership_records() -> Vec<AudioProcessRecord> {
    vec![root(), child(), root(), child(), root(), root()]
}
fn observation(pids: &[u32]) -> CaptureObservation {
    CaptureObservation {
        endpoint_id: "endpoint".into(),
        observed: true,
        sessions: pids
            .iter()
            .enumerate()
            .map(|(i, pid)| CaptureSession::new(format!("session-{i}").into(), 1, *pid, 0).unwrap())
            .collect(),
    }
}
fn matched() -> Script {
    let mut records = ownership_records();
    records.extend([child(), root(), child()]);
    let mut s = Script::process("capture-matched", records, "matched");
    s.kind = "capture";
    s.current = vec![true; 5];
    s.captures = vec![Some(observation(&[202])); 2];
    s
}
fn cases() -> Vec<Script> {
    let mut all = vec![Script::process("owned", ownership_records(), "owned")];
    let mut ownroot = Script::process("root", vec![root(); 4], "owned");
    ownroot.pid = 101;
    all.push(ownroot);
    for field in ["session", "image", "parent"] {
        let mut foreign = child();
        match field {
            "session" => foreign.session_id = 3,
            "image" => foreign.image_path = Some(r"C:\Other\App.exe".into()),
            _ => foreign.parent_pid = 0,
        };
        all.push(Script::process(
            &format!("foreign-{field}"),
            vec![root(), foreign.clone(), foreign, root()],
            "foreign",
        ));
    }
    for field in ["pid", "ticks", "image", "null"] {
        let mut pinned = root();
        match field {
            "pid" => pinned.pid = 102,
            "ticks" => pinned.start_ticks = 101,
            "image" => pinned.image_path = Some(r"C:\Other.exe".into()),
            _ => pinned.image_path = None,
        };
        let mut s = Script::process(&format!("root-{field}"), vec![], "unavailable");
        s.reads = vec![(101, Value::Record(pinned))];
        s.current = vec![true];
        all.push(s);
    }
    for field in ["pid", "ticks", "image", "null"] {
        let mut bad = child();
        match field {
            "pid" => bad.pid = 203,
            "ticks" => bad.start_ticks = 0,
            "image" => bad.image_path = Some(" ".into()),
            _ => bad.image_path = None,
        };
        let mut s = Script::process(&format!("child-{field}"), vec![], "unavailable");
        s.reads = vec![(101, Value::Record(root())), (202, Value::Record(bad))];
        s.current = vec![true];
        all.push(s);
    }
    let mut cycle = child();
    cycle.parent_pid = 202;
    let mut s = Script::process("cycle", vec![root(), cycle], "unavailable");
    s.current = vec![true];
    all.push(s);
    let mut newer = root();
    newer.start_ticks = 300;
    let mut s = Script::process("newer-parent", vec![root(), child(), newer], "unavailable");
    s.current = vec![true];
    all.push(s);
    for index in [3, 5] {
        let mut records = ownership_records();
        records[index].parent_pid += 1;
        records.truncate(index + 1);
        let mut s = Script::process(&format!("changed-{index}"), records, "unavailable");
        s.current = vec![true];
        all.push(s);
    }
    for value in [Value::Null, Value::Error] {
        let mut s = Script::process("read-failure", vec![], "unavailable");
        s.reads = vec![(101, value)];
        s.current = vec![true];
        all.push(s);
    }
    for open in ["null", "error"] {
        let mut s = Script::process(&format!("open-{open}"), vec![], "unavailable");
        s.open = open;
        s.current = vec![true];
        all.push(s);
    }
    let mut s = Script::process("initial-not-current", vec![], "unavailable");
    s.current = vec![false];
    all.push(s);
    let mut s = Script::process("late-not-current", ownership_records(), "unavailable");
    s.current = vec![true, false];
    all.push(s);
    let mut s = Script::process("dispose-failure", ownership_records(), "unavailable");
    s.close_error = true;
    all.push(s);
    let mut s = Script::process("invalid-pid", vec![], "invalid_request");
    s.pid = 0;
    s.current = vec![];
    all.push(s);
    let mut records = ownership_records();
    for r in &mut records {
        r.image_path = Some(r"c:\fixture\APP.EXE".into());
    }
    all.push(Script::process("path-casing", records, "owned"));
    for length in [63, 64] {
        let chain: Vec<_> = (0..length)
            .map(|i| {
                record(
                    1000 + i,
                    if i == length - 1 { 101 } else { 1001 + i },
                    200 - i as i64,
                )
            })
            .collect();
        let mut reads = vec![root()];
        reads.extend(chain.clone());
        if length == 63 {
            reads.push(root());
        }
        reads.extend(chain);
        if length == 63 {
            reads.push(root());
        }
        reads.push(root());
        let mut s = Script::process(
            &format!("depth-{length}"),
            reads,
            if length == 63 { "owned" } else { "unavailable" },
        );
        s.pid = 1000;
        all.push(s);
    }
    all.push(matched());
    let mut s = matched();
    s.name = "capture-shared-pid".into();
    s.expected = "ambiguous";
    s.captures = vec![Some(observation(&[202, 202])); 2];
    all.push(s);
    // UTF-16 ordinal order visits the astral ID before U+E000. The final
    // inventory arrives reversed; inactive processes never enter the cache.
    let mut s = matched();
    s.name = "capture-mixed-pids-ordinal-order".into();
    let mut foreign = record(303, 0, 300);
    foreign.session_id = 3;
    let mut records = ownership_records();
    records.extend([
        child(),
        root(),
        foreign.clone(),
        foreign.clone(),
        root(),
        root(),
        child(),
        foreign,
    ]);
    s.reads = records
        .into_iter()
        .map(|r| (r.pid, Value::Record(r)))
        .collect();
    s.current = vec![true; 7];
    let mut initial = observation(&[303, 202]);
    initial.sessions[0].instance_id = "\u{e000}".into();
    initial.sessions[1].instance_id = "\u{10000}".into();
    initial
        .sessions
        .push(CaptureSession::new("inactive".into(), 0, u32::MAX, 0).unwrap());
    let mut final_capture = initial.clone();
    final_capture.sessions.reverse();
    s.captures = vec![Some(initial), Some(final_capture)];
    all.push(s);
    for mode in [
        "empty",
        "empty-null",
        "ambiguous",
        "pid-overflow",
        "initial-not-current",
        "capture-null",
        "unavailable",
        "active-overflow",
        "duplicates",
    ] {
        let mut s = matched();
        s.name = format!("capture-{mode}");
        s.reads = vec![];
        s.current = vec![true];
        s.expected = "unavailable";
        match mode {
            "empty" | "empty-null" => {
                s.captures = vec![Some(observation(&[])); 2];
                s.current = vec![true; 3];
                s.expected = "none";
                if mode == "empty-null" {
                    s.open = "null";
                }
            }
            "ambiguous" => {
                let mut o = observation(&[202]);
                o.sessions[0].single_process = false;
                s.captures = vec![Some(o)];
                s.expected = "ambiguous";
            }
            "pid-overflow" => {
                s.captures = vec![Some(observation(&[u32::MAX]))];
                s.expected = "ambiguous";
            }
            "initial-not-current" => {
                s.current = vec![false];
                s.captures = vec![];
            }
            "capture-null" => s.captures = vec![None],
            "unavailable" => {
                let mut o = observation(&[]);
                o.observed = false;
                s.captures = vec![Some(o)];
            }
            "active-overflow" => s.captures = vec![Some(observation(&vec![202; 33]))],
            _ => {
                let mut o = observation(&[202, 202]);
                o.sessions[1].instance_id = o.sessions[0].instance_id.clone();
                s.captures = vec![Some(o)];
            }
        }
        all.push(s);
    }
    for mode in [
        "final-changed",
        "cache-changed",
        "owned-disappeared",
        "dispose-error",
        "current-after-dispose",
        "current-before-dispose",
    ] {
        let mut s = matched();
        s.name = format!("capture-{mode}");
        s.expected = "unavailable";
        match mode {
            "final-changed" => {
                s.reads.truncate(7);
                s.current.truncate(3);
                s.captures[1].as_mut().unwrap().sessions[0].instance_id = "changed".into();
            }
            "cache-changed" => {
                s.reads.truncate(8);
                if let Value::Record(r) = &mut s.reads[7].1 {
                    r.parent_pid += 1;
                }
                s.current.truncate(3);
            }
            "owned-disappeared" => {
                s.reads.truncate(7);
                s.reads[6].1 = Value::Null;
                s.current.truncate(3);
                s.captures.truncate(1);
            }
            "dispose-error" => {
                s.close_error = true;
                s.current.truncate(4);
            }
            "current-after-dispose" => s.current[4] = false,
            _ => {
                s.current.truncate(4);
                s.current[3] = false;
            }
        }
        all.push(s);
    }
    all
}
fn check(script: &Script, result: &IvyJson) {
    let actual = result
        .get("error")
        .unwrap()
        .as_str()
        .ok()
        .unwrap_or_else(|| {
            result
                .get("result")
                .unwrap()
                .get("state")
                .unwrap()
                .as_str()
                .unwrap()
        });
    assert_eq!(actual, script.expected, "{}", script.name);
    assert_eq!(
        result.get("reads").unwrap().as_f64(),
        Some(script.reads.len() as f64),
        "{}",
        script.name
    );
    assert_eq!(
        result.get("current").unwrap().as_f64(),
        Some(script.current.len() as f64),
        "{}",
        script.name
    );
    assert_eq!(
        result.get("capture").unwrap().as_f64(),
        Some(script.captures.len() as f64),
        "{}",
        script.name
    );
}
#[test]
fn scripted_ancestry_and_capture_correlation() {
    let rows: Vec<_> = cases()
        .iter()
        .map(|s| {
            let result = run(s);
            check(s, &result);
            IvyJson::object([("input", s.json()), ("rust", result)])
        })
        .collect();
    save("process-ownership-ordinary.json", &IvyJson::Array(rows));
}
