use ivy_contracts::JsString;
use ivy_host_runtime::health::truncate_details;

#[test]
fn health_details_drop_excess_capacity_without_changing_utf16_prefix_or_copying_small_inputs() {
    for (capacity, units) in [
        (0, Vec::new()),
        (16, vec![65, 0xd800, 0xdc01, 0xdfff]),
        (4096, vec![0xdfff]),
        (65536, Vec::new()),
        (65536, vec![65, 0xd800, 0xdc01, 0xdfff]),
        (65536, [vec![65; 4095], vec![0xd83d, 0xde00]].concat()),
    ] {
        let expected = units[..units.len().min(4096)].to_vec();
        let mut body = Vec::with_capacity(capacity);
        body.extend(units);
        let original = body.as_ptr();
        let details = truncate_details(JsString(body));
        assert_eq!(details.0, expected);
        assert!(
            details.0.capacity() <= 4096,
            "The admitted diagnostic must not keep discarded UTF-16 capacity"
        );
        if capacity <= 4096 {
            assert_eq!(details.0.as_ptr(), original);
        }
        let retained = details.0.as_ptr();
        let details = truncate_details(details);
        assert_eq!(details.0, expected);
        assert_eq!(
            details.0.as_ptr(),
            retained,
            "The shared file boundary must move an already bounded diagnostic"
        );
    }
}
