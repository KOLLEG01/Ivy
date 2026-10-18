//! Exact phone/JID filtering before any inbound message is journaled.
//! The transport supplies its original PN/LID mapping result; this module does
//! not perform lookup, mutate auth state, or admit groups and own messages.

fn digits(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    ((7..=15).contains(&bytes.len())
        && matches!(bytes.first(), Some(b'1'..=b'9'))
        && bytes.iter().all(u8::is_ascii_digit))
    .then_some(value)
}

pub fn phone(value: &str) -> Option<&str> {
    digits(
        value
            .strip_prefix('+')
            .or_else(|| value.strip_prefix("00"))
            .unwrap_or(value),
    )
}

pub fn jid_phone(value: &str) -> Option<&str> {
    let local = value
        .strip_suffix("@s.whatsapp.net")
        .or_else(|| value.strip_suffix("@c.us"))?;
    if let Some((number, device)) = local.split_once(':') {
        if device.is_empty() || !device.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits(number)
    } else {
        digits(local)
    }
}

pub struct MessageKey<'a> {
    pub id: &'a str,
    pub remote_jid: &'a str,
    pub remote_jid_alt: &'a str,
    pub from_me: bool,
    pub participant: &'a str,
}

/// Mirrors the pre-lookup guard so the transport never resolves a LID for a
/// message the application would refuse without consulting protocol state.
pub fn eligible(key: &MessageKey<'_>) -> bool {
    !key.from_me
        && !key.id.is_empty()
        && key.participant.is_empty()
        && (key.remote_jid.ends_with("@s.whatsapp.net") || key.remote_jid.ends_with("@lid"))
}

pub fn needs_lid_mapping(key: &MessageKey<'_>) -> bool {
    eligible(key) && jid_phone(key.remote_jid).is_none() && key.remote_jid.ends_with("@lid")
}

/// Return the selected configured sender index, preserving first-match order.
/// Empty mapping means absent, as in the library's original optional result.
pub fn admitted_sender(
    key: &MessageKey<'_>,
    mapped_pn: Option<&str>,
    sender_phones: &[&str],
) -> Option<usize> {
    if !eligible(key) {
        return None;
    }
    let mut number = jid_phone(key.remote_jid);
    if number.is_none() && key.remote_jid.ends_with("@lid") {
        let mapped = mapped_pn.unwrap_or("");
        let mapped_number = jid_phone(mapped);
        let alternate = jid_phone(key.remote_jid_alt);
        number = mapped_number.or(alternate);
        if !mapped.is_empty() && alternate.is_some() && mapped_number != alternate {
            return None;
        }
    }
    let number = number?;
    sender_phones
        .iter()
        .position(|configured| phone(configured) == Some(number))
}
