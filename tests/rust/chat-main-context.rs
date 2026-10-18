use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    main_context::{
        load_main_instructions, main_input_origin, main_thread_options, main_turn_context,
    },
    native_plan::chat_turn_options,
};
use ivy_contracts::{
    IvyJson as Json, IvyResult,
    json::{canonical, parse, stringify},
};
use std::{path::Path, process::Command, sync::OnceLock};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-chat-context-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("instructions")).unwrap();
        Self(path)
    }
    fn asset(&self) -> std::path::PathBuf {
        self.0.join("instructions/chat-main-instructions.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn oracle() -> &'static Json {
    static VALUE: OnceLock<Json> = OnceLock::new();
    VALUE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-main-context-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_MAIN_CONTEXT_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
fn compare(kind: &str) {
    let fixture = oracle();
    let original = fixture
        .required("instructions")
        .unwrap()
        .as_js_string()
        .unwrap();
    let path = Fixture::new();
    std::fs::write(
        path.asset(),
        STANDARD
            .decode(
                fixture
                    .required("instructionAsset")
                    .unwrap()
                    .as_str()
                    .unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    let instructions = load_main_instructions(&path.0).unwrap();
    assert_eq!(&instructions, original);
    for case in fixture.required("cases").unwrap().as_array().unwrap() {
        let category = case.required("kind").unwrap().as_str().unwrap();
        if (category == "options") != (kind == "options") {
            continue;
        }
        let template = case.get("template").cloned().unwrap_or(Json::Null);
        let before = template.clone();
        let run = || -> IvyResult<Json> {
            match category.as_str() {
                "thread" => main_thread_options(&template, &instructions),
                "turn" => main_turn_context(
                    &template,
                    case.required("origin")?.as_js_string().unwrap(),
                    &instructions,
                ),
                "origin" => Ok(Json::String(main_input_origin(
                    case.required("caller")?.as_js_string().unwrap(),
                    case.required("request")?,
                )?)),
                "options" => chat_turn_options(&template, case.get("options")),
                _ => panic!("Unknown compositor"),
            }
        };
        let actual = match run() {
            Ok(value) => Json::object([
                ("serialized", stringify(&value, usize::MAX).unwrap().into()),
                ("value", value),
            ]),
            Err(error) => Json::object([(
                "error",
                Json::object([
                    ("code", error.code.into()),
                    ("message", error.message.into()),
                    ("outcome", error.outcome.as_str().into()),
                ]),
            )]),
        };
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(case.required("expected").unwrap(), usize::MAX).unwrap(),
            "{}",
            case.required("label").unwrap().as_str().unwrap()
        );
        assert_eq!(template, before, "The caller's retained template changed");
    }
}
#[test]
fn main_thread_restrictions_context_and_notice_origins_match_original() {
    compare("context");
}
#[test]
fn collaboration_preferences_preserve_original_precedence_and_templates() {
    compare("options");
}

#[test]
fn instruction_asset_preserves_utf16_and_refuses_missing_or_invalid_data() {
    let path = Fixture::new();
    let error = load_main_instructions(&path.0).unwrap_err();
    assert_eq!(error, ivy_contracts::IvyError::internal());
    for bytes in [
        b"not json".as_slice(),
        b"{}",
        b"null",
        b"[1]",
        b"true",
        &[0xff],
        b"\xef\xbb\xbf\"BOM\"",
    ] {
        std::fs::write(path.asset(), bytes).unwrap();
        assert_eq!(
            load_main_instructions(&path.0).unwrap_err(),
            ivy_contracts::IvyError::internal()
        );
    }
    std::fs::write(path.asset(), b"\"line\\n\\ud800-\\udfff\"\n").unwrap();
    let value = load_main_instructions(&path.0).unwrap();
    assert_eq!(value.0, vec![108, 105, 110, 101, 10, 0xd800, 45, 0xdfff]);
}
