use super::*;
use serde_json::{Value, json};
use std::collections::HashMap;

struct Fixture {
    fail: &'static str,
    trace: Vec<String>,
    live: usize,
    balances: usize,
    init: Hr,
    id: Vec<u16>,
    flow: i32,
    state: u32,
    muted: bool,
    allocations: HashMap<usize, Box<[u16]>>,
    callback: Raw,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            fail: "",
            trace: vec![],
            live: 0,
            balances: 0,
            init: 0,
            id: "s".encode_utf16().collect(),
            flow: 0,
            state: 1,
            muted: false,
            allocations: HashMap::new(),
            callback: ptr::null_mut(),
        }
    }
}
thread_local! {static FIXTURE:RefCell<Fixture>=RefCell::new(Fixture::default());}
fn event(name: &str) -> Hr {
    FIXTURE.with(|f| {
        let mut f = f.borrow_mut();
        f.trace.push(name.into());
        if f.fail == name { -1 } else { 0 }
    })
}
#[repr(C)]
struct Object {
    table: *const c_void,
    kind: u32,
}
const fn unknown() -> Unknown {
    Unknown {
        query,
        add_ref: add,
        release,
    }
}
static ENUM: Enumerator = Enumerator {
    unknown: unknown(),
    reserved: [0; 2],
    get,
};
static DEVICE: Device = Device {
    unknown: unknown(),
    activate,
    properties: 0,
    id,
    state,
};
static ENDPOINT_TABLE: Endpoint = Endpoint {
    unknown: unknown(),
    flow,
};
static VOLUME_TABLE: VolumeTable = VolumeTable {
    unknown: unknown(),
    register,
    unregister,
    channels,
    unused: [0; 8],
    set_mute,
    mute,
    steps,
    step_changes: [0; 2],
    hardware,
    range,
};
fn object(kind: u32) -> Raw {
    let table = match kind {
        1 => (&ENUM as *const Enumerator).cast(),
        2 => (&DEVICE as *const Device).cast(),
        3 => (&ENDPOINT_TABLE as *const Endpoint).cast(),
        4 => (&VOLUME_TABLE as *const VolumeTable).cast(),
        _ => panic!(),
    };
    FIXTURE.with(|f| f.borrow_mut().live += 1);
    Box::into_raw(Box::new(Object { table, kind })).cast()
}
unsafe extern "system" fn add(_: Raw) -> u32 {
    1
}
unsafe extern "system" fn release(raw: Raw) -> u32 {
    let o = unsafe { Box::from_raw(raw.cast::<Object>()) };
    let callback = FIXTURE.with(|f| {
        let mut f = f.borrow_mut();
        f.live -= 1;
        f.trace.push(format!("release:{}", o.kind));
        if o.kind == 4 {
            std::mem::replace(&mut f.callback, ptr::null_mut())
        } else {
            ptr::null_mut()
        }
    });
    if !callback.is_null() {
        let t = unsafe { &**callback.cast::<*const Unknown>() };
        assert_eq!(unsafe { (t.release)(callback) }, 1);
    }
    0
}
unsafe extern "system" fn query(_: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    assert!(equal(unsafe { &*iid }, &ENDPOINT));
    unsafe {
        *out = if FIXTURE.with(|f| f.borrow().fail) == "query-null" {
            ptr::null_mut()
        } else {
            object(3)
        }
    };
    event("query")
}
unsafe extern "system" fn initialize(raw: Raw, mode: u32) -> Hr {
    assert!(raw.is_null());
    assert_eq!(mode, 0);
    let hr = event("initialize");
    if hr < 0 {
        return hr;
    }
    FIXTURE.with(|f| {
        let mut f = f.borrow_mut();
        if f.init >= 0 {
            f.balances += 1;
        }
        f.init
    })
}
unsafe extern "system" fn uninitialize() {
    FIXTURE.with(|f| {
        let mut f = f.borrow_mut();
        assert_eq!(f.live, 0);
        assert!(f.allocations.is_empty());
        assert!(f.callback.is_null());
        assert_eq!(f.balances, 1);
        f.balances = 0;
        f.trace.push("uninitialize".into());
    });
}
unsafe extern "system" fn create(
    class: *const GUID,
    outer: Raw,
    context: u32,
    iid: *const GUID,
    out: *mut Raw,
) -> Hr {
    assert!(equal(unsafe { &*class }, &CLASS));
    assert!(outer.is_null());
    assert_eq!(context, 23);
    assert!(equal(unsafe { &*iid }, &DEVICES));
    unsafe {
        *out = if FIXTURE.with(|f| f.borrow().fail) == "create-null" {
            ptr::null_mut()
        } else {
            object(1)
        }
    };
    event("create")
}
unsafe fn string(raw: *const u16) -> String {
    let mut n = 0;
    while unsafe { *raw.add(n) } != 0 {
        n += 1;
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(raw, n) }).unwrap()
}
unsafe extern "system" fn get(_: Raw, requested: *const u16, out: *mut Raw) -> Hr {
    let requested = unsafe { string(requested) };
    assert_eq!(requested, "s");
    let hr = event(&format!("get:{requested}"));
    unsafe {
        *out = if FIXTURE.with(|f| f.borrow().fail) == "get-null" {
            ptr::null_mut()
        } else {
            object(2)
        }
    };
    hr
}
fn allocate(mut value: Vec<u16>) -> *mut u16 {
    value.push(0);
    let mut b = value.into_boxed_slice();
    let p = b.as_mut_ptr();
    FIXTURE.with(|f| f.borrow_mut().allocations.insert(p as usize, b));
    p
}
unsafe extern "system" fn free(raw: Raw) {
    FIXTURE.with(|f| {
        let mut f = f.borrow_mut();
        assert!(f.allocations.remove(&(raw as usize)).is_some());
        f.trace.push("free".into());
    });
}
unsafe extern "system" fn id(_: Raw, out: *mut *mut u16) -> Hr {
    unsafe {
        *out = if FIXTURE.with(|f| f.borrow().fail) == "id-null" {
            ptr::null_mut()
        } else {
            allocate(FIXTURE.with(|f| f.borrow().id.clone()))
        }
    };
    event("id")
}
unsafe extern "system" fn flow(_: Raw, out: *mut i32) -> Hr {
    unsafe { *out = FIXTURE.with(|f| f.borrow().flow) };
    event("flow")
}
unsafe extern "system" fn state(_: Raw, out: *mut u32) -> Hr {
    unsafe { *out = FIXTURE.with(|f| f.borrow().state) };
    event("state")
}
unsafe extern "system" fn activate(
    _: Raw,
    iid: *const GUID,
    context: u32,
    data: Raw,
    out: *mut Raw,
) -> Hr {
    assert!(equal(unsafe { &*iid }, &VOLUME));
    assert_eq!(context, 23);
    assert!(data.is_null());
    unsafe {
        *out = if FIXTURE.with(|f| f.borrow().fail) == "activate-null" {
            ptr::null_mut()
        } else {
            object(4)
        }
    };
    event("activate")
}
unsafe extern "system" fn channels(_: Raw, out: *mut i32) -> Hr {
    unsafe {
        *out = if FIXTURE.with(|f| f.borrow().fail) == "channels-negative" {
            -1
        } else {
            2
        }
    };
    event("channels")
}
unsafe extern "system" fn steps(_: Raw, a: *mut u32, b: *mut u32) -> Hr {
    unsafe {
        *a = 0;
        *b = 10;
    };
    event("steps")
}
unsafe extern "system" fn hardware(_: Raw, out: *mut u32) -> Hr {
    unsafe {
        *out = 0;
    };
    event("hardware")
}
unsafe extern "system" fn range(_: Raw, a: *mut f32, b: *mut f32, c: *mut f32) -> Hr {
    unsafe {
        *a = -60.0;
        *b = 0.0;
        *c = 1.0;
    };
    event("range")
}
unsafe extern "system" fn register(_: Raw, callback: Raw) -> Hr {
    let hr = event("register");
    if hr < 0 {
        return hr;
    }
    let t = unsafe { &**callback.cast::<*const NotifyTable>() };
    assert_eq!(unsafe { (t.unknown.add_ref)(callback) }, 2);
    for iid in [&NOTIFY, &UNKNOWN] {
        let mut out = ptr::null_mut();
        assert_eq!(unsafe { (t.unknown.query)(callback, iid, &mut out) }, 0);
        assert_eq!(out, callback);
        assert_eq!(unsafe { (t.unknown.release)(out) }, 2);
    }
    let mut out = callback;
    assert_eq!(
        unsafe { (t.unknown.query)(callback, &VOLUME, &mut out) },
        0x80004002u32 as i32
    );
    assert!(out.is_null());
    assert_eq!(
        unsafe { (t.unknown.query)(callback, &NOTIFY, ptr::null_mut()) },
        0x80004003u32 as i32
    );
    assert_eq!(unsafe { (t.notify)(callback, ptr::null_mut()) }, 0);
    FIXTURE.with(|f| f.borrow_mut().callback = callback);
    0
}
unsafe extern "system" fn unregister(_: Raw, callback: Raw) -> Hr {
    let hr = event("unregister");
    if hr < 0 {
        return hr;
    }
    assert_eq!(
        FIXTURE.with(|f| std::mem::replace(&mut f.borrow_mut().callback, ptr::null_mut())),
        callback
    );
    let t = unsafe { &**callback.cast::<*const Unknown>() };
    assert_eq!(unsafe { (t.release)(callback) }, 1);
    0
}
unsafe extern "system" fn mute(_: Raw, out: *mut i32) -> Hr {
    unsafe { *out = i32::from(FIXTURE.with(|f| f.borrow().muted)) };
    event("mute")
}
unsafe extern "system" fn set_mute(_: Raw, value: i32, context: *const GUID) -> Hr {
    assert!(!context.is_null());
    assert!(equal(unsafe { &*context }, &GUID::from_u128(0)));
    assert!(value == 0 || value == 1);
    let hr = event(&format!("set:{value}"));
    if hr >= 0 {
        FIXTURE.with(|f| f.borrow_mut().muted = value != 0);
    }
    if FIXTURE.with(|f| f.borrow().fail) == "set-false" {
        1
    } else {
        hr
    }
}
fn api() -> Api {
    Api {
        initialize,
        uninitialize,
        create,
        free,
    }
}
fn fixture(fail: &'static str) {
    FIXTURE.with(|f| {
        *f.borrow_mut() = Fixture {
            fail,
            ..Fixture::default()
        }
    });
}
fn snapshot(name: &str, ok: bool) -> Value {
    FIXTURE.with(|f|{let f=f.borrow();assert_eq!(f.live,0);assert_eq!(f.balances,0);assert!(f.allocations.is_empty());assert!(f.callback.is_null());json!({"name":name,"ok":ok,"muted":f.muted,"trace":f.trace,"live":f.live,"allocations":f.allocations.len(),"balances":f.balances,"callbackHeld":!f.callback.is_null()})})
}
fn trace(name: &str, rows: &[Value]) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_AUDIO_MUTE_WINDOWS_TRACE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            serde_json::to_vec_pretty(rows).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn exact_mute_adapter_is_lazy_thread_bound_and_balances_all_outputs() {
    let mut rows = vec![];
    for fail in [
        "",
        "initialize",
        "create",
        "create-null",
        "get:s",
        "get-null",
        "id",
        "id-null",
        "query",
        "query-null",
        "flow",
        "state",
        "activate",
        "activate-null",
        "channels",
        "channels-negative",
        "steps",
        "hardware",
        "range",
        "register",
        "mute",
        "set:1",
        "unregister",
        "set-false",
    ] {
        fixture(fail);
        let result = (|| -> Result<()> {
            let mut e = WindowsEndpoint::open_with(api(), "s")?;
            assert_eq!(e.id()?, "s");
            assert!(e.active_render()?);
            assert!(!e.muted()?);
            e.set_muted(true)?;
            assert!(e.muted()?);
            e.close()?;
            e.close()?;
            assert!(e.id().is_err());
            Ok(())
        })();
        assert_eq!(
            result.is_ok(),
            matches!(fail, "" | "unregister" | "set-false"),
            "{fail}"
        );
        rows.push(snapshot(fail, result.is_ok()));
    }
    assert_eq!(
        rows[0]["trace"],
        json!([
            "initialize",
            "create",
            "get:s",
            "release:1",
            "id",
            "free",
            "query",
            "flow",
            "release:3",
            "state",
            "activate",
            "channels",
            "steps",
            "hardware",
            "range",
            "register",
            "mute",
            "set:1",
            "mute",
            "unregister",
            "release:4",
            "release:2",
            "uninitialize"
        ])
    );
    trace("mute-windows-faults.json", &rows);
}

#[test]
fn mute_adapter_preserves_flow_state_identity_and_existing_apartments() {
    let mut rows = vec![];
    for (name, flow, state, init, expected) in [
        ("capture", 1, 1, 0, false),
        ("unknown-flow", 9, 1, 0, false),
        ("inactive", 0, 2, 0, false),
        ("combined-state", 0, 3, 0, false),
        ("active", 0, 1, 0, true),
        ("already-mta", 0, 1, 1, true),
        ("existing-sta", 0, 1, 0x80010106u32 as i32, true),
    ] {
        fixture("");
        FIXTURE.with(|f| {
            let mut f = f.borrow_mut();
            f.flow = flow;
            f.state = state;
            f.init = init;
        });
        let mut e = WindowsEndpoint::open_with(api(), "s").unwrap();
        assert_eq!(e.active_render().unwrap(), expected);
        e.close().unwrap();
        let row = snapshot(name, true);
        assert!(
            !row["trace"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e == "activate")
        );
        if flow != 0 {
            assert!(
                !row["trace"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e == "state")
            );
        }
        if name == "existing-sta" {
            assert!(
                !row["trace"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e == "uninitialize")
            );
        }
        rows.push(row);
    }
    for (name, id, valid) in [
        ("empty-id", vec![], true),
        ("maximum-id", vec![b'x' as u16; 32767], true),
        ("oversized-id", vec![b'x' as u16; 32768], false),
        ("invalid-utf16", vec![0xd800], false),
    ] {
        fixture("");
        FIXTURE.with(|f| f.borrow_mut().id = id);
        let mut e = WindowsEndpoint::open_with(api(), "s").unwrap();
        assert_eq!(e.id().is_ok(), valid);
        e.close().unwrap();
        rows.push(snapshot(name, valid));
    }
    for invalid in ["", " ", "s\0"] {
        fixture("");
        assert!(WindowsEndpoint::open_with(api(), invalid).is_err());
        let row = snapshot("invalid-request", false);
        assert!(row["trace"].as_array().unwrap().is_empty());
        rows.push(row);
    }
    trace("mute-windows-state.json", &rows);
}

#[tokio::test(flavor = "current_thread")]
async fn native_policy_rejects_replacement_before_volume_and_cancels_the_system_timer() {
    use crate::media::{AudioMuteGuard, AudioMutePolicy};
    let mut rows = vec![];
    for (name, changed_id, active, already, success) in [
        ("normal", false, true, false, true),
        ("already", false, true, true, true),
        ("replacement", true, true, false, false),
        ("inactive", false, false, false, false),
    ] {
        fixture("");
        FIXTURE.with(|f| {
            let mut f = f.borrow_mut();
            if changed_id {
                f.id = "replacement".encode_utf16().collect();
            }
            f.state = if active { 1 } else { 2 };
            f.muted = already;
        });
        let policy = AudioMutePolicy {
            muted_render_endpoint: Some("s".into()),
            unmute_receive: false,
            interval_seconds: 60,
        };
        let guard = AudioMuteGuard::start(
            &policy,
            "c",
            || panic!("No periodic failure expected"),
            |id| Ok(Box::new(WindowsEndpoint::open_with(api(), id)?)),
        );
        assert_eq!(guard.is_ok(), success, "{name}");
        if let Ok(guard) = guard {
            tokio::time::timeout(std::time::Duration::from_secs(5), guard.close().wait())
                .await
                .unwrap()
                .unwrap();
        }
        let row = snapshot(name, success);
        let events = row["trace"].as_array().unwrap();
        if !success {
            assert!(!events.iter().any(|e| e == "activate"));
        } else {
            assert_eq!(row["muted"], true);
        }
        if already {
            assert!(!events.iter().any(|e| e == "set:1"));
        }
        rows.push(row);
    }
    trace("mute-windows-guard.json", &rows);
}
