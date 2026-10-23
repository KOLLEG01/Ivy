use super::*;
use ivy_contracts::{
    IvyJson,
    json::{JsString, parse, stringify},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn number(n: impl Into<f64>) -> IvyJson {
    IvyJson::Number(n.into())
}
fn array(values: impl IntoIterator<Item = IvyJson>) -> IvyJson {
    IvyJson::Array(values.into_iter().collect())
}
fn strings(values: &[&str]) -> IvyJson {
    array(values.iter().map(|s| (*s).into()))
}
fn fixture() -> ControlError {
    ControlError::Provider(Box::new(std::io::Error::other("fixture")))
}
fn base() -> Vec<ControlWindow> {
    vec![ControlWindow {
        handle: "123".into(),
        minimized: false,
        controls: vec![
            Control {
                id: vec![42, 1],
                parent_id: vec![42, 0],
                name: "Mute microphone".into(),
                enabled: true,
                offscreen: false,
                toggle: Some(0),
            },
            Control {
                id: vec![42, 2],
                parent_id: vec![42, 0],
                name: "Stop voice chat".into(),
                enabled: true,
                offscreen: false,
                toggle: None,
            },
        ],
    }]
}
fn windows_json(windows: Vec<ControlWindow>) -> IvyJson {
    ControlsObservation {
        observed: true,
        windows,
    }
    .to_json()
    .remove("windows")
    .unwrap()
}
fn labels(value: &IvyJson) -> Result<Vec<JsString>, ControlError> {
    value
        .as_array()
        .ok_or(ControlError::Labels)?
        .iter()
        .map(|v| v.as_js_string().cloned().ok_or(ControlError::Labels))
        .collect()
}
fn int(v: &IvyJson) -> Option<i32> {
    v.as_f64()
        .filter(|n| {
            n.is_finite()
                && n.fract() == 0.0
                && *n >= f64::from(i32::MIN)
                && *n <= f64::from(i32::MAX)
        })
        .map(|n| n as i32)
}
fn decode_windows(value: &IvyJson) -> Option<Vec<ControlWindow>> {
    let id = |v: &IvyJson| v.as_array()?.iter().map(int).collect::<Option<Vec<_>>>();
    value
        .as_array()?
        .iter()
        .map(|w| {
            Some(ControlWindow {
                handle: w.get("handle")?.as_js_string()?.clone(),
                minimized: w.get("minimized")?.as_bool()?,
                controls: w
                    .get("controls")?
                    .as_array()?
                    .iter()
                    .map(|c| {
                        Some(Control {
                            id: id(c.get("id")?)?,
                            parent_id: id(c.get("parentId")?)?,
                            name: c.get("name")?.as_js_string()?.clone(),
                            enabled: c.get("enabled")?.as_bool()?,
                            offscreen: c.get("offscreen")?.as_bool()?,
                            toggle: if c.get("toggle")?.is_null() {
                                None
                            } else {
                                Some(int(c.get("toggle")?)?)
                            },
                        })
                    })
                    .collect::<Option<Vec<_>>>()?,
            })
        })
        .collect()
}
fn classify(value: &IvyJson) -> IvyJson {
    let observation =
        decode_windows(value.get("windows").unwrap_or(&IvyJson::Null)).map(|windows| {
            ControlsObservation {
                observed: value.get("state").and_then(IvyJson::as_js_string)
                    == Some(&"observed".into()),
                windows,
            }
        });
    classify_voice(observation.as_ref()).to_json()
}
#[derive(Default)]
struct ReadState {
    trace: Vec<String>,
    calls: usize,
}
struct ReadSource {
    input: IvyJson,
    state: Arc<Mutex<ReadState>>,
}
impl ReadSource {
    fn stage(&self, name: &str) -> Result<(), ControlError> {
        self.state.lock().unwrap().trace.push(name.into());
        if self.input.get("fault").unwrap().as_js_string() == Some(&name.into()) {
            Err(fixture())
        } else {
            Ok(())
        }
    }
}
impl ControlSource for ReadSource {
    fn prepare(&mut self, _: &[JsString]) -> Result<(), ControlError> {
        self.stage("prepare")
    }
    fn read(&mut self, _: &[JsString]) -> Result<Vec<ControlWindow>, ControlError> {
        self.stage("read")?;
        decode_windows(self.input.get("windows").unwrap()).ok_or(ControlError::InvalidObservation)
    }
    fn close(&mut self) -> Result<(), ControlError> {
        self.stage("close")
    }
}
fn expected_read(input: &IvyJson) -> IvyJson {
    let state = Arc::new(Mutex::new(ReadState::default()));
    let current = || {
        let mut s = state.lock().unwrap();
        s.trace.push("current".into());
        let owners = input.get("owners").unwrap().as_array().unwrap();
        let v = owners[s.calls.min(owners.len() - 1)].as_bool().unwrap();
        s.calls += 1;
        v
    };
    let open = || {
        state.lock().unwrap().trace.push("open".into());
        if input.get("fault").unwrap().as_js_string() == Some(&"open".into()) {
            Err(fixture())
        } else {
            Ok(ReadSource {
                input: input.clone(),
                state: state.clone(),
            })
        }
    };
    let mut started = || {
        state.lock().unwrap().trace.push("started".into());
        if input.get("fault").unwrap().as_js_string() == Some(&"started".into()) {
            panic!("fixture started")
        }
    };
    let mut diagnostic = |_: &ControlError| {
        state.lock().unwrap().trace.push("diagnostic".into());
        if input.get("fault").unwrap().as_js_string() == Some(&"diagnostic".into()) {
            panic!("fixture diagnostic")
        }
    };
    let result = labels(input.get("labels").unwrap()).and_then(|labels| {
        read_controls(
            &labels,
            &current,
            open,
            Some(&mut diagnostic),
            Some(&mut started),
        )
    });
    let trace = array(state.lock().unwrap().trace.iter().map(|s| s.clone().into()));
    match result {
        Ok(result) => IvyJson::object([
            ("ok", true.into()),
            ("result", result.to_json()),
            ("trace", trace),
        ]),
        Err(e) => IvyJson::object([
            ("ok", false.into()),
            ("error", e.to_string().into()),
            ("trace", trace),
        ]),
    }
}
fn expected_selection(input: &IvyJson) -> IvyJson {
    let (mut ats, mut names) = (0, 0);
    let result = labels(input.get("labels").unwrap()).and_then(|labels| {
        select_named(
            int(input.get("count").unwrap()).unwrap(),
            |i| {
                ats += 1;
                if Some(i) == int(input.get("atFail").unwrap()) {
                    Err(fixture())
                } else {
                    Ok(i)
                }
            },
            |i| {
                names += 1;
                if Some(*i) == int(input.get("nameFail").unwrap()) {
                    return Err(fixture());
                }
                let values = input.get("names").unwrap().as_array().unwrap();
                let value = &values[*i as usize % values.len()];
                Ok(value.as_js_string().cloned())
            },
            &labels,
        )
    });
    let mut fields = match result {
        Ok(values) => vec![
            ("ok", true.into()),
            ("result", array(values.into_iter().map(number))),
        ],
        Err(e) => vec![("ok", false.into()), ("error", e.to_string().into())],
    };
    fields.extend([("at", number(ats)), ("names", number(names))]);
    IvyJson::object(fields)
}

struct VoiceState {
    windows: Vec<ControlWindow>,
    ticks: i64,
    delta: i64,
    current: bool,
    fail: bool,
    close_fail: bool,
    discoveries: i32,
    refreshes: i32,
    disposals: i32,
    received: Option<VoiceControlIdentity>,
}
struct Voice(Arc<Mutex<VoiceState>>);
impl ControlSource for Voice {
    fn read(&mut self, _: &[JsString]) -> Result<Vec<ControlWindow>, ControlError> {
        let mut s = self.0.lock().unwrap();
        s.discoveries += 1;
        s.ticks = 618;
        Ok(s.windows.clone())
    }
    fn close(&mut self) -> Result<(), ControlError> {
        let mut s = self.0.lock().unwrap();
        s.disposals += 1;
        if s.close_fail { Err(fixture()) } else { Ok(()) }
    }
}
impl VoiceSource for Voice {
    fn refresh(&mut self, id: VoiceControlIdentity) -> Result<Vec<ControlWindow>, ControlError> {
        let mut s = self.0.lock().unwrap();
        s.refreshes += 1;
        s.received = Some(id);
        let delta = s.delta;
        s.ticks += delta;
        if s.fail {
            Err(fixture())
        } else {
            Ok(s.windows.clone())
        }
    }
}
fn voice_state() -> Arc<Mutex<VoiceState>> {
    Arc::new(Mutex::new(VoiceState {
        windows: base(),
        ticks: 0,
        delta: 0,
        current: true,
        fail: false,
        close_fail: false,
        discoveries: 0,
        refreshes: 0,
        disposals: 0,
        received: None,
    }))
}
fn sessions() -> IvyJson {
    let state = voice_state();
    let origin = Instant::now();
    let now_state = state.clone();
    let current_state = state.clone();
    let now: Arc<dyn Fn() -> Instant + Send + Sync> =
        Arc::new(move || origin + Duration::from_millis(now_state.lock().unwrap().ticks as u64));
    let mut session = DesktopVoiceSession::open(
        || Ok(Voice(state.clone())),
        Arc::new(move || current_state.lock().unwrap().current),
        Some(now),
    )
    .unwrap();
    let mut rows = Vec::new();
    fn read(
        session: &mut DesktopVoiceSession<Voice>,
        state: &Arc<Mutex<VoiceState>>,
        origin: Instant,
        rows: &mut Vec<IvyJson>,
        action: &str,
    ) -> TimedVoiceControlObservation {
        let result = session.read().unwrap();
        let s = state.lock().unwrap();
        rows.push(IvyJson::object([
            ("action", action.into()),
            (
                "timestamp",
                number(result.timestamp.duration_since(origin).as_millis() as u32),
            ),
            ("observation", result.observation.to_json()),
            ("discoveries", number(s.discoveries)),
            ("refreshes", number(s.refreshes)),
            ("disposals", number(s.disposals)),
        ]));
        result
    }
    state.lock().unwrap().delta = 40;
    let mut first = read(&mut session, &state, origin, &mut rows, "active");
    first.observation.identity.as_mut().unwrap().microphone_id[1] = 90;
    state
        .lock()
        .unwrap()
        .received
        .as_mut()
        .unwrap()
        .microphone_id[1] = 91;
    read(
        &mut session,
        &state,
        origin,
        &mut rows,
        "snapshot_mutations",
    );
    {
        let mut s = state.lock().unwrap();
        s.windows[0].controls[0].name = "Unmute microphone".into();
        s.windows[0].controls[0].toggle = Some(1);
        s.delta = 0;
    }
    read(&mut session, &state, origin, &mut rows, "muted");
    state.lock().unwrap().windows = base();
    for (delta, action) in [
        (499, "under_limit"),
        (500, "at_limit"),
        (501, "over_limit"),
        (-1, "backwards"),
        (0, "fresh_again"),
    ] {
        state.lock().unwrap().delta = delta;
        read(&mut session, &state, origin, &mut rows, action);
    }
    for change in [
        "id",
        "parent",
        "disabled",
        "hidden",
        "toggle",
        "transition",
        "window",
        "minimized",
        "duplicate",
        "empty",
    ] {
        {
            let mut s = state.lock().unwrap();
            s.windows = base();
            match change {
                "id" => s.windows[0].controls[0].id = vec![42, 99],
                "parent" => s.windows[0].controls[0].parent_id = vec![42, 99],
                "disabled" => s.windows[0].controls[0].enabled = false,
                "hidden" => s.windows[0].controls[0].offscreen = true,
                "toggle" => s.windows[0].controls[0].toggle = None,
                "transition" => s.windows[0].controls[0].name = "Cancel voice chat".into(),
                "window" => s.windows[0].handle = "124".into(),
                "minimized" => s.windows[0].minimized = true,
                "duplicate" => s.windows[0].controls[1] = s.windows[0].controls[0].clone(),
                "empty" => s.windows.clear(),
                _ => unreachable!(),
            }
        }
        read(&mut session, &state, origin, &mut rows, change);
    }
    {
        let mut s = state.lock().unwrap();
        s.windows = base();
        s.current = false;
    }
    read(&mut session, &state, origin, &mut rows, "not_current");
    {
        let mut s = state.lock().unwrap();
        s.current = true;
        s.fail = true;
    }
    read(&mut session, &state, origin, &mut rows, "provider_failure");
    state.lock().unwrap().fail = false;
    let (returned, wrong_read, wrong_close) = std::thread::spawn(move || {
        let wrong_read = matches!(session.read(), Err(ControlError::WrongThread));
        let wrong_close = matches!(session.close(), Err(ControlError::WrongThread));
        (session, wrong_read, wrong_close)
    })
    .join()
    .unwrap();
    session = returned;
    read(
        &mut session,
        &state,
        origin,
        &mut rows,
        "after_foreign_thread",
    );
    state.lock().unwrap().close_fail = true;
    let close_failed = session.close().is_err();
    read(
        &mut session,
        &state,
        origin,
        &mut rows,
        "after_failed_close",
    );
    session.close().unwrap();
    let failed_state = voice_state();
    failed_state.lock().unwrap().windows.clear();
    let discovery_failed =
        DesktopVoiceSession::open(|| Ok(Voice(failed_state.clone())), Arc::new(|| true), None)
            .is_err();
    let mut opened = false;
    let old_owner_failed = DesktopVoiceSession::open(
        || {
            opened = true;
            Ok(Voice(voice_state()))
        },
        Arc::new(|| false),
        None,
    )
    .is_err();
    IvyJson::object([
        ("rows", array(rows)),
        ("wrongRead", wrong_read.into()),
        ("wrongClose", wrong_close.into()),
        ("closeFailed", close_failed.into()),
        ("disposals", number(state.lock().unwrap().disposals)),
        ("discoveryFailed", discovery_failed.into()),
        (
            "failedDiscoveryDisposals",
            number(failed_state.lock().unwrap().disposals),
        ),
        ("oldOwnerFailed", old_owner_failed.into()),
        ("opened", opened.into()),
    ])
}

#[test]
fn original_control_boundaries_classification_and_thread_bound_session_agree() {
    let valid = windows_json(base());
    let mut classifications = Vec::new();
    let mut windows_cases = vec![
        valid.clone(),
        IvyJson::Null,
        array([]),
        array([IvyJson::Null]),
    ];
    for handle in [
        "0",
        "-1",
        "+1",
        "01",
        "1",
        "9223372036854775807",
        "9223372036854775808",
        " 1",
        "1 ",
        "١",
        "🙂",
    ] {
        let mut w = base();
        w[0].handle = handle.into();
        windows_cases.push(windows_json(w));
    }
    for len in [0, 1, 32, 33] {
        for parent in [false, true] {
            let mut w = base();
            let ids = (0..len).collect();
            if parent {
                w[0].controls[0].parent_id = ids
            } else {
                w[0].controls[0].id = ids
            }
            windows_cases.push(windows_json(w));
        }
    }
    for name in voice_labels().into_iter().chain([
        "unrelated".into(),
        "mute microphone".into(),
        "Mute microphone ".into(),
        "Mute\0microphone".into(),
    ]) {
        for toggle in [None, Some(-1), Some(0), Some(1), Some(2), Some(3)] {
            for flags in 0..8 {
                let mut w = base();
                w[0].controls[0].name = name.clone();
                w[0].controls[0].toggle = toggle;
                w[0].controls[0].enabled = flags & 1 == 0;
                w[0].controls[0].offscreen = flags & 2 != 0;
                w[0].minimized = flags & 4 != 0;
                classifications.push(
                    ControlsObservation {
                        observed: true,
                        windows: w,
                    }
                    .to_json(),
                );
            }
        }
    }
    for count in [1, 2, 32, 33] {
        let mut w = Vec::new();
        for i in 0..count {
            w.push(ControlWindow {
                handle: (i + 1).to_string().into(),
                minimized: false,
                controls: Vec::new(),
            })
        }
        windows_cases.push(windows_json(w));
    }
    for name in voice_labels() {
        let mut w = base();
        w[0].controls = vec![Control {
            id: vec![9, 0],
            parent_id: vec![8],
            name: name.clone(),
            enabled: true,
            offscreen: false,
            toggle: None,
        }];
        windows_cases.push(windows_json(w));
    }
    for count in [64, 65] {
        let mut w = base();
        w[0].controls = (0..count)
            .map(|i| Control {
                id: vec![90, i],
                parent_id: vec![80],
                name: "Start voice chat".into(),
                enabled: true,
                offscreen: false,
                toggle: None,
            })
            .collect();
        windows_cases.push(windows_json(w));
    }
    for variant in [
        "sameParent",
        "sameId",
        "duplicateWindow",
        "duplicateControl",
        "splitWindows",
        "otherGroup",
        "extraMic",
        "extraStop",
        "hiddenStop",
    ] {
        let mut w = base();
        match variant {
            "sameParent" => w[0].controls[0].parent_id = w[0].controls[0].id.clone(),
            "sameId" => w[0].controls[1].id = w[0].controls[0].id.clone(),
            "duplicateWindow" => w.push(w[0].clone()),
            "duplicateControl" => {
                let duplicate = w[0].controls[0].clone();
                w[0].controls.push(duplicate);
            }
            "splitWindows" => {
                let control = w[0].controls.pop().unwrap();
                w.push(ControlWindow {
                    handle: "456".into(),
                    minimized: false,
                    controls: vec![control],
                })
            }
            "otherGroup" => w[0].controls[1].parent_id = vec![42, 8],
            "extraMic" | "extraStop" => {
                let mut c = w[0].controls[usize::from(variant == "extraStop")].clone();
                c.id = vec![42, 9];
                w[0].controls.push(c)
            }
            "hiddenStop" => {
                let mut c = w[0].controls[1].clone();
                c.id = vec![42, 9];
                c.offscreen = true;
                w[0].controls.push(c)
            }
            _ => unreachable!(),
        }
        windows_cases.push(windows_json(w));
    }
    classifications.extend(
        windows_cases
            .iter()
            .cloned()
            .map(|windows| IvyJson::object([("state", "observed".into()), ("windows", windows)])),
    );
    classifications.push(IvyJson::Null);
    classifications.push(IvyJson::object([
        ("state", "unknown".into()),
        ("windows", valid.clone()),
    ]));
    let names = array(voice_labels().into_iter().map(IvyJson::String));
    let mut label_cases = vec![
        names.clone(),
        IvyJson::Null,
        array([]),
        strings(&[""]),
        strings(&["same", "same"]),
        strings(&["Mute\0microphone"]),
        strings(&["same", "SAME"]),
        array([IvyJson::Null]),
        strings(&["\u{85}"]),
        strings(&["\u{feff}"]),
    ];
    for (text, count) in [("a", 256), ("a", 257), ("🙂", 128), ("🙂", 129)] {
        label_cases.push(array([text.repeat(count).into()]));
    }
    for count in [32, 33] {
        label_cases.push(array((0..count).map(|i| format!("label{i}").into())));
    }
    let mut reads = Vec::new();
    let add_read = |reads: &mut Vec<IvyJson>,
                    labels: IvyJson,
                    windows: IvyJson,
                    owners: IvyJson,
                    fault: &str| {
        reads.push(IvyJson::object([
            ("labels", labels),
            ("windows", windows),
            ("owners", owners),
            ("fault", fault.into()),
        ]))
    };
    for labels in &label_cases {
        add_read(
            &mut reads,
            labels.clone(),
            valid.clone(),
            array([true.into()]),
            "",
        );
    }
    for windows in &windows_cases {
        add_read(
            &mut reads,
            names.clone(),
            windows.clone(),
            array([true.into()]),
            "",
        );
    }
    for owners in [
        vec![false],
        vec![true, false],
        vec![true, true, false],
        vec![true, true, true],
    ] {
        for fault in [
            "",
            "open",
            "prepare",
            "started",
            "read",
            "close",
            "diagnostic",
        ] {
            add_read(
                &mut reads,
                names.clone(),
                valid.clone(),
                array(owners.iter().copied().map(IvyJson::Bool)),
                fault,
            );
        }
    }
    let mut selections = Vec::new();
    for count in [-1, 0, 1, 64, 65, 4096, 4097] {
        for candidates in [
            array(["other".into()]),
            array(["Start voice chat".into()]),
            array(["other".into(), "Stop voice chat".into(), IvyJson::Null]),
        ] {
            for fault in [-1, 0, 2] {
                selections.push(IvyJson::object([
                    ("count", number(count)),
                    ("labels", names.clone()),
                    ("names", candidates.clone()),
                    ("atFail", number(fault)),
                    ("nameFail", number(-1)),
                ]));
                selections.push(IvyJson::object([
                    ("count", number(count)),
                    ("labels", names.clone()),
                    ("names", candidates.clone()),
                    ("atFail", number(-1)),
                    ("nameFail", number(fault)),
                ]));
            }
        }
    }
    for labels in label_cases {
        selections.push(IvyJson::object([
            ("count", number(1)),
            ("labels", labels),
            ("names", array(["Start voice chat".into()])),
            ("atFail", number(-1)),
            ("nameFail", number(-1)),
        ]));
    }
    let input = IvyJson::object([
        ("reads", array(reads.clone())),
        ("classifications", array(classifications.clone())),
        ("selections", array(selections.clone())),
    ]);
    let encoded = stringify(&input, 8 * 1024 * 1024).unwrap();
    let trace = std::env::var("IVY_PHONE_DESKTOP_CONTROLS_TRACE").ok();
    if let Some(path) = &trace {
        std::fs::write(
            std::path::Path::new(path).with_extension("input.json"),
            &encoded,
        )
        .unwrap();
    }
    let actual = parse(
        include_str!("../fixtures/phone-desktop-controls-cases.json"),
        16 * 1024 * 1024,
    )
    .unwrap();
    assert_eq!(actual.get("labels"), Some(&names));
    assert_eq!(actual.get("starts"), Some(&strings(START_LABELS)));
    assert_eq!(actual.get("stops"), Some(&strings(STOP_LABELS)));
    for (name, expected) in [
        ("reads", reads.iter().map(expected_read).collect::<Vec<_>>()),
        (
            "classifications",
            classifications.iter().map(classify).collect(),
        ),
        (
            "selected",
            selections.iter().map(expected_selection).collect(),
        ),
    ] {
        let values = actual.get(name).unwrap().as_array().unwrap();
        assert_eq!(values.len(), expected.len());
        for (i, (a, e)) in values.iter().zip(expected).enumerate() {
            assert_eq!(a, &e, "{name}[{i}]");
        }
    }
    let expected_sessions = sessions();
    if let Some(path) = trace {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("readComparisons", number(reads.len() as u32)),
                    (
                        "classificationComparisons",
                        number(classifications.len() as u32),
                    ),
                    ("selectionComparisons", number(selections.len() as u32)),
                    ("sessions", expected_sessions),
                    ("original", actual),
                ]),
                16 * 1024 * 1024,
            )
            .unwrap(),
        )
        .unwrap();
    }
}
