use ivy_hive::admission::{
    AuthenticationRate, AuthenticationRateOptions, RequestBudget, RequestBudgetOptions,
};
use std::time::{Duration, Instant};

#[test]
fn authentication_bounds_expire_global_and_client_windows_independently() {
    let mut rate = AuthenticationRate::new(AuthenticationRateOptions {
        window: Duration::from_millis(10),
        maximum: 3,
        per_client: 2,
        client_maximum: 2,
    });
    let epoch = Instant::now();
    let at = |ms| epoch + Duration::from_millis(ms);
    rate.admit(&"a".into(), at(0)).unwrap();
    rate.admit(&"a".into(), at(1)).unwrap();
    let failure = rate.admit(&"a".into(), at(2)).unwrap_err();
    assert_eq!(
        failure.details.unwrap().get("scope"),
        Some(&"client".into())
    );
    rate.admit(&"b".into(), at(3)).unwrap();
    let failure = rate.admit(&"b".into(), at(4)).unwrap_err();
    assert_eq!(
        failure.details.unwrap().get("scope"),
        Some(&"global".into())
    );
    rate.admit(&"a".into(), at(10)).unwrap();
    rate.admit(&"b".into(), at(11)).unwrap();
    assert!(rate.admit(&"b".into(), at(12)).is_err());
    rate.admit(&"b".into(), at(13)).unwrap();
    assert_eq!(
        rate.admit(&"c".into(), at(13)).unwrap_err().message,
        "Authentication client budget is full."
    );
    rate.admit(&"c".into(), at(21)).unwrap();
}

#[test]
fn failed_growth_and_idempotent_release_preserve_shared_and_consumer_usage() {
    let budget = RequestBudget::new(RequestBudgetOptions {
        maximum: 3,
        maximum_bytes: 15,
        per_consumer: 2,
        per_consumer_bytes: 10,
        name: "request".into(),
    });
    let mut a = budget.acquire(&"a".into(), Some(6)).unwrap();
    let mut b = budget.acquire(&"b".into(), Some(6)).unwrap();
    let error = a.grow(5).unwrap_err();
    let details = error.details.unwrap();
    assert_eq!(details.get("scope"), Some(&"global".into()));
    assert_eq!(
        details.get("active"),
        Some(&ivy_contracts::IvyJson::Number(12.0))
    );
    a.grow(3).unwrap();
    let mut second = budget.acquire(&"a".into(), Some(0)).unwrap();
    assert!(budget.acquire(&"a".into(), Some(0)).is_err());
    assert!(b.grow(1).is_err());
    a.release();
    a.release();
    assert!(a.grow(0).is_err());
    assert!(second.grow(10).is_err());
    second.grow(9).unwrap();
    second.release();
    b.release();
    let mut reused = budget.acquire(&"a".into(), Some(10)).unwrap();
    reused.release();
}

#[test]
fn native_byte_totals_reject_overflow_without_changing_usage() {
    let budget = RequestBudget::new(RequestBudgetOptions {
        maximum: 2,
        maximum_bytes: usize::MAX,
        per_consumer: 2,
        per_consumer_bytes: usize::MAX,
        name: "request".into(),
    });
    let mut a = budget
        .acquire(&"a".into(), Some(usize::MAX / 2 + 1))
        .unwrap();
    assert!(
        budget
            .acquire(&"b".into(), Some(usize::MAX / 2 + 1))
            .is_err()
    );
    assert!(a.grow(usize::MAX).is_err());
    a.release();
    let mut b = budget.acquire(&"b".into(), Some(usize::MAX)).unwrap();
    b.release();
    let mut reused = budget.acquire(&"a".into(), Some(0)).unwrap();
    reused.release();
}

#[test]
fn abandoned_observer_does_not_refund_work_owned_elsewhere() {
    let budget = RequestBudget::new(RequestBudgetOptions {
        maximum: 1,
        ..Default::default()
    });
    let lease = budget.acquire(&"a".into(), None).unwrap();
    drop(lease);
    assert!(budget.acquire(&"a".into(), None).is_err());
}
