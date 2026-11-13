//! Current technical heads contain index inputs; their revision owns the original.
use super::*;

pub(crate) fn technical_head(value: &IvyJson) -> IvyResult<String> {
    let mut head = IvyJson::object([] as [(&str, IvyJson); 0]);
    for field in ["assignmentId", "phase", "threadId", "operationId"] {
        if let Some(value) = value.get(field) {
            head.insert(field, value.clone());
        }
    }
    for (field, children) in [
        ("request", &["itemId", "operationId"][..]),
        ("pending", &["dueAt"][..]),
        ("contextRelease", &["state"][..]),
    ] {
        let mut projection = IvyJson::object([] as [(&str, IvyJson); 0]);
        if let Some(value) = value.get(field) {
            for child in children {
                if let Some(value) = value.get(child) {
                    projection.insert(*child, value.clone());
                }
            }
        }
        if projection
            .as_object()
            .is_some_and(|fields| !fields.is_empty())
        {
            head.insert(field, projection);
        }
    }
    canonical(&head, FRAME)
}

/// Sealed imports hash the unchanged original revision, after checking its head.
pub(crate) fn check_technical_head(head: &str, original: &str) -> IvyResult<()> {
    if head == original || technical_head(&parse(original, FRAME)?)? == head {
        return Ok(());
    }
    Err(IvyError::new(
        "secretary_import_invalid",
        "The technical head differs from its original revision.",
    ))
}

impl Journal {
    /// Rebuild only duplicated derived heads, after the owner/import backup gate.
    /// Stream one original at a time; history, quota and business revisions stay exact.
    pub(super) fn compact_technical_heads(&self) -> IvyResult<()> {
        self.transaction(|| {
            let mut statement = self.db.prepare("SELECT d.id,r.value_json FROM technical_documents d JOIN technical_revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.value_json=r.value_json ORDER BY d.id").map_err(storage)?;
            let mut rows = statement.query([]).map_err(storage)?;
            while let Some(row) = rows.next().map_err(storage)? {
                let id: String = row.get(0).map_err(storage)?;
                let original: String = row.get(1).map_err(storage)?;
                // Preserve malformed retained originals for explicit recovery;
                // deriving an index projection must not rewrite their evidence.
                let Ok(value) = parse(&original, FRAME) else { continue; };
                let head = technical_head(&value)?;
                if head != original {
                    self.db.execute("UPDATE technical_documents SET value_json=? WHERE id=? AND value_json=?", params![head,id,original]).map_err(storage)?;
                }
            }
            Ok(())
        })
    }
}
