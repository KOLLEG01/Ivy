use super::*;

#[test]
fn original_constructor_paths_options_and_bound_business_owners() {
    let original = construction_reference("serverConstructor");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let rows = original.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 25);
    for row in rows {
        let input = row.get("input").unwrap().get("options").unwrap();
        let native = match runtime.block_on(execute(input)) {
            Ok(value) => obj([("value", value)]),
            Err(Error::Ivy(error)) => obj([("error", error.to_wire())]),
            Err(Error::UrlConstructor { input }) => {
                obj([("urlConstructorInput", IvyJson::String(input))])
            }
        };
        let expected = row.get("expected").unwrap();
        assert!(
            &native == expected,
            "Input: {}\nNative: {}\nOriginal: {}",
            encode(row.get("input").unwrap()),
            encode(&native),
            encode(expected)
        );
    }
}
