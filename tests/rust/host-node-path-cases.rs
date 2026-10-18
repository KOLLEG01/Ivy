use ivy_contracts::{IvyJson, JsString, json};
use ivy_host_runtime::node_path::{self, Context, Style};
use std::{collections::BTreeMap, path::PathBuf, process::Command};

#[test]
fn lexical_paths_match_node_for_roots_devices_dot_segments_case_and_utf16() {
    let output=Command::new("node").arg("tests/fixtures/host-node-path-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let cases=json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap();
    let cases=cases.as_array().unwrap();assert!(cases.len()>15_000);
    for case in cases {
        let style=if case.required("style").unwrap().as_str().unwrap()=="win32" {Style::Windows}else{Style::Posix};
        let cwd=case.required("cwd").unwrap().as_js_string().unwrap().clone();
        let drives=case.required("drives").unwrap();
        let drive_cwds=["C:","D:"].map(|key|(JsString::from(key),drives.required(key).unwrap().as_js_string().unwrap().clone())).into_iter().collect();
        let context=Context {style,cwd,drive_cwds};
        let args:Vec<_>=case.required("args").unwrap().as_array().unwrap().iter().map(|v|v.as_js_string().unwrap().clone()).collect();
        let actual=match case.required("operation").unwrap().as_str().unwrap().as_str() {
            "normalize"=>IvyJson::String(node_path::normalize(style,&args[0])),
            "isAbsolute"=>node_path::is_absolute(style,&args[0]).into(),
            "dirname"=>IvyJson::String(node_path::dirname(style,&args[0])),
            "basename"=>IvyJson::String(node_path::basename(style,&args[0])),
            "inside"=>context.inside(&args[0],&args[1]).into(),
            "join"=>IvyJson::String(node_path::join(style,&args)),
            "resolve"=>IvyJson::String(context.resolve(&args)),
            "relative"=>IvyJson::String(context.relative(&args[0],&args[1])),
            _=>panic!("Unknown path operation"),
        };
        assert_eq!(&actual,case.required("expected").unwrap(),"{case:?}");
    }
}

#[test]
fn native_conversion_and_current_context_retain_host_paths() {
    let context=Context::current().unwrap();
    assert_eq!(context.style,Style::native());
    assert_eq!(context.resolve(&[]),node_path::from_native(&std::env::current_dir().unwrap()));
    assert_eq!(node_path::to_native(&node_path::from_native(&std::env::current_dir().unwrap())),std::env::current_dir().unwrap());
    let value=JsString(vec![67,58,92,0xd800,92,0xdc00]);
    #[cfg(windows)] assert_eq!(node_path::from_native(&node_path::to_native(&value)),value);
    #[cfg(not(windows))] assert_eq!(node_path::from_native(&node_path::to_native(&value)),JsString(vec![67,58,92,0xfffd,92,0xfffd]));
    let no_drive=Context {style:Style::Windows,cwd:"C:\\work".into(),drive_cwds:BTreeMap::new()};
    assert_eq!(no_drive.resolve(&["D:child".into()]),JsString::from("D:\\child"));
}
