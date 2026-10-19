use crate::rtcp::{hex, original};
use ivy_contracts::{IvyJson, json::stringify};
use ivy_phone_bridge::media::{
    MediaError, RtcpReceptionStatistics, RtcpReportBuilder, RtcpReportSchedule,
};
use std::time::Duration;
fn n(v: impl Into<f64>) -> IvyJson {
    IvyJson::Number(v.into())
}
fn action(kind: &str, values: &[u32]) -> IvyJson {
    IvyJson::Array(
        std::iter::once(kind.into())
            .chain(values.iter().map(|&v| n(v)))
            .collect(),
    )
}

#[test]
fn original_reception_statistics_match_controlled_clock_wrap_loss_and_sampling_intervals() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original statistics unavailable; differential not executed");
        return;
    }
    let scenarios: Vec<Vec<(u16, u32, u32)>> = vec![
        vec![
            (65534, 1000000, 0),
            (0, 1000320, 320),
            (65535, 1000160, 480),
            (2, 1000640, 640),
            (2, 1000640, 800),
            (65533, 999840, 960),
            (3, 1000800, 1120),
        ],
        (0..10)
            .map(|i| (1000 + i, 32000 + u32::from(i) * 160, u32::from(i) * 160))
            .collect(),
        vec![
            (100, 8000, 0),
            (102, 8320, 320),
            (101, 8160, 321),
            (102, 8320, 400),
            (103, 8480, 480),
        ],
        vec![
            (0, u32::MAX - 480, 0),
            (65280, 0, 1),
            (255, 480, u32::MAX),
            (65281, 960, 480),
            (256, 1440, 960),
            (65535, 1920, 1440),
            (0, 2400, 1920),
            (0, 2400, 1920),
        ],
        vec![
            (1, 0, 0),
            (2, 0, 0x80000000),
            (3, 0, 0),
            (4, u32::MAX, 0),
            (5, 1, u32::MAX),
        ],
    ];
    let mut cases = Vec::new();
    for source in [0, 9, u32::MAX] {
        for packets in &scenarios {
            for every_packet in [false, true] {
                let mut actions = vec![action("sample", &[0]), action("sample", &[0])];
                for &(seq, stamp, arrival) in packets {
                    actions.push(action("rx", &[u32::from(seq), stamp, arrival]));
                    if every_packet {
                        actions.push(action("sample", &[arrival]));
                    }
                }
                actions.push(action("sample", &[u32::MAX]));
                actions.push(IvyJson::Array(vec!["sr".into(), "8123456789abcdef".into()]));
                actions.push(action("sample", &[123]));
                actions.push(action("sample", &[u32::MAX]));
                cases.push(IvyJson::object([
                    ("op", "statistics".into()),
                    ("source", n(source)),
                    ("actions", IvyJson::Array(actions)),
                ]));
            }
        }
    }
    let mut random = Vec::new();
    let mut seed = 0x6a09e667_u32;
    for i in 0..4096_u32 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        random.push(action(
            "rx",
            &[seed & 65535, seed.rotate_left(13), seed.rotate_right(7)],
        ));
        if i % 7 == 0 {
            random.push(action("sample", &[seed]));
        }
    }
    random.push(action("sample", &[0]));
    cases.push(IvyJson::object([
        ("op", "statistics".into()),
        ("source", n(19)),
        ("actions", IvyJson::Array(random)),
    ]));
    let expected = original(&cases);
    assert_eq!(expected.len(), cases.len());
    let mut samples = 0;
    for (input, output) in cases.iter().zip(&expected) {
        assert_eq!(
            output.get("ok").unwrap().as_bool(),
            Some(true),
            "{output:?}"
        );
        let mut state =
            RtcpReceptionStatistics::new(input.get("source").unwrap().as_f64().unwrap() as u32);
        let trace = output.get("trace").unwrap().as_array().unwrap();
        let mut next = 0;
        for action in input.get("actions").unwrap().as_array().unwrap() {
            let a = action.as_array().unwrap();
            let value = |i| a[i].as_f64().unwrap() as u32;
            match a[0].as_str().unwrap() {
                "rx" => state.record(value(1) as u16, value(2), value(3)),
                "sr" => {
                    assert_eq!(trace[next].get("kind").unwrap().as_str(), Some("sr"));
                    let received =
                        trace[next].get("receivedNtp32").unwrap().as_f64().unwrap() as u32;
                    state.sender_report(
                        u64::from_str_radix(a[1].as_str().unwrap(), 16).unwrap(),
                        received,
                    );
                    next += 1;
                }
                "sample" => {
                    let sample = state.sample(value(1));
                    let bytes = RtcpReportBuilder::receiver_packet(0, &[sample]).unwrap();
                    assert_eq!(
                        hex(&bytes[8..]),
                        trace[next].get("hex").unwrap().as_str().unwrap(),
                        "source{} sample{}",
                        state.source(),
                        next
                    );
                    next += 1;
                    samples += 1;
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(next, trace.len());
    }
    assert!(samples > 800);
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_STATISTICS_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("cases", n(cases.len() as u32)),
                    ("samples", n(samples as u32)),
                    ("inputs", IvyJson::Array(cases)),
                    ("original", IvyJson::Array(expected)),
                ]),
                4_000_000,
            )
            .unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn report_schedule_matches_actual_original_timer_callbacks_sr_rr_and_timeout_latch() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original report timer unavailable; differential not executed");
        return;
    }
    let result = original(&[IvyJson::object([("op", "timer".into())])]).remove(0);
    assert_eq!(
        result.get("ok").unwrap().as_bool(),
        Some(true),
        "{result:?}"
    );
    assert_eq!(
        result.get("defaultTimeout").unwrap().as_f64(),
        Some(30000.0)
    );
    for (key, expected) in [
        ("firstRead", true),
        ("secondRead", true),
        ("afterTimer", false),
        ("timedOut", true),
        ("activityRecovered", true),
    ] {
        assert_eq!(result.get(key).unwrap().as_bool(), Some(expected), "{key}");
    }
    let trace = result.get("trace").unwrap().as_array().unwrap();
    assert_eq!(trace.len(), 3);
    assert_eq!(trace[0].get("sender").unwrap().as_bool(), Some(true));
    assert_eq!(trace[1].get("sender").unwrap().as_bool(), Some(false));
    assert_eq!(trace[2].get("bye").unwrap().as_bool(), Some(true));
    assert_eq!(
        trace[2].get("reason").unwrap().as_str(),
        Some("phone_media_closed")
    );
    for point in trace {
        assert_eq!(point.get("timeouts").unwrap().as_f64(), Some(1.0));
    }
    let ms = Duration::from_millis;
    let mut schedule = RtcpReportSchedule::new(0);
    schedule.set_timeout_ms(0);
    let intervals = result.get("intervals").unwrap().as_array().unwrap();
    assert_eq!(intervals.len(), 256);
    for value in intervals {
        let value = value.as_f64().unwrap() as u16;
        assert!((2500..=7500).contains(&value));
    }
    let first = intervals[0].as_f64().unwrap() as u16;
    assert!(schedule.start(Duration::ZERO, first).unwrap());
    assert!(!schedule.start(ms(1), first).unwrap());
    assert!(schedule.sender_report(1));
    assert!(schedule.sender_report(1));
    let due = ms(u64::from(first));
    assert!(
        schedule
            .begin_report(due - Duration::from_nanos(1), 1)
            .unwrap()
            .is_none()
    );
    assert!(schedule.begin_report(due, 1).unwrap().unwrap().new_timeout);
    assert!(schedule.timed_out());
    assert!(schedule.begin_report(due, 1).unwrap().is_none());
    let finish = due + ms(137);
    schedule.complete_report(finish, 2500, 1).unwrap();
    assert_eq!(schedule.next(), Some(finish + ms(2500)));
    assert!(!schedule.sender_report(1));
    assert!(schedule.sender_report(2));
    assert!(
        !schedule
            .begin_report(finish + ms(2500), 10000)
            .unwrap()
            .unwrap()
            .new_timeout
    );
    schedule.record_activity(10000);
    assert!(!schedule.timed_out());
    schedule.close();
    schedule
        .complete_report(finish + ms(2501), 2500, 2)
        .unwrap();
    assert_eq!(schedule.next(), None);
    assert!(!schedule.sender_report(2));
    assert_eq!(
        schedule.start(finish + ms(3000), 2500),
        Err(MediaError::Revoked)
    );
    let mut boundary = RtcpReportSchedule::new(0);
    boundary.start(ms(0), 2500).unwrap();
    assert!(
        !boundary
            .begin_report(ms(2500), 30000)
            .unwrap()
            .unwrap()
            .new_timeout
    );
    boundary.complete_report(ms(2500), 2500, u32::MAX).unwrap();
    assert!(!boundary.sender_report(0)); // Retain the original unsigned > comparison at wrap.
    assert!(
        boundary
            .begin_report(ms(5000), 30001)
            .unwrap()
            .unwrap()
            .new_timeout
    );
    boundary.complete_report(ms(5000), 2500, u32::MAX).unwrap();
    boundary.record_activity(1000);
    assert!(
        !boundary
            .begin_report(ms(7500), 0)
            .unwrap()
            .unwrap()
            .new_timeout
    ); // Wall clock may move backwards.
    boundary.complete_report(ms(7500), 2500, 1).unwrap();
    assert_eq!(
        boundary.begin_report(ms(1), 1),
        Err(MediaError::ClockWentBackwards)
    );
    let mut upper = RtcpReportSchedule::new(0);
    upper.start(ms(0), 7500).unwrap();
    assert_eq!(upper.next(), Some(ms(7500)));
    let mut overflow = RtcpReportSchedule::new(0);
    assert_eq!(
        overflow.start(Duration::MAX, 2500),
        Err(MediaError::ClockOverflow)
    );
    assert!(!overflow.started());
    assert_eq!(overflow.next(), None);
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_TIMER_TRACE") {
        std::fs::write(path, stringify(&result, 1_000_000).unwrap()).unwrap();
    }
}
