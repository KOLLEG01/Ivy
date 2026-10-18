//! Secretary's UTC-minute policy. The caller supplies shared IANA local parts.
const MINUTE: i64 = 60_000;
const WINDOW: i64 = 7 * 24 * 60 * MINUTE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMinute {
    pub date: String,
    pub time: String,
    pub weekday: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule<'a> {
    Other,
    Interval {
        minutes: u32,
    },
    Civil {
        weekly: bool,
        zone: &'a str,
        time: &'a str,
        weekdays: &'a [u8],
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    Interval {
        slot: i64,
    },
    Civil {
        weekly: bool,
        date: String,
        time: String,
        zone: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub key: String,
    pub occurred_at: i64,
    pub payload: Payload,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub items: Vec<Occurrence>,
    pub checked_at: i64,
}

pub fn occurrences<E>(
    schedule: Schedule<'_>,
    now: i64,
    since: i64,
    records_per_tick: u32,
    mut local: impl FnMut(i64, &str) -> Result<LocalMinute, E>,
) -> Result<Page, E> {
    if schedule == Schedule::Other {
        return Ok(Page {
            items: vec![],
            checked_at: now.max(since),
        });
    }
    let end = now.min(since.saturating_add(WINDOW));
    let mut page = Page {
        items: vec![],
        checked_at: since,
    };
    if since >= end {
        return Ok(page);
    }
    match schedule {
        Schedule::Other => unreachable!(),
        Schedule::Interval { minutes } => {
            // Admission validates positive minutes and records_per_tick.
            assert!(minutes > 0 && records_per_tick > 0);
            let interval = i64::from(minutes) * MINUTE;
            let first = since.div_euclid(interval) + 1;
            let last = end.div_euclid(interval);
            let through = last.min(first + i64::from(records_per_tick) - 1);
            for slot in first..=through {
                page.items.push(Occurrence {
                    key: format!("interval:{minutes}:{slot}"),
                    occurred_at: slot * interval,
                    payload: Payload::Interval { slot },
                });
            }
            page.checked_at = if through < last {
                through * interval
            } else {
                end
            };
        }
        Schedule::Civil {
            weekly,
            zone,
            time,
            weekdays,
        } => {
            let mut previous = local(since.div_euclid(MINUTE) * MINUTE, zone)?;
            let mut at = (since.div_euclid(MINUTE) + 1) * MINUTE;
            while at <= end {
                let current = local(at, zone)?;
                if (!weekly || weekdays.contains(&current.weekday))
                    && current.time.as_str() >= time
                    && (current.date != previous.date || previous.time.as_str() < time)
                {
                    page.items.push(Occurrence {
                        key: format!(
                            "{}:{zone}:{}:{time}",
                            if weekly { "weekly" } else { "daily" },
                            current.date
                        ),
                        occurred_at: at,
                        payload: Payload::Civil {
                            weekly,
                            date: current.date.clone(),
                            time: time.into(),
                            zone: zone.into(),
                        },
                    });
                }
                previous = current;
                at += MINUTE;
            }
            page.checked_at = end;
        }
    }
    Ok(page)
}
