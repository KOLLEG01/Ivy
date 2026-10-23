use ivy_contracts::{
    IvyJson, IvyResult,
    json::{canonical, parse},
};
use std::{future::Future, time::Duration};
pub(crate) fn fixture(mode: &str) -> IvyJson {
    let file = match mode {
        "process" => "phone-service-process-cases.json",
        "calls" => "phone-call-command-cases.json",
        "flow-controls" => "phone-flow-control-cases.json",
        "native" => "phone-service-native-cases.json",
        _ => panic!("Unknown Phone protocol fixture: {mode}"),
    };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures")
        .join(file);
    parse(&std::fs::read_to_string(path).unwrap(), 1024 * 1024).unwrap()
}
pub(crate) fn id(n: u32) -> IvyJson {
    format!("40000000-0000-4000-8000-{n:012}").into()
}
pub(crate) fn observed(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(v) => IvyJson::object([("ok", true.into()), ("value", v)]),
        Err(e) => IvyJson::object([
            ("ok", false.into()),
            ("code", e.code_js().into()),
            ("outcome", e.outcome_json()),
        ]),
    }
}
pub(crate) fn equal(actual: &IvyJson, expected: &IvyJson, name: &str) {
    assert_eq!(
        canonical(actual, 8 * 1024 * 1024).unwrap(),
        canonical(expected, 8 * 1024 * 1024).unwrap(),
        "{name}"
    );
}
pub(crate) async fn timed<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), future)
        .await
        .unwrap()
}
pub(crate) async fn until(mut condition: impl FnMut() -> bool) {
    timed(async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}
