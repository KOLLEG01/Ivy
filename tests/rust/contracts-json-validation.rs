use ivy_contracts::{
    IvyJson, JsString,
    json::{
        canonical, canonical_equal, canonical_matches, canonical_object_size, canonical_size,
        parse, stringify, stringify_into, stringify_size, validate_json,
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
            let complete = full.as_deref().unwrap_or("invalid");
            for text in [
                "",
                "different",
                "null ",
                " true",
                complete,
                complete.trim_end_matches('}'),
            ] {
                assert_eq!(
                    canonical_matches(&value, text, maximum).map_err(|e| e.to_wire()),
                    canonical(&value, maximum)
                        .map(|encoded| encoded == text)
                        .map_err(|e| e.to_wire())
                );
            }
            if let Some(pairs) = value.as_object() {
                assert_eq!(
                    canonical_object_size(pairs.iter().map(|(key, value)| (key, value)), maximum)
                        .map_err(|e| e.to_wire()),
                    expected
                );
            }
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

#[test]
fn borrowed_object_size_keeps_shared_depth_node_and_byte_budgets() {
    let compare = |value: &IvyJson| {
        let pairs = value.as_object().unwrap();
        let full = canonical(value, usize::MAX);
        let mut limits = vec![0, 1, 2, 11, 32, 1024, usize::MAX];
        if let Ok(text) = &full {
            limits.extend([text.len() - 1, text.len(), text.len() + 1]);
        }
        for maximum in limits {
            assert_eq!(
                canonical_object_size(pairs.iter().map(|(key, value)| (key, value)), maximum)
                    .map_err(|error| error.to_wire()),
                canonical(value, maximum)
                    .map(|text| text.len())
                    .map_err(|error| error.to_wire())
            );
        }
    };
    for depth in [127, 128] {
        let mut value = IvyJson::Null;
        for _ in 0..depth {
            value = IvyJson::Array(vec![value]);
        }
        compare(&IvyJson::object([("nested", value)]));
    }
    for second in [199_998, 199_999] {
        compare(&IvyJson::object([
            ("a", IvyJson::Array(vec![IvyJson::Null; 199_999])),
            ("b", IvyJson::Array(vec![IvyJson::Null; second])),
        ]));
    }
    compare(&IvyJson::Object(vec![
        ("x".into(), IvyJson::Number(f64::NAN)),
        ("x".into(), IvyJson::Null),
        (
            JsString(vec![0xd800, 0]),
            IvyJson::String(JsString(vec![0xdfff, 10])),
        ),
    ]));
}

#[test]
fn canonical_equality_matches_original_text_for_raw_keys_and_values() {
    let mut values: Vec<_> = [
        "null",
        "true",
        "false",
        "0",
        "-0",
        "1",
        "1e-7",
        "1e-6",
        "[]",
        "{}",
        r#"[1,{"b":false,"a":null}]"#,
        r#"[1,{"a":null,"b":false}]"#,
        r#"{"10":0,"2":1,"01":"é","text":"日本語😀"}"#,
        r#"{"text":"日本語😀","2":1,"01":"é","10":-0}"#,
    ]
    .into_iter()
    .map(|text| parse(text, 1024).unwrap())
    .collect();
    for entries in [
        vec![("x", true), ("y", false), ("x", false)],
        vec![("y", false), ("x", true), ("x", false)],
        vec![("x", false), ("y", false), ("x", true)],
        vec![("x", false), ("y", false)],
    ] {
        values.push(IvyJson::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        ));
    }
    let units = JsString(vec![0xd800, 0, 34, 92, 0xdfff, 0xd83d, 0xde00]);
    values.push(IvyJson::object([(
        units.clone(),
        IvyJson::String(units.clone()),
    )]));
    values.push(IvyJson::Array(vec![IvyJson::String(units)]));
    for number in [f64::NAN, f64::INFINITY, 9_007_199_254_740_992.0] {
        values.push(IvyJson::Number(number));
    }
    for left in &values {
        for right in &values {
            let mut limits = vec![0, 1, 2, 11, usize::MAX];
            for value in [left, right] {
                if let Ok(text) = canonical(value, usize::MAX) {
                    limits.extend([text.len().saturating_sub(1), text.len(), text.len() + 1]);
                }
            }
            for maximum in limits {
                let expected = canonical(left, maximum)
                    .and_then(|a| canonical(right, maximum).map(|b| a == b))
                    .map_err(|e| e.to_wire());
                assert_eq!(
                    canonical_equal(left, right, maximum).map_err(|e| e.to_wire()),
                    expected
                );
            }
        }
    }
}

#[test]
fn canonical_equality_validates_both_full_values_before_comparing() {
    let different = IvyJson::Bool(false);
    let compare = |left: &IvyJson, right: &IvyJson| {
        let expected = canonical(left, usize::MAX)
            .and_then(|a| canonical(right, usize::MAX).map(|b| a == b))
            .map_err(|e| e.to_wire());
        assert_eq!(
            canonical_equal(left, right, usize::MAX).map_err(|e| e.to_wire()),
            expected
        );
        for value in [left, right] {
            assert_eq!(
                canonical_matches(value, "different", usize::MAX).map_err(|e| e.to_wire()),
                canonical(value, usize::MAX)
                    .map(|_| false)
                    .map_err(|e| e.to_wire())
            );
        }
    };
    for depth in [128, 129] {
        let mut nested = IvyJson::Null;
        for _ in 0..depth {
            nested = IvyJson::Array(vec![nested]);
        }
        compare(&nested, &nested);
        compare(&different, &nested);
        compare(&nested, &different);
    }
    for count in [399_999, 400_000] {
        let wide = IvyJson::Array(vec![IvyJson::Null; count]);
        compare(&wide, &wide);
        compare(&different, &wide);
        compare(&wide, &different);
    }
    let invalid_number = IvyJson::Number(f64::NAN);
    let invalid_late = IvyJson::object([
        ("a", "x".repeat(1024).into()),
        ("z", IvyJson::Number(9_007_199_254_740_992.0)),
    ]);
    compare(&invalid_late, &invalid_number);
    compare(&invalid_number, &invalid_late);
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
    let other = value.clone();
    let original_equality = peak(|| {
        assert_eq!(
            canonical(&value, bytes).unwrap(),
            canonical(&other, bytes).unwrap()
        );
    });
    let borrowed_equality = peak(|| {
        assert!(canonical_equal(&value, &other, bytes).unwrap());
    });
    let text = canonical(&value, bytes).unwrap();
    let text_comparison = peak(|| {
        assert!(canonical_matches(&value, &text, bytes).unwrap());
    });
    let json = peak(|| {
        validate_json(&value, bytes).unwrap();
    });
    let canonical = peak(|| {
        assert_eq!(canonical_size(&value, bytes).unwrap(), bytes);
    });
    let borrowed = peak(|| {
        assert_eq!(
            canonical_object_size(
                value
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| (key, value)),
                bytes
            )
            .unwrap(),
            bytes
        );
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
        "JSON validation allocation peaks beside owned payload: wire={bytes}, materialized={materialized}, JSON={json}, canonical={canonical}, borrowed={borrowed}, schema={schema}, refused={refused}, original-equality={original_equality}, borrowed-equality={borrowed_equality}, text-comparison={text_comparison}"
    );
    assert!(materialized >= bytes as isize);
    assert!(original_equality >= 2 * bytes as isize);
    for amount in [
        json,
        canonical,
        borrowed,
        schema,
        refused,
        borrowed_equality,
        text_comparison,
    ] {
        assert!(
            amount < 64 * 1024,
            "Validation retained a payload-sized temporary: {amount}"
        );
    }
}
