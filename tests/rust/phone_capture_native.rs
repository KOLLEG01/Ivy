use super::*;
use ivy_contracts::IvyJson;
use std::cell::RefCell;

#[repr(C)]
struct Object {
    table: *const (),
    index: usize,
}
#[derive(Default)]
struct State {
    mode: &'static str,
    pointers: [Raw; 7],
    refs: [i32; 7],
    allocations: i32,
    trace: Vec<String>,
    abi_ok: bool,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
fn step(name: &str) -> Hr {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.trace.push(name.into());
        if s.mode == name {
            0x80004005u32 as i32
        } else if s.mode == format!("positive:{name}") {
            1
        } else {
            0
        }
    })
}
fn output(name: &str, index: usize, out: *mut Raw) -> Hr {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.mode == format!("null:{name}") {
            unsafe {
                *out = ptr::null_mut();
            }
        } else {
            s.refs[index] += 1;
            unsafe {
                *out = s.pointers[index];
            }
        }
    });
    step(name)
}
unsafe extern "system" fn query(this: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    let index = unsafe { (*this.cast::<Object>()).index };
    let (name, target, expected) = if index == 1 {
        ("query:endpoint", 2, &ENDPOINT)
    } else {
        ("query:session", 6, &SESSION)
    };
    STATE.with(|s| {
        s.borrow_mut().abi_ok &= unsafe { (*iid).data1 == expected.data1 };
    });
    output(name, target, out)
}
unsafe extern "system" fn add_ref(_: Raw) -> u32 {
    0
}
unsafe extern "system" fn release(this: Raw) -> u32 {
    let index = unsafe { (*this.cast::<Object>()).index };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.refs[index] -= 1;
        s.trace.push(format!("release:{index}"));
    });
    0
}
unsafe extern "system" fn initialize(reserved: Raw, mode: u32) -> Hr {
    STATE.with(|s| {
        s.borrow_mut().abi_ok &= reserved.is_null() && mode == 0;
    });
    step("initialize")
}
unsafe extern "system" fn uninitialize() {
    step("uninitialize");
}
unsafe extern "system" fn create(
    class: *const GUID,
    outer: Raw,
    context: u32,
    iid: *const GUID,
    out: *mut Raw,
) -> Hr {
    STATE.with(|s| {
        s.borrow_mut().abi_ok &=
            unsafe { (*class).data1 == CLASS.data1 && (*iid).data1 == DEVICES.data1 }
                && outer.is_null()
                && context == 23;
    });
    output("create", 0, out)
}
unsafe extern "system" fn get(_: Raw, id: *const u16, out: *mut Raw) -> Hr {
    let expected: Vec<_> = "capture-fixture".encode_utf16().chain([0]).collect();
    STATE.with(|s| {
        s.borrow_mut().abi_ok &=
            unsafe { std::slice::from_raw_parts(id, expected.len()) == expected };
    });
    output("get", 1, out)
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemAlloc(bytes: usize) -> Raw;
}
fn wide(name: &str, text: &str, out: *mut *mut u16) -> Hr {
    let mode = STATE.with(|s| s.borrow().mode);
    if mode == format!("null:{name}") {
        unsafe {
            *out = ptr::null_mut();
        }
        return step(name);
    }
    let text: Vec<_> = text.encode_utf16().chain([0]).collect();
    let raw = unsafe { CoTaskMemAlloc(text.len() * 2) }.cast::<u16>();
    if raw.is_null() {
        return 0x8007000eu32 as i32;
    }
    unsafe {
        ptr::copy_nonoverlapping(text.as_ptr(), raw, text.len());
        *out = raw;
    }
    STATE.with(|s| s.borrow_mut().allocations += 1);
    step(name)
}
unsafe extern "system" fn free(raw: Raw) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.allocations -= 1;
        s.trace.push("free".into());
    });
    unsafe {
        CoTaskMemFree(raw);
    }
}
unsafe extern "system" fn id(_: Raw, out: *mut *mut u16) -> Hr {
    let wrong = STATE.with(|s| s.borrow().mode == "wrong-id");
    wide("id", if wrong { "other" } else { "capture-fixture" }, out)
}
unsafe extern "system" fn state(_: Raw, out: *mut u32) -> Hr {
    unsafe {
        *out = if STATE.with(|s| s.borrow().mode == "wrong-state") {
            0
        } else {
            1
        };
    }
    step("state")
}
unsafe extern "system" fn flow(_: Raw, out: *mut i32) -> Hr {
    unsafe {
        *out = if STATE.with(|s| s.borrow().mode == "wrong-flow") {
            0
        } else {
            1
        };
    }
    step("flow")
}
unsafe extern "system" fn activate(
    _: Raw,
    iid: *const GUID,
    context: u32,
    parameters: Raw,
    out: *mut Raw,
) -> Hr {
    STATE.with(|s| {
        s.borrow_mut().abi_ok &=
            unsafe { (*iid).data1 == MANAGER.data1 } && context == 23 && parameters.is_null()
    });
    output("activate", 3, out)
}
unsafe extern "system" fn sessions(_: Raw, out: *mut Raw) -> Hr {
    output("sessions", 4, out)
}
unsafe extern "system" fn count(_: Raw, out: *mut i32) -> Hr {
    unsafe {
        *out = STATE.with(|s| match s.borrow().mode {
            "over-count" => 4097,
            "negative-count" => -1,
            _ => 1,
        });
    }
    step("count")
}
unsafe extern "system" fn at(_: Raw, index: i32, out: *mut Raw) -> Hr {
    STATE.with(|s| s.borrow_mut().abi_ok &= index == 0);
    output("at", 5, out)
}
unsafe extern "system" fn instance(_: Raw, out: *mut *mut u16) -> Hr {
    wide("instance", "session-fixture", out)
}
unsafe extern "system" fn process(_: Raw, out: *mut u32) -> Hr {
    let mode = STATE.with(|s| s.borrow().mode);
    unsafe {
        *out = if mode == "zero-pid" { 0 } else { 101 };
    }
    let hr = step("process");
    if mode == "multi-process" {
        0x0889000d
    } else {
        hr
    }
}
unsafe extern "system" fn session_state(_: Raw, out: *mut i32) -> Hr {
    unsafe {
        *out = if STATE.with(|s| s.borrow().mode == "invalid-session-state") {
            3
        } else {
            1
        };
    }
    step("session-state")
}
const UNKNOWN: Unknown = Unknown {
    query,
    add_ref,
    release,
};
static DEVICES_TABLE: Devices = Devices {
    unknown: UNKNOWN,
    reserved: [0; 2],
    get,
};
static DEVICE_TABLE: Device = Device {
    unknown: UNKNOWN,
    activate,
    properties: 0,
    id,
    state,
};
static ENDPOINT_TABLE: Endpoint = Endpoint {
    unknown: UNKNOWN,
    flow,
};
static MANAGER_TABLE: Manager = Manager {
    unknown: UNKNOWN,
    reserved: [0; 2],
    sessions,
};
static SESSIONS_TABLE: Sessions = Sessions {
    unknown: UNKNOWN,
    count,
    at,
};
static BASE_TABLE: Unknown = UNKNOWN;
static SESSION_TABLE: Session = Session {
    unknown: UNKNOWN,
    state: session_state,
    reserved: [0; 9],
    instance,
    process,
};
pub(crate) fn verify_capture_native() -> IvyJson {
    let mut rows = vec![];
    for mode in [
        "",
        "positive:initialize",
        "initialize",
        "create",
        "null:create",
        "positive:create",
        "get",
        "null:get",
        "id",
        "null:id",
        "positive:id",
        "wrong-id",
        "state",
        "wrong-state",
        "query:endpoint",
        "null:query:endpoint",
        "flow",
        "wrong-flow",
        "activate",
        "null:activate",
        "sessions",
        "null:sessions",
        "count",
        "over-count",
        "negative-count",
        "at",
        "null:at",
        "query:session",
        "null:query:session",
        "instance",
        "null:instance",
        "process",
        "positive:process",
        "session-state",
        "invalid-session-state",
        "zero-pid",
        "multi-process",
    ] {
        let tables = [
            &DEVICES_TABLE as *const _ as *const (),
            &DEVICE_TABLE as *const _ as *const (),
            &ENDPOINT_TABLE as *const _ as *const (),
            &MANAGER_TABLE as *const _ as *const (),
            &SESSIONS_TABLE as *const _ as *const (),
            &BASE_TABLE as *const _ as *const (),
            &SESSION_TABLE as *const _ as *const (),
        ];
        let mut objects: Vec<_> = tables
            .into_iter()
            .enumerate()
            .map(|(index, table)| Box::new(Object { table, index }))
            .collect();
        let pointers = std::array::from_fn(|i| (&mut *objects[i] as *mut Object).cast());
        STATE.with(|s| {
            *s.borrow_mut() = State {
                mode,
                pointers,
                abi_ok: true,
                ..Default::default()
            }
        });
        let api = Api {
            initialize,
            uninitialize,
            create,
            free,
        };
        let result = CaptureObservation::read(&"capture-fixture".into(), |id| {
            WindowsCaptureEndpoint::with_api(id, api).map(Some)
        })
        .unwrap();
        assert_eq!(
            result.observed,
            matches!(
                mode,
                "" | "positive:initialize" | "zero-pid" | "multi-process"
            ),
            "{mode}"
        );
        if matches!(mode, "zero-pid" | "multi-process") {
            assert!(!result.sessions[0].single_process);
        }
        STATE.with(|s| {
            let s = s.borrow();
            assert!(s.abi_ok, "{mode}");
            assert_eq!(s.refs, [0; 7], "{mode}");
            assert_eq!(s.allocations, 0, "{mode}");
            assert_eq!(
                s.trace
                    .iter()
                    .filter(|x| x.as_str() == "uninitialize")
                    .count(),
                usize::from(mode != "initialize"),
                "{mode}"
            );
            if mode != "initialize" {
                assert_eq!(s.trace.last().unwrap(), "uninitialize");
            }
            rows.push(IvyJson::object([
                ("case", mode.into()),
                ("observation", result.to_json()),
                (
                    "trace",
                    IvyJson::Array(s.trace.iter().cloned().map(Into::into).collect()),
                ),
            ]));
        });
        drop(objects);
    }
    IvyJson::Array(rows)
}
