use super::*;
use ivy_contracts::json::{parse, stringify};
use ivy_host_runtime::{codex_home::resolve_codex_home_json, node_path::Style};
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

fn value(text: &str) -> IvyJson {
    parse(text, 1024 * 1024).unwrap()
}
fn text(item: &IvyJson, key: &str) -> JsString {
    item.get(key)
        .and_then(IvyJson::as_js_string)
        .unwrap()
        .clone()
}
fn code<T>(result: IvyResult<T>) -> String {
    result.err().expect("Expected launch refusal").code
}

#[test]
fn retained_account_context_preserves_native_home_selection() {
    let cases = value(include_str!("../fixtures/phone-voice-home-cases.json"));
    for item in cases.as_array().unwrap() {
        if item.get("platform") != Some(&if cfg!(windows) { "win32" } else { "linux" }.into()) {
            continue;
        }
        let mut paths = Context {
            style: if item.get("platform") == Some(&"win32".into()) {
                Style::Windows
            } else {
                Style::Posix
            },
            cwd: text(item, "pathsCwd"),
        };
        let mut environment: BTreeMap<_, _> = item
            .get("environment")
            .unwrap()
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.as_js_string().unwrap().clone()))
            .collect();
        let mut user_home = item
            .get("userHome")
            .and_then(IvyJson::as_js_string)
            .cloned();
        let mut cwd = text(item, "cwd");
        let mut account_home = text(item, "accountHome");
        let connector = NativeVoiceConnector::new(&HomeContext {
            paths: &paths,
            environment: &environment,
            user_home: user_home.as_ref(),
            cwd: &cwd,
            account_home: &account_home,
        });
        // The asynchronous owner must keep the original account inputs even if
        // its caller replaces its context before a later connection attempt.
        paths.cwd = "replaced".into();
        environment.clear();
        user_home = None;
        cwd = "replaced".into();
        account_home = "replaced".into();
        let retained = connector.0.borrowed();
        assert_eq!(retained.paths.cwd, text(item, "pathsCwd"));
        assert_eq!(user_home, None);
        assert_eq!(cwd, JsString::from("replaced"));
        assert_eq!(account_home, JsString::from("replaced"));
        let actual = resolve_codex_home_json(item.get("settings").unwrap(), &retained);
        if let Some(expected) = item.get("expectedError") {
            assert_eq!(IvyJson::from(actual.unwrap_err().code), *expected);
        } else {
            let actual = actual.unwrap();
            assert_eq!(actual.path, text(item, "expectedPath"));
            assert_eq!(JsString::from(actual.source), text(item, "expectedSource"));
        }
    }
}

fn paths() -> Context {
    Context {
        style: Style::native(),
        cwd: root("/account/work", "C:\\account\\work"),
    }
}
fn root(posix: &str, windows: &str) -> JsString {
    if cfg!(windows) {
        windows.into()
    } else {
        posix.into()
    }
}

#[tokio::test]
async fn concrete_connector_preserves_shared_pre_effect_refusals() {
    let paths = paths();
    let environment = BTreeMap::new();
    let account_home = root("/account", "C:\\account");
    let connector = NativeVoiceConnector::new(&HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: None,
        cwd: &paths.cwd,
        account_home: &account_home,
    });
    // Every row exits before the stop-fence read, catalog I/O or any launch.
    for (settings, expected) in [
        (IvyJson::Null, "invalid_arguments"),
        (
            value(r#"{"appServer":{"mode":"claude-adapter"}}"#),
            "native_transport_unavailable",
        ),
        (
            value(r#"{"codexHome":"relative","nativeExecutable":"relative"}"#),
            "invalid_arguments",
        ),
        (
            value(r#"{"nativeExecutable":"relative"}"#),
            "invalid_arguments",
        ),
    ] {
        let result = tokio::time::timeout(
            Duration::from_secs(4),
            connector.clone().connect(
                VoiceConnectOptions {
                    settings,
                    artifact_root: root("/artifacts", "C:\\artifacts"),
                    data_root: root("/runtime/codex-runtime", "C:\\runtime\\codex-runtime"),
                    credential: None,
                    client_name: "ivy-phone-bridge",
                    client_version: "0.1.0",
                },
                Events::default(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(code(result), expected);
    }
}

#[tokio::test]
async fn configured_control_uses_the_concrete_launcher_lazily() {
    let paths = paths();
    let environment = BTreeMap::new();
    let account_home = root("/account", "C:\\account");
    let opened = Arc::new(AtomicUsize::new(0));
    let count = opened.clone();
    let control = VoiceControl::configured(
        IvyJson::object([
            ("cwd", IvyJson::String(root("/work", "C:\\work"))),
            ("appServer", value(r#"{"mode":"claude-adapter"}"#)),
        ]),
        root("/artifacts", "C:\\artifacts"),
        root("/runtime", "C:\\runtime"),
        None,
        &HomeContext {
            paths: &paths,
            environment: &environment,
            user_home: None,
            cwd: &paths.cwd,
            account_home: &account_home,
        },
        VoiceControlHooks {
            events: Events::default(),
            on_open: Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
            on_error: Arc::new(|_| {}),
        },
    )
    .unwrap();
    assert_eq!(opened.load(Ordering::SeqCst), 0);
    assert!(control.current().is_none());
    assert_eq!(
        code(
            tokio::time::timeout(Duration::from_secs(4), control.open().wait())
                .await
                .unwrap()
        ),
        "native_transport_unavailable"
    );
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    control
        .close(Box::pin(async { Ok(()) }))
        .wait()
        .await
        .unwrap();
    assert_eq!(code(control.open().wait().await), "service_not_ready");
}
