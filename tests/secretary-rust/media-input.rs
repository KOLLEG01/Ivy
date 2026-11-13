use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyJson, json::parse};
use ivy_secretary::media_input::{Source, prepare};
#[path = "../fixtures/reference-data.rs"]
mod reference_data;

fn corpus(bytes: &[u8]) -> IvyJson {
    parse(std::str::from_utf8(&reference_data::read(bytes)).unwrap(), 4 * 1024 * 1024).unwrap()
}
#[test]
fn original_media_signature_encoding_integrity_and_exact_budget_differential() {
    let corpus = corpus(include_bytes!("media-input-cases.json.gz"));
    let cases = corpus.as_array().unwrap();
    assert!(
        cases.len() > 300,
        "complete signature/size boundary corpus required"
    );
    for (index, case) in cases.iter().enumerate() {
        let wire = case.required("source").unwrap();
        let source = Source {
            name: wire
                .required("name")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            media_type: wire.required("mediaType").unwrap().as_js_string().cloned(),
            byte_length: wire.required("byteLength").unwrap().as_f64().unwrap() as usize,
            content_hash: wire.required("contentHash").unwrap().as_str().unwrap(),
        };
        let bytes = STANDARD
            .decode(case.required("bytes").unwrap().as_str().unwrap())
            .unwrap();
        let budget = case.required("budget").unwrap().as_f64().unwrap();
        let actual = match prepare(&source, &bytes, budget) {
            Ok(value) => IvyJson::object([("value", value.to_json())]),
            Err(error) => IvyJson::object([("code", error.code.into())]),
        };
        assert_eq!(&actual, case.required("expected").unwrap(), "case {index}");
    }
}

#[test]
fn catalogue_identity_preserves_exact_observed_account_differential() {
    let corpus = corpus(include_bytes!("source-identity-cases.json.gz"));
    for case in corpus.as_array().unwrap() {
        let actual = match ivy_secretary::source_identity::catalogue_account_hash(
            case.required("value").unwrap(),
        ) {
            Ok(hash) => IvyJson::object([("hash", hash.into())]),
            Err(error) => IvyJson::object([("code", error.code.into())]),
        };
        assert_eq!(&actual, case.required("expected").unwrap());
    }
}
