use super::*;
use ivy_contracts::JsString;

#[test]
fn lifecycle_record_moves_payload_metadata_and_pin_identity() {
    let id = JsString(vec![0x61, 0xd800]);
    let id_pointer = id.0.as_ptr();
    let value = Json::Object(vec![("raw".into(), 1.0.into()), ("raw".into(), 2.0.into())]);
    let value_pointer = value.as_object().unwrap().as_ptr();
    let metadata = Json::object([("parentId", Json::String(JsString(vec![0xdc00])))]);
    let metadata_pointer = metadata.as_object().unwrap().as_ptr();
    let stored = StoredRecord {
        key: "task-board/task".into(),
        pin: crate::operations::ObjectPin {
            object_id: id,
            revision: 3.0,
        },
        value,
        metadata,
        revision: Json::object([("contentHash", "retained".into())]),
    };
    let record = lifecycle_document(stored).unwrap();
    assert_eq!(record.document.pin.object_id.0.as_ptr(), id_pointer);
    assert_eq!(record.document.pin.revision, 3.0);
    assert_eq!(
        record.document.value.as_object().unwrap().as_ptr(),
        value_pointer
    );
    assert_eq!(
        record.metadata.as_object().unwrap().as_ptr(),
        metadata_pointer
    );
    assert_eq!(record.document.value.as_object().unwrap().len(), 2);
    assert_eq!(record.document.parent_id, Some(JsString(vec![0xdc00])));
    assert_eq!(record.document.content_hash, "retained");
}
