//! Pure configured target and incoming-source policy. Native observations are
//! supplied by the owner; policy never performs a call or releases a reservation.
use crate::contracts::validate_admission;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, hash::hash_json, wire_schema::validate_shared,
};
use std::{
    collections::BTreeSet,
    net::{Ipv4Addr, Ipv6Addr},
};

pub const CALL_RESERVATION: usize = 64 * 1024;
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a IvyJson> {
    v.get(k).ok_or_else(IvyError::internal)
}
fn text<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a JsString> {
    field(v, k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn array<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a [IvyJson]> {
    field(v, k)?.as_array().ok_or_else(IvyError::internal)
}
fn optional_text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<Option<&'a JsString>> {
    match value.get(key) {
        None | Some(IvyJson::Null) => Ok(None),
        Some(IvyJson::String(text)) => Ok(Some(text)),
        _ => Err(IvyError::new(
            "invalid_arguments",
            "Expected a Phone text field.",
        )),
    }
}
fn is_e164(s: &[u8]) -> bool {
    s.len() >= 8
        && s.len() <= 16
        && s[0] == 43
        && (49..=57).contains(&s[1])
        && s[1..].iter().all(|c| (48..=57).contains(c))
}
fn peer_address(s: &JsString) -> bool {
    let Ok(s) = s.to_utf8() else { return false };
    if s.parse::<Ipv4Addr>().is_ok() {
        return true;
    }
    let address = if let Some((address, zone)) = s.split_once('%') {
        if zone.is_empty()
            || !zone
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-.:".contains(&c))
        {
            return false;
        }
        address
    } else {
        &s
    };
    address.parse::<Ipv6Addr>().is_ok()
}
/// Match numbered SIP/TEL identities across hosts and URI parameters.
pub fn phone_caller_identity(uri: &JsString) -> JsString {
    let Ok(text) = uri.to_utf8() else {
        return uri.clone();
    };
    let Some((scheme, tail)) = text.split_once(':') else {
        return uri.clone();
    };
    if !["sip", "sips", "tel"]
        .iter()
        .any(|name| scheme.eq_ignore_ascii_case(name))
    {
        return uri.clone();
    }
    let number = tail.split(['@', ';']).next().unwrap_or_default();
    if is_e164(number.as_bytes()) {
        number.into()
    } else {
        uri.clone()
    }
}
fn peer(rule: &IvyJson, context: &IvyJson) -> bool {
    if rule.get("peerAddress") == Some(&"registered".into()) {
        context.get("registrationPeer") == Some(&true.into())
    } else {
        rule.get("peerAddress") == context.get("peerAddress")
    }
}
fn same(expected: &JsString, actual: &JsString) -> bool {
    phone_caller_identity(expected) == phone_caller_identity(actual)
}
#[derive(Clone)]
pub struct PhoneAdmission {
    definition: IvyJson,
    hash: String,
}
impl PhoneAdmission {
    pub fn new(definition: IvyJson) -> IvyResult<Self> {
        validate_admission("PhonePolicy", &definition)?;
        let mut ids = BTreeSet::new();
        for recipient in array(&definition, "recipients")? {
            let id = field(recipient, "id")?;
            validate_shared("Identifier", id)?;
            require(
                ids.insert(text(recipient, "id")?.clone()),
                "invalid_arguments",
                "Phone recipient IDs must be unique.",
            )?;
        }
        let mut rules = array(&definition, "incoming")?.to_vec();
        rules.extend(
            definition
                .get("challengedIncoming")
                .and_then(IvyJson::as_array)
                .unwrap_or(&[])
                .to_vec(),
        );
        require(
            rules.iter().all(|r| {
                r.get("peerAddress") == Some(&"registered".into())
                    || r.get("peerAddress")
                        .and_then(IvyJson::as_js_string)
                        .is_some_and(peer_address)
            }),
            "invalid_arguments",
            "Phone incoming policy needs an IP address or the registered SIP peer.",
        )?;
        let mut incoming = BTreeSet::new();
        for rule in array(&definition, "incoming")? {
            require(
                incoming.insert(hash_json(&IvyJson::object([
                    ("peerAddress", field(rule, "peerAddress")?.clone()),
                    ("transport", field(rule, "transport")?.clone()),
                    ("fromUri", field(rule, "fromUri")?.clone()),
                ]))?),
                "invalid_arguments",
                "Phone incoming rules must be unique.",
            )?;
        }
        let hash = hash_json(&definition)?;
        Ok(Self { definition, hash })
    }
    pub fn definition(&self) -> &IvyJson {
        &self.definition
    }
    pub fn hash(&self) -> &str {
        &self.hash
    }
    fn recipient(&self, id: &IvyJson) -> IvyResult<Option<&IvyJson>> {
        Ok(array(&self.definition, "recipients")?
            .iter()
            .find(|v| v.get("id") == Some(id)))
    }
    fn resolve_destination(&self, value: &JsString) -> IvyResult<JsString> {
        let destination = value.to_utf8()?;
        if is_e164(destination.as_bytes()) {
            let domain = self
                .definition
                .get("dialDomain")
                .and_then(IvyJson::as_js_string)
                .and_then(|v| v.to_utf8().ok());
            let valid = domain.as_ref().is_some_and(|d| {
                let (host, port) = d
                    .split_once(':')
                    .map_or((d.as_str(), None), |(h, p)| (h, Some(p)));
                !host.is_empty()
                    && host
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
                    && port.is_none_or(|p| {
                        !p.is_empty() && p.len() <= 5 && p.bytes().all(|c| c.is_ascii_digit())
                    })
            });
            require(
                valid,
                "invalid_arguments",
                "An E.164 destination requires a configured SIP dial domain.",
            )?;
            return Ok(format!("sip:{destination}@{}", domain.unwrap()).into());
        }
        let target = destination
            .strip_prefix("sips:")
            .or_else(|| destination.strip_prefix("sip:"));
        require(
            value.0.len() <= 2048
                && target.is_some_and(|target| {
                    !target.is_empty()
                        && !target.chars().any(|c| c.is_whitespace() || c.is_control())
                }),
            "invalid_arguments",
            "Expected E.164 or SIP destination.",
        )?;
        Ok(value.clone())
    }
    /// Positional source arguments are represented by their original field names;
    /// absent optional arguments stay absent when the admission is materialized.
    pub fn outgoing(&self, args: &IvyJson) -> IvyResult<IvyJson> {
        let no_recipient = IvyJson::Null;
        let recipient_id = args.get("recipientId").unwrap_or(&no_recipient);
        let recipient = self.recipient(recipient_id)?;
        let destination = optional_text(args, "destination")?;
        let route = optional_text(args, "route")?;
        let prompt = optional_text(args, "voicePrompt")?;
        let screening = match args.get("screening") {
            None | Some(IvyJson::Null) => None,
            Some(value @ IvyJson::Object(_)) => Some(value),
            _ => {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "Expected Phone screening settings.",
                ));
            }
        };
        let direct = recipient_id.is_null()
            && self.definition.get("allowDirectDial") == Some(&true.into())
            && screening.is_none()
            && route == Some(&JsString::from("windows"));
        require(
            recipient.is_some() && args.get("destination").is_none()
                || direct && destination.is_some_and(|value| !value.0.is_empty()),
            "phone_destination_refused",
            "The Phone destination or route is not configured.",
        )?;
        require(
            prompt.is_none()
                || route == Some(&JsString::from("voice"))
                    && screening.is_none()
                    && recipient.is_some(),
            "phone_caller_refused",
            "A Voice prompt requires an admitted configured Voice recipient.",
        )?;
        if let Some(prompt) = prompt {
            require(
                !prompt.0.is_empty() && prompt.0.len() <= 30000 && prompt.to_utf8()?.len() <= 32000,
                "invalid_arguments",
                "A Voice prompt must fit the native session startup text bound.",
            )?;
        }
        let destination = recipient
            .and_then(|r| r.get("destination"))
            .or_else(|| args.get("destination"))
            .and_then(IvyJson::as_js_string)
            .ok_or_else(IvyError::internal)?;
        let mut admission = IvyJson::object([
            ("epoch", args.get("epoch").cloned().unwrap_or(IvyJson::Null)),
            (
                "operationId",
                args.get("operationId").cloned().unwrap_or(IvyJson::Null),
            ),
            (
                "principalId",
                args.get("principalId").cloned().unwrap_or(IvyJson::Null),
            ),
            ("policyHash", self.hash.clone().into()),
            ("direction", "outgoing".into()),
            ("callId", IvyJson::Null),
            ("recipientId", recipient_id.clone()),
            ("destination", self.resolve_destination(destination)?.into()),
            ("incoming", IvyJson::Null),
        ]);
        if let Some(route) = route {
            admission.insert("route", route.clone().into());
        }
        if let Some(screening) = screening {
            admission.insert("screening", screening.clone());
        }
        if let Some(prompt) = prompt {
            admission.insert("voicePrompt", prompt.clone().into());
        }
        validate_admission("PhoneCallAdmission", &admission)?;
        Ok(admission)
    }
    pub fn matches_recipient(&self, call: &IvyJson, recipient_id: &JsString) -> IvyResult<bool> {
        let Some(recipient) = self.recipient(&recipient_id.clone().into())? else {
            return Ok(false);
        };
        if call.get("direction") == Some(&"outgoing".into()) {
            return Ok(call.get("recipientId") == Some(&recipient_id.clone().into()));
        }
        Ok(same(
            &self.resolve_destination(text(recipient, "destination")?)?,
            text(field(call, "incoming")?, "fromUri")?,
        ))
    }
    fn allows_incoming(&self, context: &IvyJson) -> IvyResult<bool> {
        for rule in array(&self.definition, "incoming")? {
            if peer(rule, context)
                && rule.get("transport") == context.get("transport")
                && same(text(rule, "fromUri")?, text(context, "fromUri")?)
                && rule.get("expectedAssertedNumber").is_none_or(|n| {
                    context
                        .get("assertedNumbers")
                        .and_then(IvyJson::as_array)
                        .is_some_and(|a| a.contains(n))
                })
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn challenges_incoming(&self, context: &IvyJson) -> IvyResult<bool> {
        Ok(!self.allows_incoming(context)?
            && self
                .definition
                .get("challengedIncoming")
                .and_then(IvyJson::as_array)
                .unwrap_or(&[])
                .iter()
                .any(|r| peer(r, context) && r.get("transport") == context.get("transport")))
    }
    pub fn incoming(
        &self,
        epoch: IvyJson,
        operation_id: IvyJson,
        principal_id: IvyJson,
        observation: &IvyJson,
    ) -> IvyResult<IvyJson> {
        validate_admission("SipCallObservation", observation)?;
        let context = field(observation, "incoming")?;
        require(
            observation.get("direction") == Some(&"incoming".into())
                && observation.get("state") == Some(&"ringing".into())
                && !context.is_null()
                && context.get("sipCallId") == observation.get("sipCallId")
                && (self.allows_incoming(context)? || self.challenges_incoming(context)?),
            "phone_caller_refused",
            "This original incoming call is not admitted by the Phone policy.",
        )?;
        let admission = IvyJson::object([
            ("epoch", epoch),
            ("operationId", operation_id),
            ("principalId", principal_id),
            ("policyHash", self.hash.clone().into()),
            ("direction", "incoming".into()),
            ("callId", field(observation, "id")?.clone()),
            ("recipientId", IvyJson::Null),
            ("destination", IvyJson::Null),
            ("incoming", context.clone()),
        ]);
        validate_admission("PhoneCallAdmission", &admission)?;
        Ok(admission)
    }
    pub fn authorize(
        &self,
        _principal_id: &JsString,
        call: &IvyJson,
        action: &str,
    ) -> IvyResult<()> {
        validate_admission("PhoneCall", call)?;
        if action != "connect" {
            return Ok(());
        }
        require(
            call.get("policyHash") == Some(&self.hash.clone().into()),
            "phone_policy_changed",
            "Original call policy changed; cleanup remains available.",
        )?;
        if call.get("direction") == Some(&"outgoing".into()) {
            let mut args = call.clone();
            if !call.get("recipientId").is_some_and(IvyJson::is_null) {
                if let IvyJson::Object(items) = &mut args {
                    items.retain(|(key, _)| key != &JsString::from("destination"));
                }
            }
            let current = self.outgoing(&args)?;
            require(
                current.get("destination") == call.get("destination"),
                "phone_policy_changed",
                "Original recipient destination changed.",
            )
        } else {
            let context = field(call, "incoming")?;
            require(
                self.allows_incoming(context)? || self.challenges_incoming(context)?,
                "phone_caller_refused",
                "Original incoming caller policy is no longer admitted.",
            )
        }
    }
}
#[cfg(test)]
#[path = "../../../../tests/rust/phone_admission.rs"]
mod tests;
