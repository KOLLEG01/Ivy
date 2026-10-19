//! Original local Microsoft workflow rows, native receipt blobs and epoch decisions.
use crate::{
    journal::{Journal, Pin},
    microsoft_schema::{MicrosoftRecord, validate_microsoft},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::validate_agent,
    hash::{digest, hash_json},
    json::{canonical, parse},
};
use ivy_sdk::{
    native_evidence::{native_instant, native_operation_terminal, validate_native_progress},
    native_operation::{NativeOperationCall, NativeOperationOwner},
    native_owner::NativeOwner,
};
use std::{future::Future, pin::Pin as FuturePin};

const FRAME: usize = 32 * 1024 * 1024;
fn check(condition: bool) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(
            "microsoft_evidence_invalid",
            "Microsoft recovery data must retain its original identity and bytes.",
        ))
    }
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn local_text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn pin(value: &IvyJson) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: local_text(&text(value, "objectId")?),
        revision: value
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
pub fn pin_value(pin: &Pin) -> IvyJson {
    IvyJson::object([
        ("objectId", pin.object_id.clone().into()),
        ("revision", IvyJson::Number(pin.revision as f64)),
    ])
}
pub fn native_call(value: &IvyJson) -> IvyResult<NativeOperationCall> {
    Ok(NativeOperationCall {
        operation_id: text(value, "operationId")?,
        method: text(value, "method")?,
        params: value.required("params")?.clone(),
        definition_hash: text(value, "definitionHash")?,
    })
}
#[derive(Clone, Debug)]
pub struct MicrosoftDocument {
    pub pin: Pin,
    pub value: IvyJson,
}
impl MicrosoftDocument {
    pub fn value(&self) -> IvyJson {
        IvyJson::object([("pin", pin_value(&self.pin)), ("value", self.value.clone())])
    }
}
pub type MicrosoftGuardFuture<'a> = FuturePin<Box<dyn Future<Output = IvyResult<()>> + 'a>>;
pub trait MicrosoftGuard {
    fn check(&self) -> MicrosoftGuardFuture<'_>;
}

pub struct MicrosoftStore<'a> {
    pub journal: &'a Journal,
    pub guard: &'a dyn MicrosoftGuard,
}
impl MicrosoftStore<'_> {
    pub fn read(
        &self,
        key: MicrosoftRecord,
        selected: &IvyJson,
        immutable: bool,
        name: Option<&JsString>,
    ) -> IvyResult<MicrosoftDocument> {
        let (id, revision) = if let Some(id) = selected.as_js_string() {
            (local_text(id), None)
        } else {
            let pin = pin(selected)?;
            (pin.object_id, Some(pin.revision))
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
        check(name.is_none_or(|v| v.0.is_empty() || JsString::from(row.name.clone()) == *v))?;
        check(!immutable || row.pin.revision == 1)?;
        validate_microsoft(key.schema(), &row.value)?;
        Ok(MicrosoftDocument {
            pin: row.pin,
            value: row.value,
        })
    }
    pub fn find(
        &self,
        key: MicrosoftRecord,
        name: &JsString,
        immutable: bool,
    ) -> IvyResult<Option<MicrosoftDocument>> {
        self.journal
            .named(key.key(), &local_text(name), true)?
            .map(|row| self.read(key, &pin_value(&row.pin), immutable, Some(name)))
            .transpose()
    }
    pub async fn create(
        &self,
        key: MicrosoftRecord,
        name: &JsString,
        value: &IvyJson,
        immutable: bool,
    ) -> IvyResult<MicrosoftDocument> {
        validate_microsoft(key.schema(), value)?;
        self.guard.check().await?;
        let row = self
            .journal
            .create_technical(key.key(), &local_text(name), value)?;
        if immutable {
            check(same(&row.value, value)?)?;
        }
        Ok(MicrosoftDocument {
            pin: row.pin,
            value: row.value,
        })
    }
    pub async fn amend(
        &self,
        key: MicrosoftRecord,
        current: &MicrosoftDocument,
        value: &IvyJson,
    ) -> IvyResult<MicrosoftDocument> {
        validate_microsoft(key.schema(), value)?;
        if same(&current.value, value)? {
            return Ok(current.clone());
        }
        self.guard.check().await?;
        let row = self
            .journal
            .amend_technical(key.key(), &current.pin, value)?;
        Ok(MicrosoftDocument {
            pin: row.pin,
            value: row.value,
        })
    }
    pub fn check_call_history(
        &self,
        call: &MicrosoftDocument,
        owner: &dyn NativeOperationOwner,
    ) -> IvyResult<()> {
        let value = &call.value;
        let observation = value.required("observation")?;
        let seen = value.required("seen")?;
        let latest = if !observation.is_null() {
            observation
        } else {
            seen
        };
        if latest.is_null() {
            return Ok(());
        }
        let current = self.operation(value.required("plan")?, value.required("slot")?, latest)?;
        let original = native_call(value)?;
        owner.check_operation(&current, &original)?;
        check(
            native_instant(&text(&current, "createdAt")?, None)?
                >= native_instant(&text(value, "preparedAt")?, None)?
                && !observation.is_null() == native_operation_terminal(&current),
        )?;
        if !seen.is_null() && !observation.is_null() {
            let pending = self.operation(value.required("plan")?, value.required("slot")?, seen)?;
            owner.check_operation(&pending, &original)?;
            check(!native_operation_terminal(&pending))?;
            validate_native_progress(&pending, &current, Some("microsoft_evidence_invalid"))?;
        }
        Ok(())
    }
    pub async fn save_operation(
        &self,
        plan: &IvyJson,
        slot: &IvyJson,
        operation: &IvyJson,
    ) -> IvyResult<IvyJson> {
        validate_agent("Operation", operation)?;
        let bytes = canonical(operation, FRAME)?.into_bytes();
        let hash = digest(&bytes);
        self.guard.check().await?;
        let name = format!(
            "microsoft-operation-{}",
            &hash_json(&IvyJson::Array(vec![
                plan.clone(),
                slot.clone(),
                hash.clone().into()
            ]))?[7..]
        );
        let pin = self
            .journal
            .save_blob("secretary/microsoft-native", &name, &bytes)?;
        let value = IvyJson::object([
            ("byteLength", IvyJson::Number(bytes.len() as f64)),
            ("contentHash", hash.into()),
            ("chunks", IvyJson::Array(vec![pin_value(&pin)])),
        ]);
        validate_microsoft("MicrosoftBlob", &value)?;
        Ok(value)
    }
    pub fn operation(
        &self,
        _plan: &IvyJson,
        _slot: &IvyJson,
        value: &IvyJson,
    ) -> IvyResult<IvyJson> {
        // Resolve the retained blob pin exactly as the original reader does.
        // Call ownership and progress are checked by check_call_history.
        validate_microsoft("MicrosoftBlob", value)?;
        let chunks = value
            .required("chunks")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        check(chunks.len() == 1)?;
        let saved = self
            .journal
            .blob("secretary/microsoft-native", "", Some(&pin(&chunks[0])?))?;
        check(saved.is_some())?;
        let (_, bytes) = saved.unwrap();
        check(
            value.get("byteLength") == Some(&IvyJson::Number(bytes.len() as f64))
                && value.get("contentHash") == Some(&digest(&bytes).into()),
        )?;
        let text = std::str::from_utf8(&bytes).map_err(|_| IvyError::internal())?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let operation = parse(text, FRAME).map_err(|_| IvyError::internal())?;
        check(canonical(&operation, FRAME)? == text)?;
        validate_agent("Operation", &operation)?;
        Ok(operation)
    }
}

/// A current local absence decision, never a Hive Object or dispatch authority.
pub struct MicrosoftEpochBoundaries<'a> {
    pub journal: &'a Journal,
    pub owner: &'a NativeOwner,
    pub namespace: &'a str,
}
impl MicrosoftEpochBoundaries<'_> {
    fn call_hash(call: &IvyJson) -> IvyResult<String> {
        hash_json(&IvyJson::object(
            ["operationId", "method", "params", "definitionHash"]
                .map(|key| Ok((key, call.required(key)?.clone())))
                .into_iter()
                .collect::<IvyResult<Vec<_>>>()?,
        ))
    }
    fn name(plan: &IvyJson, call: &IvyJson) -> IvyResult<String> {
        Ok(format!(
            "epoch-{}",
            &hash_json(&IvyJson::Array(vec![
                plan.clone(),
                call.required("operationId")?.clone(),
                call.required("method")?.clone(),
                call.required("params")?.clone(),
                call.required("definitionHash")?.clone()
            ]))?[7..]
        ))
    }
    pub fn read(
        &self,
        plan: &IvyJson,
        call: &IvyJson,
        previous_epoch: &IvyJson,
    ) -> IvyResult<Option<IvyJson>> {
        let key = format!("{}/epoch-boundary", self.namespace);
        let Some(saved) = self.journal.named(&key, &Self::name(plan, call)?, true)? else {
            return Ok(None);
        };
        let value = saved.value;
        check(
            value.get("schemaVersion") == Some(&IvyJson::Number(1.0))
                && same(value.required("plan")?, plan)?
                && value.get("previousEpoch") == Some(previous_epoch)
                && value.get("callHash") == Some(&Self::call_hash(call)?.into())
                && value.required("absence")?.get("operationId") == call.get("operationId")
                && value.required("status")?.get("epoch")
                    == value.required("absence")?.get("epoch")
                && value.required("status")?.get("epoch") != Some(previous_epoch),
        )?;
        Ok(Some(value))
    }
    pub async fn record(
        &self,
        plan: &IvyJson,
        call: &IvyJson,
        previous_epoch: &IvyJson,
        absence: &IvyJson,
        guard: &dyn MicrosoftGuard,
    ) -> IvyResult<()> {
        guard.check().await?;
        let status = self.owner.status().await?;
        let value = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("plan", plan.clone()),
            ("callHash", Self::call_hash(call)?.into()),
            ("previousEpoch", previous_epoch.clone()),
            ("absence", absence.clone()),
            ("status", status),
        ]);
        check(
            absence.get("operationId") == call.get("operationId")
                && value.required("status")?.get("epoch") == absence.get("epoch")
                && value.required("status")?.get("epoch") != Some(previous_epoch),
        )?;
        let key = format!("{}/epoch-boundary", self.namespace);
        let name = Self::name(plan, call)?;
        if let Some(prior) = self.journal.named(&key, &name, true)? {
            check(same(&prior.value, &value)?)?;
            return Ok(());
        }
        self.journal.create_technical(&key, &name, &value)?;
        Ok(())
    }
}
