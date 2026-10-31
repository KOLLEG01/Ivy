use super::*;

fn rank(state: &mut State, content: &str) -> IvyJson {
    state
        .project(
            ValueRef::Text(content.as_bytes()),
            ValueRef::Text(b"/rank"),
            |value| Ok(value.clone()),
        )
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

#[test]
fn sql_scalar_projections_do_not_copy_selected_containers() {
    use crate::retention::graph_tests::{LIVE, PEAK};
    use std::sync::atomic::Ordering;
    const CHILD: &str = "IVY_SQL_PROJECTION_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "store::query::cache_tests::sql_scalar_projections_do_not_copy_selected_containers",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
            "Memory child selected no test"
        );
        return;
    }
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let content = serde_json::json!({
        "object": {"padding":"x".repeat(128 * 1024)},
        "array": ["x".repeat(128 * 1024)],
    })
    .to_string();
    let mut statement = store.connection().prepare(
        "SELECT ivy_type(?1,?2),ivy_value(?1,?2),ivy_is_null(?1,?2),ivy_contains(?1,?2,'x'),ivy_recency(?1,?2)"
    ).unwrap();
    let mut maximum = 0;
    for pointer in ["/object", "/array"] {
        let mut query = || {
            statement
                .query_row([content.as_str(), pointer], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, f64>(4)?,
                    ))
                })
                .unwrap()
        };
        assert_eq!(query(), (9, None, 0, 0.0, 0.0));
        assert_eq!(store.query_state.0.lock().unwrap().cache.len(), 1);
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let result = query();
        maximum = maximum.max(PEAK.load(Ordering::Relaxed) - before);
        assert_eq!(result, (9, None, 0, 0.0, 0.0));
    }
    eprintln!(
        "Hive SQL scalar projection Rust allocation peak: {maximum} bytes above cached 256 KiB JSON input"
    );
    assert!(
        maximum < 64 * 1024,
        "SQL scalar projection copied a JSON container: {maximum}"
    );
}
