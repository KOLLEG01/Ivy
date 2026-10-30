use super::*;

fn rank(state: &mut State, content: &str) -> IvyJson {
    state
        .value(ValueRef::Text(content.as_bytes()), ValueRef::Text(b"/rank"))
        .unwrap()
}

#[test]
fn oversized_tree_is_queried_without_displacing_cached_documents() {
    let mut state = State::default();
    assert_eq!(rank(&mut state, r#"{"rank":1}"#), IvyJson::Number(1.0));
    let large = serde_json::json!({"padding":"x".repeat(600_000),"rank":7}).to_string();
    assert!(large.len() < JSON_CACHE_ENTRY_BYTES);
    assert_eq!(rank(&mut state, &large), IvyJson::Number(7.0));
    assert_eq!(state.cache.len(), 1);
    assert!(!state.cache.contains_key(&large));
    assert_eq!(rank(&mut state, r#"{"rank":1}"#), IvyJson::Number(1.0));
}

#[test]
fn retained_trees_fill_the_byte_budget_before_the_entry_limit() {
    let mut state = State::default();
    for value in 0..8 {
        let content = serde_json::json!({"padding":"x".repeat(240_000),"rank":value}).to_string();
        assert_eq!(rank(&mut state, &content), IvyJson::Number(value as f64));
        assert!(state.cache_bytes <= JSON_CACHE_BYTES);
    }
    assert!(!state.cache.is_empty());
    assert!(state.cache.len() < 8);
}

#[test]
fn query_scope_releases_cache_on_success_and_failure() {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    for fail in [false, true] {
        let result = store.with_budget(Duration::from_secs(5), |store| {
            store
                .connection()
                .query_row("SELECT ivy_value('{\"rank\":7}','/rank')", [], |row| {
                    row.get::<_, f64>(0)
                })
                .unwrap();
            assert!(store.query_state.0.lock().unwrap().cache_bytes > 0);
            if fail {
                Err(IvyError::internal())
            } else {
                Ok(())
            }
        });
        assert_eq!(result.is_err(), fail);
        let state = store.query_state.0.lock().unwrap();
        assert!(state.cache.is_empty());
        assert_eq!(state.cache_bytes, 0);
    }
}
