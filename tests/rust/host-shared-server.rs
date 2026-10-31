use ivy_codex_server::foreground_arguments;
use ivy_contracts::{IvyJson as J, json::parse};

#[test]
fn server_owner_admits_only_foreground_local_listeners() {
    for args in [
        r#"["app-server","--listen","unix://"]"#,
        r#"["app-server","--listen","unix:///selected/socket"]"#,
    ] {
        let value = J::object([("args", parse(args, usize::MAX).unwrap())]);
        assert_eq!(foreground_arguments(&value).unwrap().len(), 3);
    }
    for args in [
        r#"["app-server","daemon","start"]"#,
        r#"["app-server","proxy","--sock"]"#,
        r#"["ui-server","--listen","unix://"]"#,
        r#"["ui-server","--listen","ws://0.0.0.0:1234"]"#,
        r#"["ui-server","--listen","unix://","--daemon"]"#,
        r#"null"#,
    ] {
        let value = J::object([("args", parse(args, usize::MAX).unwrap())]);
        assert!(foreground_arguments(&value).is_err());
    }
}
