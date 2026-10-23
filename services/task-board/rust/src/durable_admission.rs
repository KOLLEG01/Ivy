//! Current durable workflow admission over the supplied canonical repository
//! bundle. Retained catalogs and operation identity protocols remain separate.
use super::Contracts;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, builtin_schema::SchemaBundle, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};

/// The runtime owner supplies the complete trusted TaskBoard, Chat, Agent, wire
/// and Hive-operations registry. No schema is copied, rewritten or substituted.
pub struct DurableAdmission {
    schemas: SchemaBundle,
}
impl DurableAdmission {
    pub fn new(schemas: SchemaBundle) -> Self {
        Self { schemas }
    }
    fn validate(&self, name: &str, value: &Json) -> IvyResult<()> {
        canonical(value, MANAGEMENT_FRAME_BYTES)?;
        if self.schemas.matches(
            &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
            value,
        )? {
            Ok(())
        } else {
            Err(IvyError::new(
                "invalid_arguments",
                "Value does not match its TaskBoard workflow contract.",
            ))
        }
    }
    pub fn request(&self, value: &Json) -> IvyResult<()> {
        self.validate("DurableRequest", value)
    }
    pub fn operation(&self, value: &Json) -> IvyResult<()> {
        self.validate("Operation", value)
    }
    pub fn receipt(&self, value: &Json) -> IvyResult<()> {
        self.validate("OperationReceipt", value)
    }
}

/// Bind current durable definitions and Operation v1 records to their complete
/// native guard. Routing, readable versions, other retained records/definitions
/// and their original errors continue through the supplied contract owner.
pub struct DurableContracts<'a, P> {
    pub retained: &'a P,
    pub current: &'a DurableAdmission,
}
impl<P: Contracts> Contracts for DurableContracts<'_, P> {
    fn version(&self, key: &str) -> IvyResult<String> {
        self.retained.version(key)
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        self.retained.local(key)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        self.retained.readable_version(key, version)
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        if key == "task-board/operation" && version == &JsString::from("1.0.0") {
            self.current.operation(value)
        } else {
            self.retained.validate_record(key, version, value)
        }
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        match name {
            "DurableRequest" => self.current.request(value),
            "Operation" => self.current.operation(value),
            "OperationReceipt" => self.current.receipt(value),
            _ => self.retained.validate_definition(name, value),
        }
    }
}
