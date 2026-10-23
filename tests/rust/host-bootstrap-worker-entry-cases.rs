use super::ServiceFixture;
use ivy_contracts::{IvyJson, json};
use ivy_host_executor::worker::WorkerPaths;
use std::{ffi::OsString, process::Command};

#[test]
fn worker_arguments_admit_native_paths_and_refuse_invalid_options() {
    let fixture = ServiceFixture::new();
    let required = vec![
        "--config",
        "config.json",
        "--request",
        "request.json",
        "--distribution",
        "artifact",
    ];
    let with = |extra: Vec<&'static str>| {
        let mut args = required.clone();
        args.extend(extra);
        args
    };
    let defaults = [
        fixture.0.join("config.json"),
        fixture.0.join("request.json"),
        fixture.0.join("artifact"),
    ];
    // The existing twenty argument cases now assert the worker contract directly.
    for (args, expected) in [
        (required.clone(), defaults.clone()),
        (with(vec!["--"]), defaults.clone()),
        (
            with(vec!["--config", "later.json"]),
            [
                fixture.0.join("later.json"),
                defaults[1].clone(),
                defaults[2].clone(),
            ],
        ),
        (
            vec![
                "--config=café/../配置.json",
                "--request=request=file.json",
                "--distribution=artifact",
            ],
            [
                fixture.0.join("配置.json"),
                fixture.0.join("request=file.json"),
                defaults[2].clone(),
            ],
        ),
        (
            vec!["--config=-config.json", "--request=-", "--distribution=."],
            [
                fixture.0.join("-config.json"),
                fixture.0.join("-"),
                fixture.0.clone(),
            ],
        ),
        (
            vec![
                "--config",
                "-",
                "--request",
                "request",
                "--distribution",
                "../artifact",
            ],
            [
                fixture.0.join("-"),
                fixture.0.join("request"),
                fixture.0.parent().unwrap().join("artifact"),
            ],
        ),
        (
            with(vec!["--request=a=b=c"]),
            [
                defaults[0].clone(),
                fixture.0.join("a=b=c"),
                defaults[2].clone(),
            ],
        ),
    ] {
        let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
        let paths = WorkerPaths::parse(&args, &fixture.0).unwrap();
        assert_eq!(
            [paths.config, paths.request, paths.distribution],
            expected,
            "{args:?}"
        );
        assert!(paths.node_executable.is_none());
    }
    for (args, code) in [
        (vec![], "invalid_arguments"),
        (with(vec!["--config="]), "invalid_arguments"),
        (with(vec!["--request", ""]), "invalid_arguments"),
        (with(vec!["--distribution="]), "invalid_arguments"),
        (with(vec!["--config"]), "internal_error"),
        (with(vec!["--request", "--distribution"]), "internal_error"),
        (with(vec!["--config", "-value"]), "internal_error"),
        (with(vec!["--bad=value"]), "internal_error"),
        (with(vec!["positional"]), "internal_error"),
        (with(vec!["--", "--config=later"]), "internal_error"),
        (vec!["-c", "config"], "internal_error"),
        (
            vec![
                "--no-config",
                "--config",
                "config.json",
                "--request",
                "request.json",
                "--distribution",
                "artifact",
            ],
            "internal_error",
        ),
        (
            vec!["--config", "--", "--request", "r", "--distribution", "d"],
            "internal_error",
        ),
    ] {
        let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            WorkerPaths::parse(&args, &fixture.0).err().unwrap().code,
            code,
            "{args:?}"
        );
    }
}

#[test]
fn private_workload_input_keeps_absolute_file_admission() {
    let cwd = std::env::current_dir().unwrap();
    let args = [
        "--config=c",
        "--request=r",
        "--distribution=d",
        "--node-executable=relative-node",
    ]
    .map(OsString::from);
    let paths = WorkerPaths::parse(&args, &cwd).unwrap();
    assert_eq!(paths.node_executable, Some("relative-node".into()));
    assert!(paths.config.is_absolute());
    assert!(paths.request.is_absolute());
    assert!(paths.distribution.is_absolute());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(ivy_host_executor::worker::run(&args))
        .unwrap_err();
    assert_eq!(error.code, "unsupported_runtime");
    assert_eq!(error.outcome, ivy_host_runtime::Outcome::NotExecuted);
}

#[test]
fn native_worker_error_exit_precedes_node_and_os_effects() {
    for (args, code) in [
        (vec![], "invalid_arguments"),
        (vec!["--unknown"], "internal_error"),
        (
            vec![
                "--config=c",
                "--request=r",
                "--distribution=d",
                "--node-executable=relative-node",
            ],
            "unsupported_runtime",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ivy-bootstrap-auto-worker"))
            .args(args)
            .env_remove("IVY_NODE_EXECUTABLE")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = json::parse(std::str::from_utf8(&output.stderr).unwrap(), 1024).unwrap();
        assert_eq!(
            error,
            IvyJson::object([
                ("code", code.into()),
                (
                    "message",
                    "Automatic bootstrap update needs attention.".into()
                ),
            ])
        );
    }
}

#[cfg(windows)]
#[test]
fn native_worker_path_values_retain_utf16_units() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let cwd = std::env::current_dir().unwrap();
    let request = [0xd800, 0x61];
    let inline = "--request="
        .encode_utf16()
        .chain(request)
        .collect::<Vec<_>>();
    for args in [
        vec![
            "--config=c".into(),
            OsString::from_wide(&inline),
            "--distribution=d".into(),
        ],
        vec![
            "--config=c".into(),
            "--request".into(),
            OsString::from_wide(&request),
            "--distribution=d".into(),
        ],
    ] {
        let parsed = WorkerPaths::parse(&args, &cwd).unwrap();
        assert!(
            parsed
                .request
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>()
                .ends_with(&request)
        );
    }
}

#[cfg(unix)]
#[test]
fn native_worker_path_values_retain_os_bytes() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let cwd = std::env::current_dir().unwrap();
    let request = [0xff, b'a'];
    let inline = [b"--request=".as_slice(), &request].concat();
    for args in [
        vec![
            "--config=c".into(),
            OsString::from_vec(inline),
            "--distribution=d".into(),
        ],
        vec![
            "--config=c".into(),
            "--request".into(),
            OsString::from_vec(request.to_vec()),
            "--distribution=d".into(),
        ],
    ] {
        let parsed = WorkerPaths::parse(&args, &cwd).unwrap();
        assert!(parsed.request.as_os_str().as_bytes().ends_with(&request));
    }
}
