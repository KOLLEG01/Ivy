use ivy_sdk::{JsString, collation::LocaleComparator};
use std::{cmp::Ordering, sync::Arc};

#[test]
fn display_languages_use_compiled_data_and_contextual_case_mapping() {
    let english = LocaleComparator::for_locale("en").unwrap();
    let swedish = LocaleComparator::for_locale("sv").unwrap();
    let turkish = LocaleComparator::for_locale("tr").unwrap();
    assert_eq!(english.compare_text("ä", "z"), Ordering::Less);
    assert_eq!(swedish.compare_text("ä", "z"), Ordering::Greater);
    assert_eq!(english.compare_text("é", "e\u{301}"), Ordering::Equal);
    assert_eq!(turkish.lowercase_text("Iİ"), "ıi");
    assert_eq!(english.lowercase_text("Iİ"), "ii\u{307}");
    assert_eq!(english.lowercase_text("ΟΣ"), "ος");
    assert_eq!(
        LocaleComparator::for_locale("lt")
            .unwrap()
            .lowercase_text("I\u{301}"),
        "i\u{307}\u{301}"
    );
    LocaleComparator::system().unwrap();
    assert!(LocaleComparator::for_locale("not a language tag").is_err());
}

#[test]
fn lossless_json_casing_retains_invalid_units_and_context() {
    let locale = LocaleComparator::for_locale("en").unwrap();
    let mut original = JsString(vec![0xd800]);
    original.0.extend("ΟΣ".encode_utf16());
    original.0.extend([0xdc00, 65, 0]);
    let mut expected = JsString(vec![0xd800]);
    expected.0.extend("ος".encode_utf16());
    expected.0.extend([0xdc00, 97, 0]);
    assert_eq!(locale.lowercase(&original).unwrap(), expected);
}

#[test]
fn compiled_display_policy_can_be_shared_without_native_resource_ownership() {
    let locale = Arc::new(LocaleComparator::for_locale("tr").unwrap());
    let child = {
        let locale = locale.clone();
        std::thread::spawn(move || locale.lowercase_text("I").into_owned())
    };
    assert_eq!(locale.lowercase_text("I"), "ı");
    assert_eq!(child.join().unwrap(), "ı");
}
