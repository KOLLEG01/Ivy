use super::*;
use ivy_contracts::{json::canonical, limits::MANAGEMENT_FRAME_BYTES};

fn rows<'a>(snapshot: &'a Json, table: &str) -> &'a [Json] {
    f(snapshot, table).as_array().unwrap()
}
fn columns(row: &Json) -> &[Json] {
    row.as_array().unwrap()
}

// This fixture releases only its named terminal control. All other rows,
// including the question's complete publication graph, must remain identical.
pub(super) fn after_control(before: &Json, id: &str, operation: &Json) -> Json {
    let documents = rows(before, "documents");
    let root = documents
        .iter()
        .find(|row| columns(row)[0] == Json::from(id))
        .unwrap();
    let removed = documents
        .iter()
        .filter(|row| columns(row)[0] == Json::from(id) || columns(row)[3] == Json::from(id))
        .map(|row| columns(row)[0].clone())
        .collect::<Vec<_>>();
    let revisions = rows(before, "revisions")
        .iter()
        .filter(|row| removed.contains(&columns(row)[0]))
        .collect::<Vec<_>>();
    let blobs = rows(before, "blobs")
        .iter()
        .filter(|row| removed.contains(&columns(row)[0]) || removed.contains(&columns(row)[3]))
        .collect::<Vec<_>>();
    let mut expected = before.clone();
    for (table, _) in TABLES {
        let keep = rows(before, table)
            .iter()
            .filter(|row| {
                let row = columns(row);
                match *table {
                    "documents" | "revisions" | "cleanup_roots" | "active_gates"
                    | "operation_gates" => !removed.contains(&row[0]),
                    "cleanup_dependencies" => {
                        !removed.contains(&row[0]) && !removed.contains(&row[1])
                    }
                    "blobs" => !removed.contains(&row[0]) && !removed.contains(&row[3]),
                    _ => true,
                }
            })
            .cloned()
            .collect();
        expected.insert(*table, Json::Array(keep));
    }
    let usage = rows(before, "storage_usage")
        .iter()
        .map(|row| {
            let mut row = columns(row).to_vec();
            let (count, bytes) = if row[0] == Json::from("revisions") {
                (
                    revisions.len(),
                    revisions
                        .iter()
                        .map(|row| columns(row)[2].as_str().unwrap().len())
                        .sum::<usize>(),
                )
            } else {
                assert_eq!(row[0], Json::from("blobs"));
                (
                    blobs.len(),
                    blobs
                        .iter()
                        .map(|row| columns(row)[4].as_str().unwrap().len() / 2)
                        .sum(),
                )
            };
            row[1] = Json::Number(row[1].as_f64().unwrap() - count as f64);
            row[2] = Json::Number(row[2].as_f64().unwrap() - bytes as f64);
            Json::Array(row)
        })
        .collect();
    expected.insert("storage_usage", Json::Array(usage));
    assert!(rows(before, "retired_operations").is_empty());
    assert!(rows(before, "retired_operation_receipts").is_empty());
    let caller = Json::from(String::from_utf16_lossy(
        &f(operation, "callerPrincipalId").as_js_string().unwrap().0,
    ));
    expected.insert(
        "retired_operations",
        Json::Array(vec![Json::Array(vec![
            caller.clone(),
            f(operation, "operationId").clone(),
            f(operation, "requestHash").clone(),
            columns(root)[2].clone(),
        ])]),
    );
    let request = f(operation, "request");
    let action = f(request, "action");
    assert!([Json::from("transition"), Json::from("configure")].contains(action));
    let receipt = Json::object([
        ("receipt", true.into()),
        ("operationId", f(operation, "operationId").clone()),
        (
            "callerPrincipalId",
            f(operation, "callerPrincipalId").clone(),
        ),
        ("requestHash", f(operation, "requestHash").clone()),
        ("submittedHash", f(operation, "requestHash").clone()),
        ("phase", f(operation, "phase").clone()),
        ("action", action.clone()),
        (
            "taskId",
            request.get("taskId").cloned().unwrap_or(Json::Null),
        ),
        ("updatedAt", f(operation, "updatedAt").clone()),
        ("outcome", f(operation, "outcome").clone()),
        ("error", f(operation, "error").clone()),
    ]);
    expected.insert(
        "retired_operation_receipts",
        Json::Array(vec![Json::Array(vec![
            caller,
            f(operation, "operationId").clone(),
            canonical(&receipt, MANAGEMENT_FRAME_BYTES).unwrap().into(),
        ])]),
    );
    expected
}
