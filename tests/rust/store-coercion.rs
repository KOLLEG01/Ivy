use ivy_contracts::{IvyJson, JsString, json::parse};
use ivy_store::receipts::has_unresolved_outcome_with;
use std::{cell::Cell, rc::Rc};

struct RawCause;

#[test]
fn coercion_preserves_the_borrowed_value_and_local_raw_cause() {
    let value = parse(r#"{"nested":[{"phase":{"toString":null}}]}"#, usize::MAX).unwrap();
    let phase = value.get("nested").unwrap().as_array().unwrap()[0]
        .get("phase")
        .unwrap();
    let cause = Rc::new(RawCause);
    let calls = Cell::new(0);
    let result = has_unresolved_outcome_with(&value, &|part| {
        assert!(std::ptr::eq(part, phase));
        calls.set(calls.get() + 1);
        Err(Rc::clone(&cause))
    });
    match result {
        Err(actual) => assert!(Rc::ptr_eq(&actual, &cause)),
        Ok(_) => panic!("raw coercion failure was swallowed"),
    }
    assert_eq!(calls.get(), 1);
}

#[test]
fn coercion_keeps_first_refusal_and_unresolved_short_circuit_order() {
    for unresolved_first in [true, false] {
        let malformed = IvyJson::object([("toString", IvyJson::Null)]);
        let properties = if unresolved_first {
            [("execution", "outcome_unknown".into()), ("code", malformed)]
        } else {
            [("code", malformed), ("execution", "outcome_unknown".into())]
        };
        let value = IvyJson::object(properties);
        let first = value
            .get(if unresolved_first {
                "execution"
            } else {
                "code"
            })
            .unwrap();
        let cause = Rc::new(RawCause);
        let calls = Cell::new(0);
        let result = has_unresolved_outcome_with(&value, &|part| {
            assert!(std::ptr::eq(part, first));
            calls.set(calls.get() + 1);
            match part {
                IvyJson::String(text) => Ok(JsString::clone(text)),
                _ => Err(Rc::clone(&cause)),
            }
        });
        match result {
            Ok(found) => assert!(unresolved_first && found),
            Err(actual) => {
                assert!(!unresolved_first);
                assert!(Rc::ptr_eq(&actual, &cause));
            }
        }
        assert_eq!(calls.get(), 1);
    }
}
