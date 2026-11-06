//! Suppressed optional delivery releases only an exactly observed native turn.
use super::{IvyError, IvyResult, storage_error};
use ivy_store::rusqlite::{Connection, OptionalExtension, params};

const MAX_PROOFS: i64 = 1024;
const MAX_PROOF_BYTES: i64 = 1024 * 1024;
const MAX_PER_OPERATION: i64 = 8;

pub(super) fn release(db: &Connection, thread: &str, turn: &str) -> IvyResult<()> {
    db.execute("UPDATE completion_receipts SET released=1 WHERE operation_key IN
        (SELECT key FROM state WHERE kind='operation' AND json_extract(value_json,'$.method')='turn/start'
          AND json_extract(value_json,'$.phase')='succeeded' AND json_extract(value_json,'$.params.threadId')=?
          AND json_extract(value_json,'$.reply.result.turn.id')=?)", params![thread,turn]).map_err(storage_error)?;
    Ok(())
}

/// Runs in the caller's transaction. No unmatched observation creates a row.
/// Dispatched intent exists before native execution; a late original reply can
/// consume this proof without another native read, publication or action.
pub(super) fn suppress(db: &Connection, thread: &str, turn: &str, epoch: &str) -> IvyResult<()> {
    release(db, thread, turn)?;
    let (mut count, mut bytes): (i64, i64) = db
        .query_row(
            "SELECT count(*),coalesce(sum(byte_length),0) FROM suppressed_completions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(storage_error)?;
    if count >= MAX_PROOFS || bytes >= MAX_PROOF_BYTES {
        return Ok(());
    }
    let mut pending = db.prepare("SELECT key FROM state INDEXED BY state_pending_completion
        WHERE kind='operation' AND json_extract(value_json,'$.method')='turn/start'
          AND json_extract(value_json,'$.phase') IN ('dispatched','outcome_unknown')
          AND json_extract(value_json,'$.params.threadId')=? AND json_extract(value_json,'$.epoch')=?
        ORDER BY key LIMIT ?").map_err(storage_error)?;
    let keys = pending
        .query_map(params![thread, epoch, MAX_PROOFS], |row| {
            row.get::<_, String>(0)
        })
        .map_err(storage_error)?;
    for key in keys {
        let key = key.map_err(storage_error)?;
        let exists: Option<i64> = db
            .query_row(
                "SELECT 1 FROM suppressed_completions WHERE operation_key=? AND turn_id=?",
                params![key, turn],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        if exists.is_some() {
            continue;
        }
        let owned: i64 = db
            .query_row(
                "SELECT count(*) FROM suppressed_completions WHERE operation_key=?",
                [&key],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        // Count, UTF-8 storage and index/row metadata are bounded separately
        // from the original operation's durable quota and outcome reservation.
        let charge = key
            .len()
            .saturating_add(turn.len())
            .saturating_add(thread.len())
            .saturating_add(epoch.len())
            .saturating_mul(2)
            .saturating_add(128);
        let charge = i64::try_from(charge).map_err(|_| IvyError::internal())?;
        if owned >= MAX_PER_OPERATION
            || count >= MAX_PROOFS
            || bytes.saturating_add(charge) > MAX_PROOF_BYTES
        {
            continue; // The original receipt stays protected when proof capacity is occupied.
        }
        db.execute(
            "INSERT INTO suppressed_completions VALUES(?,?,?,?,?)",
            params![key, turn, thread, epoch, charge],
        )
        .map_err(storage_error)?;
        count += 1;
        bytes += charge;
    }
    Ok(())
}

pub(super) fn finish(
    db: &Connection,
    key: &str,
    thread: &str,
    turn: &str,
    epoch: &str,
) -> IvyResult<()> {
    db.execute(
        "UPDATE completion_receipts SET released=1 WHERE operation_key=?
        AND EXISTS(SELECT 1 FROM suppressed_completions WHERE operation_key=? AND turn_id=? AND thread_id=? AND epoch=?)",
        params![key, key, turn, thread, epoch],
    )
    .map_err(storage_error)?;
    Ok(())
}

pub(super) fn forget(db: &Connection, key: &str) -> IvyResult<()> {
    db.execute(
        "DELETE FROM suppressed_completions WHERE operation_key=?",
        [key],
    )
    .map_err(storage_error)?;
    Ok(())
}
