use ivy_contracts::{IvyJson as J, json::parse};
use ivy_dashboards::{
    raster::ColorMode,
    sources::Source,
    validation::{Definition, Input, OutputOptions},
};

fn definition() -> J {
    J::object([
        ("title", " \u{2003}Weather\u{a0} ".into()),
        ("html", " <p>forecast</p>\n".into()),
        ("sources", J::Object(vec![])),
    ])
}

fn rejects_definition(field: &str, value: J) {
    let mut input = definition();
    input.insert(field, value);
    assert_eq!(
        Definition::parse(&input).err().unwrap().code,
        "invalid_arguments",
        "{field}"
    );
}

#[test]
fn defaults_and_source_selectors_produce_typed_values() {
    let mut input = definition();
    input.insert(
        "sources",
        J::object([
            ("current", J::object([("objectId", "object".into())])),
            (
                "history",
                J::object([("contractKey", "weather/history".into())]),
            ),
        ]),
    );
    let parsed = Definition::parse(&input).unwrap();
    assert_eq!(parsed.title, "Weather".into());
    assert_eq!(parsed.html, " <p>forecast</p>\n".into());
    assert_eq!(parsed.refresh_seconds, 5);
    assert_eq!(parsed.metadata, J::Object(vec![]));
    assert!(parsed.image_token.is_none());
    assert!(matches!(&parsed.sources[0].1, Source::Object(id) if id == &"object".into()));
    assert!(
        matches!(&parsed.sources[1].1, Source::Contract { key, limit: 50 } if key == &"weather/history".into())
    );
    let saved = parsed.into_json();
    assert_eq!(
        saved
            .required("sources")
            .unwrap()
            .required("history")
            .unwrap()
            .required("limit")
            .unwrap(),
        &J::Number(50.0)
    );

    assert!(matches!(
        Input::parse("list", &J::Object(vec![])).unwrap(),
        Input::List {
            cursor: None,
            limit: 50
        }
    ));
    let output = OutputOptions::parse(&J::Object(vec![])).unwrap();
    assert_eq!(
        (output.width, output.height, output.mode),
        (800, 600, ColorMode::Color)
    );
}

#[test]
fn wrong_types_are_rejected_without_length_or_numeric_coercion() {
    for title in [
        J::Null,
        J::Bool(true),
        J::Number(1.0),
        J::Array(vec!["title".into()]),
        J::Object(vec![]),
        J::object([("length", "0x1".into())]),
        J::object([("length", J::Array(vec![J::Number(201.0)]))]),
    ] {
        let mut input = definition();
        input.insert("title", title);
        let error = Definition::parse(&input).err().unwrap();
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(error.message, "Invalid dashboard.title: expected a string.");
    }
    for width in [
        "800".into(),
        J::Bool(true),
        J::Array(vec![J::Number(800.0)]),
        J::object([("length", J::Number(800.0))]),
    ] {
        assert_eq!(
            OutputOptions::parse(&J::object([("width", width)]))
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    for name in ["read", "data", "delete", "render", "image"] {
        let error = Input::parse(name, &J::object([("id", J::Number(1.0))]))
            .err()
            .unwrap();
        assert_eq!(error.code, "invalid_arguments", "{name}");
        assert!(
            error.message.contains("expected a string"),
            "{name}: {error}"
        );
    }
}

#[test]
fn definition_limits_refresh_and_source_rules_are_enforced() {
    for (field, value) in [
        ("title", " \n\t".into()),
        ("title", "é".repeat(201).into()),
        ("html", "".into()),
        ("html", "x".repeat(500_001).into()),
        ("sources", J::Array(vec![])),
        (
            "sources",
            J::object([("bad-name", J::object([("objectId", "x".into())]))]),
        ),
        (
            "sources",
            J::object([("__proto__", J::object([("objectId", "x".into())]))]),
        ),
        (
            "sources",
            J::object([(
                "feed",
                J::object([("objectId", "x".into()), ("contractKey", "y".into())]),
            )]),
        ),
        (
            "sources",
            J::object([(
                "feed",
                J::object([("contractKey", "x".into()), ("limit", J::Number(101.0))]),
            )]),
        ),
        (
            "sources",
            J::Object(
                (0..21)
                    .map(|i| {
                        (
                            format!("feed{i}").into(),
                            J::object([("objectId", "x".into())]),
                        )
                    })
                    .collect(),
            ),
        ),
        ("refreshSeconds", J::Number(4.0)),
        ("refreshSeconds", J::Number(86_401.0)),
        ("refreshSeconds", J::Number(5.5)),
        ("refreshSeconds", "5".into()),
        ("imageToken", "a".repeat(31).into()),
        ("imageToken", "a".repeat(129).into()),
        ("imageToken", "!".repeat(32).into()),
    ] {
        rejects_definition(field, value);
    }
    let mut input = definition();
    input.insert("title", "é".repeat(200).into());
    input.insert("html", "x".repeat(500_000).into());
    input.insert("imageToken", "a_0-".repeat(32).into());
    input.insert(
        "sources",
        J::object([(
            "feed",
            J::object([("contractKey", "x".into()), ("limit", J::Number(100.0))]),
        )]),
    );
    for refresh in [0, 5, 86400] {
        input.insert("refreshSeconds", J::Number(f64::from(refresh)));
        assert_eq!(
            Definition::parse(&input).unwrap().refresh_seconds,
            refresh as u32
        );
    }
}

#[test]
fn metadata_preserves_json_keys_and_respects_the_encoded_header_limit() {
    let metadata = parse(
        r#"{"10":1,"2":2,"__proto__":{"nested":{"__proto__":true}},"text":"\ud800"}"#,
        usize::MAX,
    )
    .unwrap();
    let mut input = definition();
    input.insert("metadata", metadata.clone());
    let saved = Definition::parse(&input).unwrap().into_json();
    assert_eq!(saved.required("metadata").unwrap(), &metadata);

    // {"s":""} occupies 22 bytes after URL encoding; ASCII 'a' occupies one.
    input.insert("metadata", J::object([("s", "a".repeat(7978).into())]));
    assert!(Definition::parse(&input).is_ok());
    rejects_definition("metadata", J::object([("s", "a".repeat(7979).into())]));
    // Each emoji occupies twelve encoded UTF-8 bytes.
    rejects_definition("metadata", J::object([("s", "😀".repeat(665).into())]));
    rejects_definition("metadata", J::object([("refreshSeconds", J::Number(5.0))]));
    rejects_definition("metadata", J::Array(vec![]));
}

#[test]
fn update_revisions_are_positive_integers_paired_with_an_id() {
    let create = J::object([("value", definition())]);
    assert!(matches!(
        Input::parse("save", &create).unwrap(),
        Input::Save { target: None, .. }
    ));
    for revision in [1.0, 9_007_199_254_740_991.0] {
        let input = J::object([
            ("value", definition()),
            ("id", "id".into()),
            ("expectedRevision", J::Number(revision)),
        ]);
        assert!(
            matches!(Input::parse("save", &input).unwrap(), Input::Save { target: Some((_, n)), .. } if n == revision as u64)
        );
    }
    for revision in [
        J::Null,
        "1".into(),
        J::Bool(true),
        J::Number(0.0),
        J::Number(-1.0),
        J::Number(1.5),
        J::Number(9_007_199_254_740_992.0),
    ] {
        let input = J::object([
            ("value", definition()),
            ("id", "id".into()),
            ("expectedRevision", revision.clone()),
        ]);
        assert_eq!(
            Input::parse("save", &input).err().unwrap().code,
            "invalid_arguments"
        );
        let input = J::object([("id", "id".into()), ("expectedRevision", revision)]);
        assert_eq!(
            Input::parse("delete", &input).err().unwrap().code,
            "invalid_arguments"
        );
    }
    for (field, value) in [("id", "id".into()), ("expectedRevision", J::Number(1.0))] {
        let mut input = create.clone();
        input.insert(field, value);
        assert_eq!(
            Input::parse("save", &input).err().unwrap().code,
            "invalid_arguments"
        );
    }
}

#[test]
fn output_dimensions_pixel_budget_and_modes_are_checked() {
    for (width, height) in [(100.0, 100.0), (4096.0, 100.0), (2000.0, 4000.0)] {
        assert!(
            OutputOptions::parse(&J::object([
                ("width", J::Number(width)),
                ("height", J::Number(height))
            ]))
            .is_ok()
        );
    }
    for (width, height) in [
        (99.0, 100.0),
        (100.0, 4097.0),
        (2001.0, 4000.0),
        (100.5, 100.0),
        (f64::NAN, 100.0),
        (100.0, f64::INFINITY),
    ] {
        assert_eq!(
            OutputOptions::parse(&J::object([
                ("width", J::Number(width)),
                ("height", J::Number(height))
            ]))
            .unwrap_err()
            .code,
            "invalid_arguments"
        );
    }
    for (mode, expected) in [
        ("color", ColorMode::Color),
        ("grayscale", ColorMode::Grayscale),
        ("monochrome", ColorMode::Monochrome),
    ] {
        let output = OutputOptions::parse(&J::object([("colorMode", mode.into())])).unwrap();
        assert_eq!(output.mode, expected);
        assert_eq!(output.color_mode(), mode);
    }
    assert!(OutputOptions::parse(&J::object([("colorMode", "other".into())])).is_err());
    for token in ["", &"x".repeat(129)] {
        assert_eq!(
            Input::parse(
                "image",
                &J::object([("id", "id".into()), ("token", token.into())])
            )
            .err()
            .unwrap()
            .code,
            "invalid_arguments"
        );
    }
}

#[test]
fn tool_inputs_reject_unknown_fields_and_non_objects() {
    for name in ["list", "read", "save", "delete", "data", "render", "image"] {
        for input in [
            J::Null,
            J::Array(vec![]),
            J::object([("unknown", J::Bool(true))]),
        ] {
            assert_eq!(
                Input::parse(name, &input).err().unwrap().code,
                "invalid_arguments",
                "{name}"
            );
        }
    }
}
