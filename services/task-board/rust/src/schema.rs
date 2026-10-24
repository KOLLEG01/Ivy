use crate::journal_store::Contracts;
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::parse};
use ivy_contracts::{
    JsString,
    builtin_schema::SchemaBundle,
    json::{canonical, compare_versions},
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::collections::HashMap;

/// Complete published definitions in taskBoardContracts() return order.
/// Historical versions retain their original schemas, retention and specs.
pub use ivy_contracts::published_contracts::task_board_contracts;

const DOCUMENT: &str = "https://ivy.invalid/schemas/task-board.schema.json";
const INTERNAL: &[(&str, &str)] = &[
    ("task-board/native-signals", "NativeSignals"),
    ("task-board/native-plan", "NativePlan"),
    ("task-board/native-call", "NativeCall"),
    ("task-board/operation", "Operation"),
    ("task-board/coordination", "Coordination"),
    ("task-board/native-evidence", "NativeEvidence"),
    ("task-board/native-read-evidence", "NativeReadEvidence"),
    ("task-board/scan-cursor", "ScanCursor"),
    ("task-board/scheduler-attempt", "SchedulerAttempt"),
    ("task-board/native-turn-search", "NativeTurnSearch"),
    ("task-board/native-turn-snapshot", "NativeTurnSnapshot"),
    ("task-board/native-result-page", "NativeResultPage"),
    ("task-board/native-transcript", "NativeTranscript"),
    (
        "task-board/native-full-turn-transcript",
        "NativeFullTurnTranscript",
    ),
];

/// Native admission and routing for the service's trusted packaged contracts.
/// Public record versions keep their published schemas; current operation
/// definitions come from the supplied canonical documents. Submitted registry
/// schemas are admitted separately by Hive.
pub struct RuntimeContracts {
    definitions: SchemaBundle,
    versions: HashMap<String, String>,
    records: HashMap<(String, JsString), Option<SchemaBundle>>,
    readable: HashMap<String, Vec<JsString>>,
}
impl RuntimeContracts {
    pub fn from_documents(documents: Vec<IvyJson>) -> IvyResult<Self> {
        let definitions = SchemaBundle::from_documents(documents)?;
        let metadata = crate::runtime_inputs::metadata()?;
        let writers = metadata.required("writerVersions")?;
        let mut versions = HashMap::new();
        let mut records = HashMap::new();
        let mut readable: HashMap<String, Vec<JsString>> = HashMap::new();
        // Retain every immutable published read schema. Writer identities are
        // explicit: detached comment collections keep their original writer.
        for contract in task_board_contracts()? {
            let key = contract.required("key")?.as_str()?;
            let version = contract.required("version")?.as_str()?;
            let ordered = readable.entry(key.clone()).or_default();
            let mut position = 0;
            while position < ordered.len()
                && compare_versions(&ordered[position].to_utf8()?, &version)?.is_lt()
            {
                position += 1;
            }
            if ordered.get(position) != Some(&JsString::from(version.clone())) {
                ordered.insert(position, version.clone().into());
            }
            versions
                .entry(key.clone())
                .or_insert(writers.required(&key)?.as_str()?);
            let schema = contract
                .get("jsonSchema")
                .map(|s| SchemaBundle::from_documents(vec![s.clone()]))
                .transpose()?;
            records.insert((key, version.into()), schema);
        }
        Ok(Self {
            definitions,
            versions,
            records,
            readable,
        })
    }
    /// Exact published reader versions in numeric version order. This never
    /// changes a writer version or admits a schema outside the retained bundle.
    pub fn readable_versions(&self, key: &str) -> IvyResult<&[JsString]> {
        if Self::internal(key).is_some() {
            static INTERNAL_VERSION: std::sync::OnceLock<Vec<JsString>> =
                std::sync::OnceLock::new();
            return Ok(INTERNAL_VERSION.get_or_init(|| vec!["1.0.0".into()]));
        }
        self.readable
            .get(key)
            .map(Vec::as_slice)
            .ok_or_else(IvyError::internal)
    }
    fn internal(key: &str) -> Option<&'static str> {
        INTERNAL
            .iter()
            .find_map(|(k, name)| (*k == key).then_some(*name))
    }
    fn invalid() -> IvyError {
        IvyError::new(
            "invalid_arguments",
            "Value does not match its TaskBoard contract.",
        )
    }
}
impl Contracts for RuntimeContracts {
    fn version(&self, key: &str) -> IvyResult<String> {
        if Self::internal(key).is_some() {
            return Ok("1.0.0".into());
        }
        self.versions
            .get(key)
            .cloned()
            .ok_or_else(IvyError::internal)
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        if Self::internal(key).is_some() {
            return Ok(true);
        }
        self.version(key)?;
        Ok(false)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        if Self::internal(key).is_some() {
            return Ok(version == &JsString::from("1.0.0"));
        }
        Ok(self
            .records
            .contains_key(&(key.to_owned(), version.clone())))
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &IvyJson) -> IvyResult<()> {
        if let Some(name) = Self::internal(key) {
            if version != &JsString::from("1.0.0") {
                return Err(Self::invalid());
            }
            return self.validate_definition(name, value);
        }
        canonical(value, MANAGEMENT_FRAME_BYTES)?;
        let schema = self
            .records
            .get(&(key.to_owned(), version.clone()))
            .and_then(Option::as_ref)
            .ok_or_else(Self::invalid)?;
        if schema.matches("#", value)? {
            Ok(())
        } else {
            Err(Self::invalid())
        }
    }
    fn validate_definition(&self, name: &str, value: &IvyJson) -> IvyResult<()> {
        canonical(value, MANAGEMENT_FRAME_BYTES)?;
        if self
            .definitions
            .matches(&format!("{DOCUMENT}#/$defs/{name}"), value)?
        {
            Ok(())
        } else {
            Err(Self::invalid())
        }
    }
}
