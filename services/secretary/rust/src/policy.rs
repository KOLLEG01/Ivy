//! Inbox filtering, independent of global Hive dispatch-time contact restrictions.
use ivy_contracts::{IvyError, IvyJson, IvyResult};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Urgency {
    Low,
    Normal,
    High,
    Critical,
}
impl Urgency {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
    pub fn read(value: &IvyJson) -> Option<Self> {
        [Self::Low, Self::Normal, Self::High, Self::Critical]
            .into_iter()
            .find(|v| value.is_str(v.as_str()))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notification {
    NeedsAttention,
    None,
    Main,
    Voice,
}
impl Notification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsAttention => "needs_attention",
            Self::None => "none",
            Self::Main => "main",
            Self::Voice => "voice",
        }
    }
    pub fn read(value: &IvyJson) -> Option<Self> {
        [Self::NeedsAttention, Self::None, Self::Main, Self::Voice]
            .into_iter()
            .find(|v| value.is_str(v.as_str()))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Ignore,
    Record,
    Task,
}

pub fn ignored(
    outgoing: bool,
    sender: &str,
    own: &[String],
    allowed: Option<&[String]>,
) -> Option<&'static str> {
    if outgoing {
        Some("outgoing")
    } else if own.iter().any(|s| s == sender) {
        Some("own_sender")
    } else if allowed.is_some_and(|a| !a.iter().any(|s| s == sender)) {
        Some("sender_not_allowed")
    } else {
        None
    }
}
pub fn suppression(
    notification: Notification,
    disposition: Disposition,
    urgency: Urgency,
    minimum_main: Urgency,
) -> Option<&'static str> {
    match notification {
        Notification::NeedsAttention => Some("needs_attention"),
        Notification::None => Some(match disposition {
            Disposition::Ignore => "ignore",
            Disposition::Record => "record",
            Disposition::Task => "notification_none",
        }),
        _ if urgency == Urgency::Low => Some("low_urgency"),
        _ if notification != Notification::Voice && urgency < minimum_main => {
            Some("below_configured_urgency")
        }
        _ => None,
    }
}
/// Local policy only: global Hive quiet hours are applied by contact dispatch too.
pub fn quiet(
    local_time: &str,
    start: &str,
    end: &str,
    urgent_bypass: bool,
    urgency: Urgency,
) -> bool {
    if urgent_bypass && urgency == Urgency::Critical {
        return false;
    }
    if start < end {
        local_time >= start && local_time < end
    } else {
        local_time >= start || local_time < end
    }
}

fn urgency(value: &IvyJson) -> IvyResult<Urgency> {
    match Urgency::read(value) {
        Some(urgency) => Ok(urgency),
        None => {
            value.as_str()?;
            Err(IvyError::internal())
        }
    }
}
pub(crate) fn assessment_suppression(
    assessment: &IvyJson,
    policy: &IvyJson,
) -> IvyResult<Option<&'static str>> {
    let value = assessment.required("notification")?;
    let notification = match Notification::read(value) {
        Some(notification) => notification,
        None => {
            value.as_str()?;
            return Err(IvyError::internal());
        }
    };
    let disposition = match assessment.required("disposition")?.as_str()?.as_str() {
        "ignore" => Disposition::Ignore,
        "record" => Disposition::Record,
        // For non-none notifications only urgency matters. This enum's Task
        // represents all other admitted dispositions in that shared policy.
        _ => Disposition::Task,
    };
    Ok(suppression(
        notification,
        disposition,
        urgency(assessment.required("urgency")?)?,
        urgency(policy.required("minimumMainUrgency")?)?,
    ))
}
