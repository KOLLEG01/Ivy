//! Fixed Phone contract graphs shared by service policy, native client and journal.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
    schema::{SchemaValidators, bundle_local_schema},
};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};
pub(crate) const OUTCOME_BYTES: usize = 65_536 + 2048;
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
struct Contracts {
    document: IvyJson,
    schemas: Mutex<BTreeMap<String, IvyJson>>,
    validators: SchemaValidators,
}
impl Contracts {
    fn schema(&self, name: &str) -> IvyResult<IvyJson> {
        require(
            self.document
                .get("$defs")
                .and_then(|d| d.get(name))
                .is_some(),
            "internal_error",
            "Unknown Phone contract.",
        )?;
        let mut schema = self.document.clone();
        schema.insert("$ref", format!("#/$defs/{name}").into());
        Ok(schema)
    }
    fn new(texts: &[&str], native: bool) -> IvyResult<Self> {
        let mut definitions = IvyJson::object([] as [(&str, IvyJson); 0]);
        for text in texts {
            let document = parse(text, MANAGEMENT_FRAME_BYTES)?;
            for (name, value) in field(&document, "$defs")?
                .as_object()
                .ok_or_else(IvyError::internal)?
            {
                definitions.insert(name.clone(), value.clone());
            }
        }
        if native {
            // native.ts rewrites these file references after its ordered merge.
            let mut encoded = stringify(&definitions, MANAGEMENT_FRAME_BYTES)?;
            for file in [
                "phone-runtime",
                "phone-launch",
                "phone-hotkey",
                "phone-desktop-controls",
                "phone-micro",
                "phone-voice-lifetime",
            ] {
                encoded = encoded.replace(&format!("{file}.schema.json#/$defs/"), "#/$defs/");
            }
            definitions = parse(&encoded, MANAGEMENT_FRAME_BYTES)?;
        }
        Self::from_definitions(definitions)
    }
    fn from_definitions(definitions: IvyJson) -> IvyResult<Self> {
        Ok(Self {
            document: IvyJson::object([("$defs", definitions)]),
            schemas: Mutex::new(BTreeMap::new()),
            validators: SchemaValidators::new(),
        })
    }
    fn validate(&self, name: &str, value: &IvyJson, bound: usize) -> IvyResult<()> {
        let contracts = self;
        let schema = {
            let mut schemas = contracts.schemas.lock().unwrap();
            if !schemas.contains_key(name) {
                require(
                    contracts
                        .document
                        .get("$defs")
                        .and_then(|defs| defs.get(name))
                        .is_some(),
                    "internal_error",
                    "Unknown Phone contract.",
                )?;
                schemas.insert(
                    name.into(),
                    bundle_local_schema(&contracts.document, &name.into())?,
                );
            }
            schemas[name].clone()
        };
        contracts.validators.validate(&schema, value, bound)
    }
}
pub(crate) fn validate(name: &str, value: &IvyJson) -> IvyResult<()> {
    static CONTRACTS: OnceLock<IvyResult<Contracts>> = OnceLock::new();
    CONTRACTS
        .get_or_init(|| {
            Contracts::new(
                &[
                    include_str!("../../../../specs/schemas/codex-app-tools.schema.json"),
                    include_str!("../../../../specs/schemas/phone-voice-archive.schema.json"),
                    include_str!("../../../../specs/schemas/phone-journal.schema.json"),
                ],
                false,
            )
        })
        .as_ref()
        .map_err(Clone::clone)?
        .validate(name, value, OUTCOME_BYTES)
}
pub(crate) fn validate_admission(name: &str, value: &IvyJson) -> IvyResult<()> {
    static CONTRACTS: OnceLock<IvyResult<Contracts>> = OnceLock::new();
    CONTRACTS
        .get_or_init(|| {
            let runtime = parse(
                include_str!("../../../../specs/schemas/phone-runtime.schema.json"),
                MANAGEMENT_FRAME_BYTES,
            )?;
            let journal = parse(
                include_str!("../../../../specs/schemas/phone-journal.schema.json"),
                MANAGEMENT_FRAME_BYTES,
            )?;
            let admission = parse(
                include_str!("../../../../specs/schemas/phone-admission.schema.json"),
                MANAGEMENT_FRAME_BYTES,
            )?;
            let mut definitions = field(&runtime, "$defs")?.clone();
            definitions.insert("Uuid", field(field(&journal, "$defs")?, "Uuid")?.clone());
            for (name, value) in field(&admission, "$defs")?
                .as_object()
                .ok_or_else(IvyError::internal)?
            {
                definitions.insert(name.clone(), value.clone());
            }
            Contracts::from_definitions(definitions)
        })
        .as_ref()
        .map_err(Clone::clone)?
        .validate(
            name,
            value,
            if name == "PhonePolicy" {
                256 * 1024
            } else {
                65_536
            },
        )
}
fn native() -> IvyResult<&'static Contracts> {
    static CONTRACTS: OnceLock<IvyResult<Contracts>> = OnceLock::new();
    CONTRACTS
        .get_or_init(|| {
            Contracts::new(
                &[
                    include_str!("../../../../specs/schemas/codex-app-tools.schema.json"),
                    include_str!("../../../../specs/schemas/phone-voice-archive.schema.json"),
                    include_str!("../../../../specs/schemas/phone-launch.schema.json"),
                    include_str!("../../../../specs/schemas/phone-hotkey.schema.json"),
                    include_str!("../../../../specs/schemas/phone-micro.schema.json"),
                    include_str!("../../../../specs/schemas/phone-capture.schema.json"),
                    include_str!("../../../../specs/schemas/phone-desktop-controls.schema.json"),
                    include_str!("../../../../specs/schemas/phone-voice-lifetime.schema.json"),
                    include_str!("../../../../specs/schemas/phone-runtime.schema.json"),
                    include_str!("../../../../specs/schemas/phone-ipc.schema.json"),
                ],
                true,
            )
        })
        .as_ref()
        .map_err(Clone::clone)
}
pub(crate) fn validate_native(name: &str, value: &IvyJson) -> IvyResult<()> {
    native()?.validate(name, value, 65_536)
}
pub fn phone_schema(name: &str) -> IvyResult<IvyJson> {
    native()?.schema(name)
}

fn service() -> IvyResult<&'static Contracts> {
    static CONTRACTS: OnceLock<IvyResult<Contracts>> = OnceLock::new();
    CONTRACTS
        .get_or_init(|| {
            let mut definitions = field(&native()?.document, "$defs")?.clone();
            for source in [
                include_str!("../../../../specs/schemas/codex-app-tools.schema.json"),
                include_str!("../../../../specs/schemas/phone-voice-archive.schema.json"),
                include_str!("../../../../specs/schemas/phone-journal.schema.json"),
                include_str!("../../../../specs/schemas/phone-admission.schema.json"),
                include_str!("../../../../specs/schemas/phone-service.schema.json"),
            ] {
                let document = parse(source, MANAGEMENT_FRAME_BYTES)?;
                for (name, value) in field(&document, "$defs")?
                    .as_object()
                    .ok_or_else(IvyError::internal)?
                {
                    definitions.insert(name.clone(), value.clone());
                }
            }
            Contracts::from_definitions(definitions)
        })
        .as_ref()
        .map_err(Clone::clone)
}
fn service_references(
    value: &IvyJson,
    definitions: &IvyJson,
    selected: &mut IvyJson,
) -> IvyResult<()> {
    match value {
        IvyJson::Array(items) => {
            for item in items {
                service_references(item, definitions, selected)?;
            }
        }
        IvyJson::Object(items) => {
            for (key, item) in items {
                if key == &"$ref".into() {
                    let target = item.as_js_string().and_then(|s| s.to_utf8().ok());
                    let target = target.as_deref().and_then(|s| s.strip_prefix("#/$defs/"));
                    require(
                        target.is_some(),
                        "internal_error",
                        "Phone contracts require local references.",
                    )?;
                    let target = target.unwrap();
                    let definition = definitions.get(target);
                    require(
                        definition.is_some(),
                        "internal_error",
                        "Phone contract reference is missing.",
                    )?;
                    if selected.get(target).is_none() {
                        selected.insert(target, definition.unwrap().clone());
                        service_references(definition.unwrap(), definitions, selected)?;
                    }
                } else {
                    service_references(item, definitions, selected)?;
                }
            }
        }
        _ => (),
    }
    Ok(())
}
/// Public service contracts inline their original root and only its reachable
/// existing definitions. Keep that published shape distinct from native frames.
pub fn phone_service_schema(name: &str) -> IvyResult<IvyJson> {
    let contracts = service()?;
    let definitions = field(&contracts.document, "$defs")?;
    require(
        definitions.get(name).is_some(),
        "internal_error",
        "Unknown Phone service contract.",
    )?;
    let mut schemas = contracts.schemas.lock().unwrap();
    if let Some(schema) = schemas.get(name) {
        return Ok(schema.clone());
    }
    let mut schema = definitions.get(name).unwrap().clone();
    let mut selected = IvyJson::object([] as [(&str, IvyJson); 0]);
    service_references(&schema, definitions, &mut selected)?;
    schema.insert("$defs", selected);
    schemas.insert(name.into(), schema.clone());
    Ok(schema)
}
pub(crate) fn validate_service(name: &str, value: &IvyJson) -> IvyResult<()> {
    service()?
        .validators
        .validate(&phone_service_schema(name)?, value, 256 * 1024)
}
