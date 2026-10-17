use ivy_secretary::{policy::*, schedule::*};

#[test]
fn interval_is_epoch_aligned_bounded_and_never_rewinds() {
    let local = |_: i64, _: &str| -> Result<LocalMinute, ()> {
        panic!("interval must not consult local time")
    };
    let page = occurrences(
        Schedule::Interval { minutes: 2 },
        600_000,
        119_999,
        2,
        local,
    )
    .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|x| (x.key.as_str(), x.occurred_at))
            .collect::<Vec<_>>(),
        [("interval:2:1", 120_000), ("interval:2:2", 240_000)]
    );
    assert_eq!(page.checked_at, 240_000);
    assert_eq!(
        occurrences(Schedule::Interval { minutes: 2 }, 0, 1, 2, local).unwrap(),
        Page {
            items: vec![],
            checked_at: 1
        }
    );
    assert_eq!(
        occurrences(Schedule::Other, 0, 1, 2, local)
            .unwrap()
            .checked_at,
        1
    );
}

#[test]
fn civil_gap_uses_next_eligible_minute_and_fold_reuses_daily_identity() {
    let schedule = Schedule::Civil {
        weekly: false,
        zone: "Fixture",
        time: "02:30",
        weekdays: &[],
    };
    let page = occurrences(schedule.clone(), 60_000, 0, 10, |at, _| {
        Ok::<_, ()>(LocalMinute {
            date: "2026-03-29".into(),
            time: if at == 0 { "01:59" } else { "03:00" }.into(),
            weekday: 0,
        })
    })
    .unwrap();
    assert_eq!(page.items[0].key, "daily:Fixture:2026-03-29:02:30");
    assert_eq!(page.items[0].occurred_at, 60_000);
    let page = occurrences(schedule, 180_000, 0, 10, |at, _| {
        Ok::<_, ()>(LocalMinute {
            date: "2026-10-25".into(),
            time: ["02:29", "02:30", "02:29", "02:30"][(at / 60_000) as usize].into(),
            weekday: 0,
        })
    })
    .unwrap();
    // The policy emits the same durable identity; the scheduler reconciles it once.
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].key, page.items[1].key);
}

#[test]
fn policy_retains_priority_and_overnight_exclusive_end() {
    assert_eq!(
        ignored(true, "me", &["me".into()], Some(&[])),
        Some("outgoing")
    );
    assert_eq!(
        ignored(false, "other", &[], Some(&[])),
        Some("sender_not_allowed")
    );
    assert_eq!(ignored(false, "other", &[], None), None);
    assert_eq!(
        suppression(
            Notification::Voice,
            Disposition::Task,
            Urgency::Low,
            Urgency::Critical
        ),
        Some("low_urgency")
    );
    assert_eq!(
        suppression(
            Notification::Voice,
            Disposition::Task,
            Urgency::Normal,
            Urgency::Critical
        ),
        None
    );
    assert!(quiet("23:00", "22:00", "07:00", false, Urgency::Critical));
    assert!(!quiet("07:00", "22:00", "07:00", false, Urgency::Critical));
    assert!(!quiet("23:00", "22:00", "07:00", true, Urgency::Critical));
}
