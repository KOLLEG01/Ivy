//! Run-local retention projections. Keep IDs and edges, not every JSON payload.
use super::*;
use std::collections::HashMap;

struct OperationIdentity {
    value: String,
    issued_at: u64,
}

fn operation_identity(value: &IvyJson) -> Option<OperationIdentity> {
    match value {
        IvyJson::String(_) => {
            let value = value.as_str().ok()?;
            let issued_at = parse_operation_id(&value).ok()?.issued_at_unix_ms;
            (issued_at >= 1_000_000_000_000).then_some(OperationIdentity { value, issued_at })
        }
        IvyJson::Array(values) => values.iter().find_map(operation_identity),
        IvyJson::Object(fields) => fields
            .iter()
            .find_map(|(_, value)| operation_identity(value)),
        _ => None,
    }
}

fn references(value: &IvyJson, ids: &HashMap<&str, usize>, found: &mut HashSet<usize>) {
    match value {
        IvyJson::String(_) => {
            if let Some(id) = value
                .as_str()
                .ok()
                .and_then(|value| ids.get(value.as_str()).copied())
            {
                found.insert(id);
            }
        }
        IvyJson::Array(values) => {
            for value in values {
                references(value, ids, found);
            }
        }
        IvyJson::Object(fields) => {
            for (_, value) in fields {
                references(value, ids, found);
            }
        }
        _ => {}
    }
}

fn reference_ids(value: &IvyJson, ids: &HashMap<&str, usize>) -> HashSet<usize> {
    let mut found = HashSet::new();
    references(value, ids, &mut found);
    found
}

enum FollowUp {
    Other,
    Context {
        item: Option<IvyJson>,
        pending_thread: bool,
    },
    Active {
        item: Option<IvyJson>,
    },
}

struct Technical {
    id: usize,
    triage_thread: bool,
    references: Vec<usize>,
    follow_up: FollowUp,
    // Receipt errors are observed only after the same protection checks as deletion.
    blocked: IvyResult<bool>,
    operation: Option<OperationIdentity>,
}

struct DomainReferences {
    id: Option<usize>,
    references: Vec<usize>,
}

#[derive(Clone, Copy)]
enum Candidate<'a> {
    Operation(usize),
    Technical(&'a str),
}

impl Journal {
    fn fence(&self, identity: &OperationIdentity) -> IvyResult<()> {
        let boundary = identity.issued_at.max(self.expired_before()?);
        self.db
            .execute(
                "INSERT OR IGNORE INTO retired_operations VALUES(?,?)",
                params![identity.value, identity.issued_at as i64],
            )
            .map_err(storage)?;
        self.db.execute("INSERT INTO journal_state(key,value) VALUES('expiredBefore',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [boundary.to_string()]).map_err(storage)?;
        let boundary = i64::try_from(boundary).map_err(|_| {
            IvyError::new(
                "storage_unavailable",
                "The journal expiry boundary is invalid.",
            )
        })?;
        self.db
            .execute(
                "DELETE FROM retired_operations WHERE issued_at<?",
                [boundary],
            )
            .map_err(storage)?;
        Ok(())
    }

    fn technical_retention(&self, ids: &HashMap<&str, usize>) -> IvyResult<Vec<Technical>> {
        let mut statement = self
            .db
            .prepare("SELECT id,key,value_json FROM technical_documents")
            .map_err(storage)?;
        let mut rows = statement.query([]).map_err(storage)?;
        let mut documents = Vec::new();
        while let Some(row) = rows.next().map_err(storage)? {
            let id: String = row.get(0).map_err(storage)?;
            let key: String = row.get(1).map_err(storage)?;
            let raw: String = row.get(2).map_err(storage)?;
            let value = parse(&raw, FRAME)?;
            let phase = text(&value, "phase");
            let follow_up = match key.as_str() {
                // Preserve missing/null and lossless identifier equality without the body.
                "secretary/follow-up-context" => FollowUp::Context {
                    item: value.get("itemId").cloned(),
                    pending_thread: value.get("threadId").is_some_and(IvyJson::is_null),
                },
                "secretary/follow-up"
                    if matches!(phase.as_str(), "prepared" | "running" | "outcome_unknown") =>
                {
                    FollowUp::Active {
                        item: value
                            .get("request")
                            .and_then(|request| request.get("itemId"))
                            .cloned(),
                    }
                }
                _ => FollowUp::Other,
            };
            documents.push(Technical {
                id: ids[id.as_str()],
                triage_thread: key == "secretary/triage-thread",
                references: reference_ids(&value, ids).into_iter().collect(),
                follow_up,
                blocked: has_unresolved_outcome(&value).map(|unknown| {
                    unknown
                        || matches!(
                            phase.as_str(),
                            "accepted"
                                | "dispatched"
                                | "prepared"
                                | "running"
                                | "pending"
                                | "archiving",
                        )
                }),
                operation: value
                    .get("operationId")
                    .filter(|value| !value.is_null())
                    .or_else(|| {
                        value
                            .get("request")
                            .and_then(|request| request.get("operationId"))
                    })
                    .and_then(operation_identity),
            });
        }
        Ok(documents)
    }

    fn domain_retention(&self, ids: &HashMap<&str, usize>) -> IvyResult<Vec<DomainReferences>> {
        let mut statement = self
            .db
            .prepare("SELECT id,value_json FROM documents")
            .map_err(storage)?;
        let mut rows = statement.query([]).map_err(storage)?;
        let mut documents = Vec::new();
        while let Some(row) = rows.next().map_err(storage)? {
            let id: String = row.get(0).map_err(storage)?;
            let raw: String = row.get(1).map_err(storage)?;
            documents.push(DomainReferences {
                id: ids.get(id.as_str()).copied(),
                references: reference_ids(&parse(&raw, FRAME)?, ids)
                    .into_iter()
                    .collect(),
            });
        }
        Ok(documents)
    }

    pub(super) fn cleanup_inner(
        &self,
        extra_bytes: i64,
        extra_count: i64,
        protected: Option<&str>,
        incoming: Option<&IvyJson>,
    ) -> IvyResult<()> {
        let needs_room = || -> IvyResult<bool> {
            let usage = self.usage()?;
            Ok(usage.count + extra_count > self.limits.count
                || usage.bytes + extra_bytes > self.limits.bytes)
        };
        if !needs_room()? {
            return Ok(());
        }
        self.db.execute("INSERT OR IGNORE INTO technical_cleanup SELECT id,0 FROM technical_documents WHERE key IN ('secretary/public-operation','secretary/follow-up') AND json_extract(value_json,'$.phase')='succeeded'", []).map_err(storage)?;
        let identities: Vec<String> = self.db.prepare("SELECT id FROM documents WHERE key='secretary/operation' AND completed_at IS NOT NULL UNION SELECT id FROM technical_documents UNION SELECT id FROM technical_blobs")
            .map_err(storage)?.query_map([], |row| row.get(0)).map_err(storage)?
            .collect::<rusqlite::Result<_>>().map_err(storage)?;
        let ids: HashMap<&str, usize> = identities
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect();
        let operations: Vec<(usize, i64)> = self.db.prepare("SELECT id,completed_at FROM documents WHERE key='secretary/operation' AND completed_at IS NOT NULL ORDER BY completed_at,rowid")
            .map_err(storage)?.query_map([], |row| Ok((ids[row.get::<_, String>(0)?.as_str()], row.get(1)?)))
            .map_err(storage)?.collect::<rusqlite::Result<_>>().map_err(storage)?;
        let documents = self.technical_retention(&ids)?;
        let incoming_refs = incoming
            .map(|value| reference_ids(value, &ids))
            .unwrap_or_default();
        let mut protected_operations = incoming_refs.clone();
        if let Some(id) = protected.and_then(|id| ids.get(id)) {
            protected_operations.insert(*id);
        }
        for document in &documents {
            protected_operations.extend(&document.references);
        }
        {
            let mut statement = self.db.prepare("SELECT value_json FROM documents WHERE key='secretary/operation' AND completed_at IS NULL").map_err(storage)?;
            let mut rows = statement.query([]).map_err(storage)?;
            while let Some(row) = rows.next().map_err(storage)? {
                let raw: String = row.get(0).map_err(storage)?;
                protected_operations.extend(reference_ids(&parse(&raw, FRAME)?, &ids));
            }
        }
        let roots: Vec<(String, i64)> = self
            .db
            .prepare(
                "SELECT root_id,delete_after FROM technical_cleanup ORDER BY delete_after,rowid",
            )
            .map_err(storage)?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage)?
            .collect::<rusqlite::Result<_>>()
            .map_err(storage)?;
        let mut candidates: Vec<_> = operations
            .iter()
            .map(|(id, at)| (*at, Candidate::Operation(*id)))
            .chain(
                roots
                    .iter()
                    .map(|(id, at)| (*at, Candidate::Technical(id.as_str()))),
            )
            .collect();
        // Stable ordering keeps operations before technical roots at equal times.
        candidates.sort_by_key(|(at, _)| *at);
        let mut removed_operations = vec![false; identities.len()];
        let mut domain = None;
        for (_, candidate) in candidates {
            let id = match candidate {
                Candidate::Operation(index) => identities[index].as_str(),
                Candidate::Technical(id) => id,
            };
            if protected == Some(id) {
                continue;
            }
            if let Candidate::Operation(index) = candidate {
                let raw: String = self
                    .db
                    .query_row("SELECT value_json FROM documents WHERE id=?", [id], |row| {
                        row.get(0)
                    })
                    .map_err(storage)?;
                let value = parse(&raw, FRAME)?;
                if !matches!(text(&value, "phase").as_str(), "succeeded" | "failed")
                    || protected_operations.contains(&index)
                    || has_unresolved_outcome(&value)?
                {
                    continue;
                }
                if text(&value, "action") == "capture" && text(&value, "phase") == "succeeded"
                    && !self.db.query_row("SELECT EXISTS(SELECT 1 FROM capture_acknowledgements WHERE capture_id=?)", [id], |row| row.get::<_, bool>(0)).map_err(storage)? { continue; }
                let Some(identity) = value.get("operationId").and_then(operation_identity) else {
                    continue;
                };
                self.fence(&identity)?;
                for sql in [
                    "DELETE FROM revisions WHERE id=?",
                    "DELETE FROM documents WHERE id=?",
                    "DELETE FROM capture_acknowledgements WHERE capture_id=?",
                ] {
                    self.db.execute(sql, [id]).map_err(storage)?;
                }
                removed_operations[index] = true;
                if !needs_room()? {
                    break;
                }
                continue;
            }
            let mut selected: HashSet<usize> = ids.get(id).copied().into_iter().collect();
            loop {
                let before = selected.len();
                for document in &documents {
                    if document.triage_thread {
                        continue;
                    }
                    if selected.contains(&document.id)
                        || document.references.iter().any(|id| selected.contains(id))
                    {
                        selected.insert(document.id);
                        selected.extend(&document.references);
                    }
                }
                if before == selected.len() {
                    break;
                }
            }
            if documents.iter().any(|document| selected.contains(&document.id) && match &document.follow_up {
                FollowUp::Context { item, pending_thread } => *pending_thread || documents.iter().any(|other| matches!(&other.follow_up, FollowUp::Active { item: other } if other == item)),
                _ => false,
            }) { continue; }
            if incoming_refs.iter().any(|id| selected.contains(id))
                || protected
                    .and_then(|id| ids.get(id))
                    .is_some_and(|id| selected.contains(id))
            {
                continue;
            }
            let mut blocked = false;
            for document in documents
                .iter()
                .filter(|document| selected.contains(&document.id))
            {
                if *document.blocked.as_ref().map_err(Clone::clone)? {
                    blocked = true;
                    break;
                }
            }
            if blocked
                || documents.iter().any(|document| {
                    !selected.contains(&document.id)
                        && document.references.iter().any(|id| selected.contains(id))
                })
            {
                continue;
            }
            if domain.is_none() {
                domain = Some(self.domain_retention(&ids)?);
            }
            if domain.as_ref().unwrap().iter().any(|document| {
                !document
                    .id
                    .is_some_and(|id| selected.contains(&id) || removed_operations[id])
                    && document.references.iter().any(|id| selected.contains(id))
            }) {
                continue;
            }
            for document in documents
                .iter()
                .filter(|document| selected.contains(&document.id))
            {
                if let Some(identity) = &document.operation {
                    self.fence(identity)?;
                }
            }
            // A cleanup root can outlive its head; still remove its orphan revisions.
            for id in selected
                .iter()
                .map(|index| identities[*index].as_str())
                .chain((!ids.contains_key(id)).then_some(id))
            {
                for sql in [
                    "DELETE FROM technical_blobs WHERE id=?",
                    "DELETE FROM technical_revisions WHERE id=?",
                    "DELETE FROM technical_documents WHERE id=? AND key<>'secretary/triage-thread'",
                    "DELETE FROM technical_cleanup WHERE root_id=?",
                ] {
                    self.db.execute(sql, [id]).map_err(storage)?;
                }
            }
            if !needs_room()? {
                break;
            }
        }
        Ok(())
    }
}
