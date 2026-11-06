use ivy_contracts::{
    IvyJson, JsString,
    json::{
        canonical, canonical_size, parse, stringify, stringify_into, stringify_size, validate_json,
    },
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicIsize, Ordering},
};

struct Counted;
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
fn account(bytes: isize) {
    let current = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(current, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            account(layout.size() as isize);
        }
        result
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            account(size as isize - layout.size() as isize);
        }
        result
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

#[test]
fn reserved_stringify_preserves_native_number_string_and_key_semantics() {
    let value = IvyJson::Object(vec![
        (
            "10".into(),
            JsString(vec![0xd800, 0x0a, 0xd83d, 0xde00]).into(),
        ),
        ("2".into(), IvyJson::Number(f64::NAN)),
        ("n".into(), IvyJson::Number(9_007_199_254_740_992.0)),
        ("zero".into(), IvyJson::Number(-0.0)),
        ("infinity".into(), IvyJson::Number(f64::INFINITY)),
    ]);
    let expected =
        "{\"2\":null,\"10\":\"\\ud800\\n😀\",\"n\":9007199254740992,\"zero\":0,\"infinity\":null}";
    assert_eq!(stringify(&value, usize::MAX).unwrap(), expected);
    assert_eq!(
        stringify_size(&value, expected.len()).unwrap(),
        expected.len()
    );
    let mut output = String::with_capacity(expected.len() + 1);
    let capacity = output.capacity();
    stringify_into(&value, expected.len(), &mut output).unwrap();
    assert_eq!(output, expected);
    assert_eq!(output.capacity(), capacity);
    for maximum in [0, 1, expected.len() - 1] {
        let original = stringify(&value, maximum).unwrap_err().to_wire();
        assert_eq!(
            stringify_size(&value, maximum).unwrap_err().to_wire(),
            original
        );
        assert_eq!(
            stringify_into(&value, maximum, String::new())
                .unwrap_err()
                .to_wire(),
            original
        );
    }
}

#[test]
fn counted_canonical_bytes_preserve_exact_boundaries_and_error_order() {
    let mut values: Vec<_> = [
        "null",
        "true",
        "[]",
        "{}",
        "[[],{},0,-0,1e-7,1e-6]",
        r#"{"10":"😀","2":"é","01":"日本語","x":"\"\\\n\u0000\ud800"}"#,
    ]
    .into_iter()
    .map(|text| parse(text, 1000).unwrap())
    .collect();
    let mut units: Vec<_> = (0..32).collect();
    units.extend([0xd800, 65, 0xdfff, 0xd83d, 0xde00]);
    values.push(IvyJson::object([(
        JsString(units.clone()),
        IvyJson::String(JsString(units)),
    )]));
    values.push(IvyJson::Object(vec![
        ("x".into(), true.into()),
        ("x".into(), false.into()),
    ]));
    for number in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        9_007_199_254_740_992.0,
    ] {
        values.push(IvyJson::Array(vec![
            "x".repeat(1024).into(),
            IvyJson::Number(number),
        ]));
    }
    for depth in [128, 129] {
        let mut value = IvyJson::Null;
        for _ in 0..depth {
            value = IvyJson::Array(vec![value]);
        }
        values.push(value);
    }
    let validators = ivy_contracts::schema::SchemaValidators::new();
    let schema = IvyJson::object([] as [(&str, IvyJson); 0]);
    for value in values {
        let full = canonical(&value, usize::MAX);
        let mut limits = vec![0, 1, 2, 5, 11, 32, 1024, usize::MAX];
        if let Ok(text) = &full {
            limits.extend([text.len() - 1, text.len(), text.len() + 1]);
        }
        for maximum in limits {
            let expected = canonical(&value, maximum)
                .map(|text| text.len())
                .map_err(|e| e.to_wire());
            assert_eq!(
                canonical_size(&value, maximum).map_err(|e| e.to_wire()),
                expected
            );
            assert_eq!(
                validators
                    .validate(&schema, &value, maximum)
                    .map_err(|e| e.to_wire()),
                expected.as_ref().map(|_| ()).map_err(Clone::clone)
            );
            let expected_json = match &full {
                Err(error) => Err(error.to_wire()),
                Ok(_) if stringify(&value, usize::MAX).unwrap().len() > maximum => {
                    Err(ivy_contracts::IvyError::new(
                        "content_too_large",
                        "JSON exceeds the configured byte limit.",
                    )
                    .to_wire())
                }
                Ok(_) => Ok(()),
            };
            assert_eq!(
                validate_json(&value, maximum).map_err(|e| e.to_wire()),
                expected_json
            );
        }
    }
}

fn peak(action: impl FnOnce()) -> isize {
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    action();
    PEAK.load(Ordering::Relaxed) - before
}

#[test]
fn large_validation_does_not_materialize_the_encoded_body() {
    const CASE: &str = "json_validation::large_validation_does_not_materialize_the_encoded_body";
    const CHILD: &str = "IVY_JSON_VALIDATION_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let units = [34, 10, 0xd800, 65, 0xdfff, 0xd83d, 0xde00]
        .into_iter()
        .cycle()
        .take(2 * 1024 * 1024)
        .collect();
    let value = IvyJson::object([("body", IvyJson::String(JsString(units)))]);
    let bytes = canonical(&value, usize::MAX).unwrap().len();
    assert!(bytes > 6 * 1024 * 1024);
    let validators = ivy_contracts::schema::SchemaValidators::new();
    let schema = parse(r#"{"type":"object"}"#, 100).unwrap();
    validators.compile(&schema).unwrap();
    let materialized = peak(|| {
        drop(canonical(&value, bytes).unwrap());
    });
    let json = peak(|| {
        validate_json(&value, bytes).unwrap();
    });
    let canonical = peak(|| {
        assert_eq!(canonical_size(&value, bytes).unwrap(), bytes);
    });
    let schema = peak(|| {
        validators.validate(&schema, &value, bytes).unwrap();
    });
    let refused = peak(|| {
        assert_eq!(
            canonical_size(&value, bytes - 1).unwrap_err().code,
            "content_too_large"
        );
        assert_eq!(
            validate_json(&value, bytes - 1).unwrap_err().code,
            "content_too_large"
        );
    });
    eprintln!(
        "JSON validation allocation peaks beside owned payload: wire={bytes}, materialized={materialized}, JSON={json}, canonical={canonical}, schema={schema}, refused={refused}"
    );
    assert!(materialized >= bytes as isize);
    for amount in [json, canonical, schema, refused] {
        assert!(
            amount < 64 * 1024,
            "Validation retained a payload-sized temporary: {amount}"
        );
    }
}
