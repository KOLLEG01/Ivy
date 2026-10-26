use ivy_contracts::JsString;

#[test]
fn native_urls_preserve_http_origins_forms_and_redirects() {
    use ivy_contracts::whatwg_url::*;
    let input = JsString::from("HTTPS://user:pass@EXAMPLE.com:443/base?x=1#fragment");
    let value = parse_url(&input).unwrap();
    assert_eq!(
        value.href,
        "https://user:pass@example.com/base?x=1#fragment"
    );
    assert_eq!(
        (
            value.protocol.as_str(),
            value.hostname.as_str(),
            value.username.as_str(),
            value.password.as_str(),
            value.hash.as_str()
        ),
        ("https:", "example.com", "user", "pass", "#fragment")
    );
    let parts = url_components(&input).unwrap();
    assert_eq!(
        parse_url(&"custom:opaque  ?q".into()).unwrap().href,
        "custom:opaque %20?q"
    );
    for (input, expected) in [
        ("file://server/C:/base/", "file://server/C:/base/"),
        ("file://localhost/C:/base/", "file:///C:/base/"),
        ("file://C:/base/", "file:///C:/base/"),
    ] {
        assert_eq!(parse_url(&input.into()).unwrap().href, expected);
    }
    assert_eq!(
        (parts.origin.as_str(), parts.pathname.as_str()),
        ("https://example.com", "/base")
    );
    assert_eq!(
        parse_url_with_base(
            &"../oauth?state=a".into(),
            &"https://example.com/ivy/child".into()
        )
        .unwrap()
        .href,
        "https://example.com/oauth?state=a"
    );
    assert_eq!(
        parse_form(&"a=one&a=two&label=a+b&plus=%2B".into()),
        vec![
            ("a".into(), "one".into()),
            ("a".into(), "two".into()),
            ("label".into(), "a b".into()),
            ("plus".into(), "+".into())
        ]
    );
    let redirect = set_url_search_params(
        &"https://example.com/oauth?state=old&keep=1&state=extra#f".into(),
        &[
            ("state".into(), "new value".into()),
            ("code".into(), "✓".into()),
        ],
    )
    .unwrap();
    assert_eq!(
        redirect,
        JsString::from("https://example.com/oauth?state=new+value&keep=1&code=%E2%9C%93#f")
    );
    assert!(parse_url(&"not an absolute url".into()).is_err());
    assert!(parse_url(&JsString(vec![0xd800])).is_err());
    let mut surrogate = JsString::from("https://example.com/");
    surrogate.0.push(0xd800);
    assert_eq!(
        parse_url(&surrogate).unwrap().href,
        "https://example.com/%EF%BF%BD"
    );
    assert_eq!(
        parse_url_with_base(&JsString(vec![0xd800]), &"https://example.com/".into())
            .unwrap()
            .href,
        "https://example.com/%EF%BF%BD"
    );
    assert_eq!(
        set_url_search_params(
            &"https://example.com/".into(),
            &[(JsString(vec![0xd800]), JsString(vec![0xdfff]))]
        )
        .unwrap(),
        JsString::from("https://example.com/?%EF%BF%BD=%EF%BF%BD")
    );
    for (input, expected) in [
        (
            "https://user%40example:secret@example.com/path?x=1#f",
            "https://example.com/path?x=1#f",
        ),
        (
            "ssh://user:secret@git.example/path",
            "ssh://git.example/path",
        ),
        ("file:///C:/path", "file:///C:/path"),
        ("data:text/plain,x", "data:text/plain,x"),
    ] {
        assert_eq!(
            clear_url_credentials(&input.into()).unwrap(),
            JsString::from(expected)
        );
    }
    assert!(clear_url_credentials(&"not a url".into()).is_err());
    let mut remote = JsString::from("https://user:secret@example.com/");
    remote.0.push(0xd800);
    assert_eq!(
        clear_url_credentials(&remote).unwrap(),
        JsString::from("https://example.com/%EF%BF%BD")
    );
    assert_eq!(
        JsString::from("ΟΣ Iİ").to_english_lowercase(),
        JsString::from("ος ii\u{307}")
    );
    assert_eq!(
        JsString(vec![65, 0xd800, 66, 0xdc00, 67]).to_english_lowercase(),
        JsString(vec![97, 0xd800, 98, 0xdc00, 99])
    );
    assert_eq!(
        JsString::from("\u{2003}text\n").trim(),
        JsString::from("text")
    );
    assert_eq!(
        JsString::from("e\u{301}").normalize_nfc(),
        JsString::from("é")
    );
}
