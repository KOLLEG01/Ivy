//! The original local triage graph and immutable native evidence blobs.
use crate::{
    journal::{Journal, Pin},
    triage_schema::{self, TriageSchema},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse},
};

const NATIVE_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy)]
pub enum TriageRecord {
    Plan,
    Thread,
    Work,
    Call,
    Completion,
}
impl TriageRecord {
    pub fn key(self) -> &'static str {
        match self {
            Self::Plan => "secretary/triage-plan",
            Self::Thread => "secretary/triage-thread",
            Self::Work => "secretary/triage-work",
            Self::Call => "secretary/triage-call",
            Self::Completion => "secretary/triage-completion",
        }
    }
    fn schema(self) -> TriageSchema {
        match self {
            Self::Plan => TriageSchema::Plan,
            Self::Thread => TriageSchema::Thread,
            Self::Work => TriageSchema::Work,
            Self::Call => TriageSchema::Call,
            Self::Completion => TriageSchema::Completion,
        }
    }
}
#[derive(Clone, Debug)]
pub struct TriageDocument {
    pub pin: Pin,
    pub value: IvyJson,
}
impl TriageDocument {
    pub fn value(&self) -> IvyJson {
        IvyJson::object([("pin", pin_value(&self.pin)), ("value", self.value.clone())])
    }
}
pub(crate) use crate::microsoft_store::pin_value;
pub fn triage_name(kind: &str, value: &IvyJson) -> IvyResult<JsString> {
    Ok(format!("triage-{kind}-{}", &hash_json(value)?[7..]).into())
}
pub(crate) fn check(value: bool, code: &str) -> IvyResult<()> {
    triage_schema::check(value, code)
}
pub(crate) fn evidence(value: bool) -> IvyResult<()> {
    check(value, "secretary_triage_evidence_invalid")
}
pub(crate) fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
pub(crate) fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
pub(crate) fn pin(value: &IvyJson) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: text(value, "objectId")?.to_utf8()?,
        revision: value
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
pub(crate) fn record(value: Option<&IvyJson>) -> IvyJson {
    value
        .filter(|v| v.as_object().is_some())
        .cloned()
        .unwrap_or_else(|| IvyJson::object(Vec::<(String, IvyJson)>::new()))
}
pub struct TriageStore<'a> {
    pub journal: &'a Journal,
}
impl TriageStore<'_> {
    pub fn named(&self, key: &str, name: &JsString) -> IvyResult<Option<Pin>> {
        Ok(self
            .journal
            .named(key, &name.to_utf8()?, true)?
            .map(|d| d.pin))
    }
    pub fn read(
        &self,
        key: TriageRecord,
        selected: &IvyJson,
        name: Option<&JsString>,
        immutable: bool,
    ) -> IvyResult<TriageDocument> {
        let (id, revision) = if let Some(id) = selected.as_js_string() {
            (id.to_utf8()?, None)
        } else {
            let p = pin(selected)?;
            (p.object_id, Some(p.revision))
        };
        let row = self
            .journal
            .read(key.key(), &id, revision, true)
            .map_err(|mut e| {
                if e.code == "not_found" {
                    e.message = if e.message.contains("revision") {
                        "The local workflow revision does not exist."
                    } else {
                        "The local workflow record does not exist."
                    }
                    .into();
                }
                e
            })?;
        evidence(name.is_none_or(|name| name.0.is_empty() || JsString::from(row.name) == *name))?;
        evidence(!immutable || row.pin.revision == 1)?;
        triage_schema::validate(key.schema(), &row.value)?;
        Ok(TriageDocument {
            pin: row.pin,
            value: row.value,
        })
    }
    pub fn find(
        &self,
        key: TriageRecord,
        name: &JsString,
        immutable: bool,
    ) -> IvyResult<Option<TriageDocument>> {
        self.named(key.key(), name)?
            .map(|p| self.read(key, &pin_value(&p), Some(name), immutable))
            .transpose()
    }
    pub fn create(
        &self,
        key: TriageRecord,
        name: &JsString,
        value: &IvyJson,
        immutable: bool,
    ) -> IvyResult<TriageDocument> {
        triage_schema::validate(key.schema(), value)?;
        let row = self
            .journal
            .create_technical(key.key(), &name.to_utf8()?, value)?;
        if immutable {
            evidence(same(&row.value, value)?)?;
        }
        Ok(TriageDocument {
            pin: row.pin,
            value: row.value,
        })
    }
    pub fn amend(
        &self,
        key: TriageRecord,
        current: &TriageDocument,
        value: &IvyJson,
    ) -> IvyResult<TriageDocument> {
        triage_schema::validate(key.schema(), value)?;
        if same(&current.value, value)? {
            return Ok(current.clone());
        }
        let row = self
            .journal
            .amend_technical(key.key(), &current.pin, value)?;
        Ok(TriageDocument {
            pin: row.pin,
            value: row.value,
        })
    }
    pub fn native_json(&self, kind: &str, selected: &IvyJson) -> IvyResult<IvyJson> {
        let saved = self.journal.blob(
            &format!("secretary/triage-native-{kind}"),
            "",
            Some(&pin(selected)?),
        )?;
        evidence(
            saved
                .as_ref()
                .is_some_and(|(_, b)| !b.is_empty() && b.len() <= NATIVE_BYTES),
        )?;
        let bytes = saved.unwrap().1;
        // TextDecoder's default BOM handling is part of the retained JS reader.
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
        std::str::from_utf8(bytes).ok().and_then(|s| parse(s, NATIVE_BYTES).ok()).ok_or_else(|| IvyError::new("secretary_triage_evidence_invalid", "Triage requires its original item, owner, native intent and complete evidence."))
    }
    pub fn save_native_json(
        &self,
        kind: &str,
        identity: &IvyJson,
        value: &IvyJson,
    ) -> IvyResult<Pin> {
        let bytes = canonical(value, NATIVE_BYTES)?.into_bytes();
        if kind == "operation" {
            evidence(
                value.as_object().is_some()
                    && value
                        .get("operationId")
                        .and_then(IvyJson::as_js_string)
                        .is_some(),
            )?;
        }
        let name = triage_name(
            &format!("native-{kind}"),
            &IvyJson::Array(vec![identity.clone(), digest(&bytes).into()]),
        )?;
        self.journal.save_blob(
            &format!("secretary/triage-native-{kind}"),
            &name.to_utf8()?,
            &bytes,
        )
    }
    pub fn binary(
        &self,
        name: &JsString,
        selected: Option<&Pin>,
        maximum_bytes: f64,
    ) -> IvyResult<Option<(Pin, Vec<u8>)>> {
        evidence(
            maximum_bytes.fract() == 0.0
                && maximum_bytes > 0.0
                && maximum_bytes <= NATIVE_BYTES as f64,
        )?;
        let saved = self
            .journal
            .blob("secretary/triage-read", &name.to_utf8()?, selected)?;
        if let Some((_, bytes)) = &saved {
            evidence(!bytes.is_empty() && bytes.len() as f64 <= maximum_bytes)?;
        }
        Ok(saved)
    }
    pub fn save_binary(&self, name: &JsString, bytes: &[u8], maximum_bytes: f64) -> IvyResult<Pin> {
        evidence(
            maximum_bytes.fract() == 0.0
                && maximum_bytes > 0.0
                && maximum_bytes <= NATIVE_BYTES as f64
                && !bytes.is_empty()
                && bytes.len() as f64 <= maximum_bytes,
        )?;
        self.journal
            .save_blob("secretary/triage-read", &name.to_utf8()?, bytes)
    }
}
