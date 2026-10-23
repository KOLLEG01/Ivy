use super::*;
use std::{cell::RefCell, collections::HashMap};
#[derive(Default)]
struct Fixture {
    fail: &'static str,
    trace: Vec<String>,
    live: usize,
    initialized: bool,
    uninitializations: usize,
    allocations: HashMap<usize, Box<[u16]>>,
    callback: Raw,
    state_calls: usize,
    count: i32,
    id: Vec<u16>,
    name: Vec<u16>,
    state: u32,
    final_state: u32,
    flow: i32,
    build: u32,
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
        query: query,
        add_ref: add,
        release: release,
    }
}
static ENUM: Enumerator = Enumerator {
    unknown: unknown(),
    enumerate: enumerate,
};
static COLLECTION: Collection = Collection {
    unknown: unknown(),
    count: count,
    at: at,
};
static DEVICE: Device = Device {
    unknown: unknown(),
    activate: activate,
    properties: properties,
    id: id,
    state: state,
};
static STORE: Store = Store {
    unknown: unknown(),
    reserved: [0; 2],
    get: get,
};
static ENDPOINT_TABLE: Endpoint = Endpoint {
    unknown: unknown(),
    flow: flow,
};
static VOLUME_TABLE: VolumeTable = VolumeTable {
    unknown: unknown(),
    register: register,
    unregister: unregister,
    channels: channels,
    unused: [0; 8],
    set_mute: 0,
    mute: mute,
    steps: steps,
    step_changes: [0; 2],
    hardware: hardware,
    range: range,
};
fn object(kind: u32) -> Raw {
    let table = match kind {
        1 => (&ENUM as *const Enumerator).cast(),
        2 => (&COLLECTION as *const Collection).cast(),
        3 => (&DEVICE as *const Device).cast(),
        4 => (&STORE as *const Store).cast(),
        5 => (&ENDPOINT_TABLE as *const Endpoint).cast(),
        6 => (&VOLUME_TABLE as *const VolumeTable).cast(),
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
    FIXTURE.with(|f| {
        let mut f = f.borrow_mut();
        f.live -= 1;
        f.trace.push(format!("release:{}", o.kind));
    });
    0
}
unsafe extern "system" fn query(_: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    assert!(equal(unsafe { &*iid }, &ENDPOINT));
    unsafe { *out = object(5) };
    event("query")
}
unsafe extern "system" fn initialize(_: Raw, mode: u32) -> Hr {
    assert_eq!(mode, 0);
    let hr = event("initialize");
    if hr >= 0 {
        FIXTURE.with(|f| f.borrow_mut().initialized = true)
    }
    hr
}
unsafe extern "system" fn uninitialize() {
    FIXTURE.with(|f| {
        let mut f = f.borrow_mut();
        assert!(f.initialized);
        assert_eq!(f.live, 0);
        assert!(f.allocations.is_empty());
        assert!(f.callback.is_null());
        f.uninitializations += 1;
        f.initialized = false;
    });
}
unsafe extern "system" fn create(
    class: *const GUID,
    outer: Raw,
    context: u32,
    iid: *const GUID,
    out: *mut Raw,
) -> Hr {
    assert!(outer.is_null());
    assert_eq!(context, 23);
    assert!(equal(unsafe { &*class }, &CLASS));
    assert!(equal(unsafe { &*iid }, &DEVICES));
    unsafe { *out = object(1) };
    event("create")
}
unsafe extern "system" fn enumerate(_: Raw, flow: i32, mask: u32, out: *mut Raw) -> Hr {
    assert_eq!((flow, mask), (2, 15));
    unsafe { *out = object(2) };
    event("enumerate")
}
unsafe extern "system" fn count(_: Raw, out: *mut i32) -> Hr {
    unsafe { *out = FIXTURE.with(|f| f.borrow().count) };
    event("count")
}
unsafe extern "system" fn at(_: Raw, index: u32, out: *mut Raw) -> Hr {
    FIXTURE.with(|f| {
        let mut f = f.borrow_mut();
        f.state_calls = 0;
        f.trace.push(format!("index:{index}"));
    });
    unsafe { *out = object(3) };
    event("item")
}
fn allocate(mut s: Vec<u16>) -> *mut u16 {
    s.push(0);
    let mut b = s.into_boxed_slice();
    let p = b.as_mut_ptr();
    FIXTURE.with(|f| f.borrow_mut().allocations.insert(p as usize, b));
    p
}
unsafe extern "system" fn free(raw: Raw) {
    FIXTURE.with(|f| assert!(f.borrow_mut().allocations.remove(&(raw as usize)).is_some()));
}
unsafe extern "system" fn id(_: Raw, out: *mut *mut u16) -> Hr {
    unsafe { *out = allocate(FIXTURE.with(|f| f.borrow().id.clone())) };
    event("id")
}
unsafe extern "system" fn properties(_: Raw, access: u32, out: *mut Raw) -> Hr {
    assert_eq!(access, 0);
    unsafe { *out = object(4) };
    event("properties")
}
unsafe extern "system" fn get(_: Raw, key: *const Key, out: *mut Variant) -> Hr {
    assert_eq!(unsafe { (*key).id }, 14);
    assert!(equal(
        unsafe { &(*key).format },
        &GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0)
    ));
    let fail = FIXTURE.with(|f| f.borrow().fail);
    if fail == "empty" {
        unsafe { (*out).vt = 0 };
        return 0;
    }
    if fail == "type" {
        unsafe {
            (*out).vt = 3;
            (*out).data[0] = 42
        };
        return 0;
    }
    unsafe {
        (*out).vt = 31;
        (*out).data[0] = allocate(FIXTURE.with(|f| f.borrow().name.clone())) as usize
    };
    event("property")
}
unsafe extern "system" fn clear(out: *mut Variant) -> Hr {
    if unsafe { (*out).vt } == 31 && unsafe { (*out).data[0] } != 0 {
        unsafe { free((*out).data[0] as Raw) }
    }
    unsafe { (*out).vt = 0 };
    event("clear")
}
unsafe extern "system" fn state(_: Raw, out: *mut u32) -> Hr {
    let (n, v) = FIXTURE.with(|f| {
        let mut f = f.borrow_mut();
        f.state_calls += 1;
        let n = f.state_calls;
        (n, if n == 1 { f.state } else { f.final_state })
    });
    unsafe { *out = v };
    event(if n == 1 { "state:first" } else { "state:final" })
}
unsafe extern "system" fn flow(_: Raw, out: *mut i32) -> Hr {
    unsafe { *out = FIXTURE.with(|f| f.borrow().flow) };
    event("flow")
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
    unsafe { *out = object(6) };
    event("activate")
}
unsafe extern "system" fn channels(_: Raw, out: *mut i32) -> Hr {
    unsafe { *out = 2 };
    event("channels")
}
unsafe extern "system" fn steps(_: Raw, a: *mut u32, b: *mut u32) -> Hr {
    unsafe {
        *a = 2;
        *b = 16
    };
    event("steps")
}
unsafe extern "system" fn hardware(_: Raw, out: *mut u32) -> Hr {
    unsafe { *out = 0 };
    event("hardware")
}
unsafe extern "system" fn range(_: Raw, a: *mut f32, b: *mut f32, c: *mut f32) -> Hr {
    unsafe {
        *a = -60.0;
        *b = 0.0;
        *c = 1.0
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
    let mut queried = ptr::null_mut();
    assert_eq!(
        unsafe { (t.unknown.query)(callback, &NOTIFY, &mut queried) },
        0
    );
    assert_eq!(queried, callback);
    assert_eq!(unsafe { (t.unknown.release)(queried) }, 2);
    assert_eq!(unsafe { (t.notify)(callback, ptr::null_mut()) }, 0);
    FIXTURE.with(|f| f.borrow_mut().callback = callback);
    0
}
unsafe extern "system" fn unregister(_: Raw, callback: Raw) -> Hr {
    assert_eq!(
        FIXTURE.with(|f| std::mem::replace(&mut f.borrow_mut().callback, ptr::null_mut())),
        callback
    );
    let t = unsafe { &**callback.cast::<*const Unknown>() };
    assert_eq!(unsafe { (t.release)(callback) }, 1);
    event("unregister")
}
unsafe extern "system" fn mute(_: Raw, out: *mut i32) -> Hr {
    unsafe { *out = 1 };
    event("mute")
}
unsafe extern "system" fn version(v: *mut Version) -> Hr {
    assert_eq!(unsafe { (*v).size }, 276);
    unsafe {
        (*v).major = 10;
        (*v).minor = 0;
        (*v).build = FIXTURE.with(|f| f.borrow().build)
    };
    event("version")
}
fn api() -> Api {
    Api {
        initialize,
        uninitialize,
        create,
        free,
        clear,
        version,
    }
}
fn fixture(fail: &'static str) {
    FIXTURE.with(|f| {
        *f.borrow_mut() = Fixture {
            fail,
            count: 1,
            id: "device".encode_utf16().collect(),
            name: "Endpoint".encode_utf16().collect(),
            state: 1,
            final_state: 1,
            flow: 1,
            build: 20348,
            ..Default::default()
        }
    });
}
fn trace_file(name: &str, rows: &[serde_json::Value]) {
    if let Some(root) = std::env::var_os("IVY_PHONE_INVENTORY_ABI_TRACE") {
        std::fs::write(
            std::path::Path::new(&root).join(name),
            serde_json::to_vec_pretty(rows).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn inventory_abi_faults_release_outputs_and_preserve_unknown_mute() {
    let mut rows = Vec::new();
    for fail in [
        "",
        "initialize",
        "create",
        "enumerate",
        "count",
        "item",
        "id",
        "properties",
        "property",
        "type",
        "empty",
        "state:first",
        "activate",
        "channels",
        "steps",
        "hardware",
        "range",
        "register",
        "mute",
        "query",
        "flow",
        "state:final",
        "version",
        "unregister",
    ] {
        fixture(fail);
        let output = read_with(api());
        let fallback = matches!(
            fail,
            "property"
                | "empty"
                | "activate"
                | "channels"
                | "steps"
                | "hardware"
                | "range"
                | "register"
                | "mute"
                | "unregister"
        );
        assert_eq!(output.is_ok(), fail.is_empty() || fallback, "{fail}");
        if let Ok(v) = &output {
            let d = &v.get("devices").unwrap().as_array().unwrap()[0];
            assert_eq!(
                d.get("name"),
                Some(&IvyJson::from(if matches!(fail, "property" | "empty") {
                    "Unknown"
                } else {
                    "Endpoint"
                }))
            );
            let muted = d.get("muted").unwrap();
            assert_eq!(
                *muted,
                if matches!(
                    fail,
                    "activate" | "channels" | "steps" | "hardware" | "range" | "register" | "mute"
                ) {
                    IvyJson::Null
                } else {
                    true.into()
                }
            );
        }
        FIXTURE.with(|f|{let f=f.borrow();assert_eq!(f.live,0,"{fail}");assert!(f.allocations.is_empty(),"{fail}");assert_eq!(f.uninitializations,usize::from(fail!="initialize"));rows.push(serde_json::json!({"fault":fail,"succeeded":output.is_ok(),"trace":f.trace,"live":f.live,"allocations":f.allocations.len(),"uninitializations":f.uninitializations}));});
    }
    trace_file("abi-faults.json", &rows);
}
#[test]
fn inventory_abi_bounds_flags_repeated_state_and_loopback_version() {
    let mut rows = Vec::new();
    for (name, n, id_len, name_len, state, final_state, flow, build, pass) in [
        ("empty", 0, 0, 0, 0, 0, 0, 20347, true),
        ("negative-count", -1, 1, 1, 1, 1, 0, 20348, true),
        ("32", 32, 1, 1, 1, 1, 1, 20348, true),
        ("33", 33, 1, 1, 1, 1, 1, 20348, false),
        ("id-boundary", 1, 1024, 256, 1, 1, 1, 20348, true),
        ("id-too-long", 1, 1025, 1, 1, 1, 1, 20348, false),
        ("name-too-long", 1, 1, 257, 1, 1, 1, 20348, false),
        ("state-changes", 1, 1, 1, 1, 8, 2, 20348, true),
        ("disabled", 1, 1, 1, 2, 2, 0, 20348, true),
        ("flags", 1, 1, 1, 3, 3, 0, 20348, true),
        ("all", 1, 1, 1, 15, 15, 0, 20348, true),
        (
            "unknown-flag",
            1,
            1,
            1,
            0x80000000,
            0x80000000,
            0,
            20348,
            true,
        ),
    ] {
        fixture("");
        FIXTURE.with(|f| {
            let mut f = f.borrow_mut();
            f.count = n;
            f.id = vec![65; id_len];
            f.name = vec![66; name_len];
            f.state = state;
            f.final_state = final_state;
            f.flow = flow;
            f.build = build;
        });
        let output = read_with(api());
        assert_eq!(output.is_ok(), pass, "{name}");
        if let Ok(v) = &output {
            assert_eq!(
                v.get("processLoopbackSupported"),
                Some(&IvyJson::from(build >= 20348))
            );
            if n > 0 {
                let d = &v.get("devices").unwrap().as_array().unwrap()[0];
                assert_eq!(
                    d.get("state"),
                    Some(&IvyJson::from(state_name(final_state)))
                );
                assert_eq!(
                    d.get("direction"),
                    Some(&IvyJson::from(if flow == 1 { "capture" } else { "render" }))
                );
            }
        }
        FIXTURE.with(|f|{let f=f.borrow();assert_eq!(f.live,0);assert!(f.allocations.is_empty());if n==33{assert_eq!(f.trace.iter().filter(|e|e.starts_with("index:")).count(),33);}rows.push(serde_json::json!({"case":name,"success":pass,"trace":f.trace,"live":f.live,"allocations":f.allocations.len()}));});
    }
    assert_eq!(state_name(7), "Active, Disabled, NotPresent");
    assert_eq!(state_name(0x80000000), "-2147483648");
    trace_file("abi-bounds.json", &rows);
}
