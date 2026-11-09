//! Disposable identities for one exact immutable conversation per task.
//! Bodies remain in Hive. A cold or oversized projection is streamed, never
//! interpreted as an empty history. Transactions do not span remote reads.
use super::{Contracts, JournalStore, persistent_reads::task_comments::CommentFailure};
use crate::local_journal::{require, transaction};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::hash_json,
    json::{parse, stringify},
};
use ivy_sdk::rpc::RpcClient;
use ivy_store::{
    rusqlite::{Connection, OptionalExtension, params},
    sqlite::error,
};

const MAX_BYTES: i64 = 8 * 1024 * 1024;
const MAX_ROOTS: i64 = 128;
const BATCH: usize = 16;
pub(crate) type Identity = (u8, JsString);
type NativeQuestions = Vec<(usize, JsString)>;

fn key(value: &JsString) -> IvyResult<String> {
    // Preserve every wire UTF-16 unit, including unpaired surrogates.
    stringify(&Json::String(value.clone()), usize::MAX)
}
fn tree_key(tree: &Json) -> IvyResult<String> {
    hash_json(&Json::object([
        ("model", Json::Number(7.0)),
        ("root", tree.required("root")?.clone()),
        ("length", tree.required("length")?.clone()),
    ]))
}
fn count(value: &Json) -> IvyResult<usize> {
    let n = value.as_f64().ok_or_else(IvyError::internal)?;
    require(
        n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0 && n < usize::MAX as f64,
        "task_board_comment_mismatch",
        "The conversation sequence is invalid.",
    )?;
    Ok(n as usize)
}
fn sequence(comment: &Json) -> IvyResult<usize> {
    let n = count(comment.required("sequence")?)?;
    require(
        n > 0,
        "task_board_comment_mismatch",
        "The conversation sequence is invalid.",
    )?;
    Ok(n)
}
fn ordinary_question(comment: &Json) -> bool {
    comment.get("purpose").is_some_and(|v| v.is_str("question"))
        && ["nativeInput", "sourceTaskId"]
            .iter()
            .all(|key| comment.get(*key).is_none_or(Json::is_null))
}
fn identities(comment: &Json) -> IvyResult<Vec<Identity>> {
    let mut ids = vec![(
        0,
        comment
            .required("commentId")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?,
    )];
    ids.push((4, ids[0].1.clone())); // First occurrence detects historical repeated IDs.
    if comment.get("purpose").is_some_and(|v| v.is_str("question"))
        && ["nativeInput", "sourceTaskId"]
            .iter()
            .all(|key| comment.get(*key).is_none_or(Json::is_null))
    {
        ids.push((3, ids[0].1.clone()));
    }
    for (kind, field) in [(1, "requests"), (2, "responses")] {
        for item in comment
            .required(field)?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let id = item
                .required("requestId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?;
            ids.push((kind, id.clone()));
            if kind == 1 {
                ids.push((12, id.clone())); // Earliest request, paired with latest (1).
            }
        }
    }
    if let Some(reply) = comment.get("replyTo").and_then(Json::as_js_string) {
        if comment
            .get("nativeInput")
            .and_then(|v| v.get("state"))
            .is_some_and(|v| v.is_str("answered") || v.is_str("expired"))
        {
            ids.push((11, reply.clone()));
        }
        if comment.get("authorKind").is_some_and(|v| v.is_str("user")) {
            ids.push((8, reply.clone()));
        }
        if let Some(source) = comment.get("sourceTaskId").and_then(Json::as_js_string) {
            ids.push((9, reply_identity(source, reply)?));
        }
    }
    for resolution in comment
        .get("resolvesQuestions")
        .and_then(Json::as_array)
        .unwrap_or(&[])
    {
        ids.push((
            10,
            resolution
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?,
        ));
    }
    if let Some(run) = comment
        .get("run")
        .and_then(|v| v.get("objectId"))
        .and_then(Json::as_js_string)
    {
        if comment.get("purpose").is_some_and(|v| v.is_str("handoff")) {
            ids.push((6, run.clone()));
        }
        if comment.get("authorKind").is_some_and(|v| v.is_str("agent"))
            && let Some(reply) = comment.get("replyTo").and_then(Json::as_js_string)
        {
            ids.push((7, reply_identity(run, reply)?));
        }
    }
    Ok(ids)
}
fn reply_identity(run: &JsString, reply: &JsString) -> IvyResult<JsString> {
    Ok(stringify(
        &Json::Array(vec![Json::String(run.clone()), Json::String(reply.clone())]),
        usize::MAX,
    )?
    .into())
}
fn queued_identity(comment: &Json) -> IvyResult<JsString> {
    Ok(stringify(
        &Json::Array(vec![
            comment.required("sequence")?.clone(),
            comment.required("commentId")?.clone(),
        ]),
        usize::MAX,
    )?
    .into())
}
fn queued(entry: &Json) -> bool {
    entry
        .get("receipt")
        .and_then(|receipt| receipt.get("state"))
        .is_some_and(|state| state.is_str("queued"))
}
fn failure(error: CommentFailure<IvyError>) -> IvyError {
    match error {
        CommentFailure::Source(e) | CommentFailure::Ivy(e) => e,
    }
}
struct Index<'a> {
    db: &'a Connection,
    task: String,
    root: String,
    generation: std::cell::Cell<i64>,
}
impl Index<'_> {
    fn current(&self, complete: bool) -> IvyResult<bool> {
        self.db.query_row("SELECT EXISTS(SELECT 1 FROM comment_index_roots WHERE task_id=? AND root_key=? AND complete=?)",
            params![self.task, self.root, complete], |r| r.get(0)).map_err(error)
    }
    fn positions(&self, ids: &[Identity]) -> IvyResult<Vec<Option<usize>>> {
        ids.iter()
            .map(|(kind, id)| {
                self.db.query_row(
            "SELECT sequence FROM comment_index_ids WHERE task_id=? AND kind=? AND identity=?",
            params![self.task, kind, key(id)?], |r| r.get::<_,i64>(0)).optional().map_err(error)?
                .map(|n|usize::try_from(n).map_err(|_|IvyError::internal())).transpose()
            })
            .collect()
    }
    fn queued_ids(&self) -> IvyResult<Vec<Json>> {
        let mut statement = self.db.prepare(
            "SELECT identity,sequence FROM comment_index_ids WHERE task_id=? AND kind=5 ORDER BY sequence"
        ).map_err(error)?;
        let rows = statement
            .query_map([&self.task], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(error)?;
        rows.map(|row| {
            let (encoded, sequence) = row.map_err(error)?;
            let text = parse(&encoded, usize::MAX)?.as_str()?;
            let pair = parse(&text, usize::MAX)?;
            let pair = pair.as_array().ok_or_else(IvyError::internal)?;
            require(
                pair.len() == 2 && pair[0].as_f64() == Some(sequence as f64),
                "task_board_comment_mismatch",
                "The queued receipt projection is invalid.",
            )?;
            Ok(pair[1].clone())
        })
        .collect()
    }
    fn touch(&self) -> IvyResult<()> {
        self.db.execute("UPDATE comment_index_roots SET last_used=(SELECT COALESCE(MAX(last_used),0)+1 FROM comment_index_roots) WHERE task_id=? AND root_key=?",
            params![self.task,self.root]).map_err(error)?;
        Ok(())
    }
    fn native_questions(
        &self,
        run_id: &JsString,
        comment_id: Option<&JsString>,
    ) -> IvyResult<NativeQuestions> {
        let mut arguments = vec![self.task.clone(), key(run_id)?];
        let sql = if let Some(id) = comment_id {
            arguments.push(key(id)?);
            "SELECT sequence,comment_id FROM comment_index_native_questions WHERE task_id=? AND run_id=? AND comment_id=? ORDER BY sequence"
        } else {
            "SELECT sequence,comment_id FROM comment_index_native_questions WHERE task_id=? AND run_id=? ORDER BY sequence"
        };
        let mut statement = self.db.prepare(sql).map_err(error)?;
        let rows = statement
            .query_map(ivy_store::rusqlite::params_from_iter(arguments), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(error)?;
        rows.map(|row| {
            let (sequence, id) = row.map_err(error)?;
            let id = parse(&id, usize::MAX)?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?;
            Ok((
                usize::try_from(sequence).map_err(|_| IvyError::internal())?,
                id,
            ))
        })
        .collect()
    }
    fn begin(&self) -> IvyResult<()> {
        transaction(self.db, || {
            self.db
                .execute(
                    "INSERT OR IGNORE INTO markers VALUES('comment-index-generation','0')",
                    [],
                )
                .map_err(error)?;
            let generation=self.db.query_row("UPDATE markers SET value=CAST(value AS INTEGER)+1 WHERE key='comment-index-generation' RETURNING CAST(value AS INTEGER)",[],|r|r.get(0)).map_err(error)?;
            self.generation.set(generation);
            self.db
                .execute(
                    "DELETE FROM comment_index_roots WHERE task_id=?",
                    [&self.task],
                )
                .map_err(error)?;
            self.db.execute("INSERT INTO comment_index_roots VALUES(?,?,?,0,0,(SELECT COALESCE(MAX(last_used),0)+1 FROM comment_index_roots))",
                params![self.task,self.root,generation]).map_err(error)?;
            self.trim()
        })
    }
    fn trim(&self) -> IvyResult<()> {
        loop {
            let (roots, bytes): (i64, i64) = self
                .db
                .query_row(
                    "SELECT COUNT(*),COALESCE(SUM(bytes),0) FROM comment_index_roots",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(error)?;
            if roots <= MAX_ROOTS && bytes <= MAX_BYTES {
                return Ok(());
            }
            self.db.execute("DELETE FROM comment_index_roots WHERE task_id=(SELECT task_id FROM comment_index_roots ORDER BY last_used,task_id LIMIT 1)",[]).map_err(error)?;
        }
    }
    fn add(&self, entries: &[Json]) -> IvyResult<()> {
        transaction(self.db, || {
            let owned=self.db.query_row("SELECT EXISTS(SELECT 1 FROM comment_index_roots WHERE task_id=? AND root_key=? AND complete=0 AND generation=?)",params![self.task,self.root,self.generation.get()],|r|r.get::<_,bool>(0)).map_err(error)?;
            if !owned {
                return Ok(());
            }
            let mut bytes = 0i64;
            for entry in entries {
                let comment = entry.required("comment")?;
                let sequence =
                    i64::try_from(sequence(comment)?).map_err(|_| IvyError::internal())?;
                let queued_id = queued_identity(comment)?;
                let encoded = key(&queued_id)?;
                if self
                    .db
                    .execute(
                        "DELETE FROM comment_index_ids WHERE task_id=? AND kind=5 AND identity=?",
                        params![self.task, encoded],
                    )
                    .map_err(error)?
                    == 1
                {
                    bytes -= encoded.len() as i64 + 64;
                }
                let mut ids = identities(comment)?;
                if queued(entry) {
                    ids.push((5, queued_id));
                }
                for (kind, id) in ids {
                    let encoded = key(&id)?;
                    let inserted = self
                        .db
                        .execute(
                            "INSERT OR IGNORE INTO comment_index_ids VALUES(?,?,?,?)",
                            params![self.task, kind, encoded, sequence],
                        )
                        .map_err(error)?;
                    if inserted == 1 {
                        bytes += encoded.len() as i64 + 64;
                    } else {
                        let sql = if kind == 4 || kind == 12 {
                            "UPDATE comment_index_ids SET sequence=MIN(sequence,?) WHERE task_id=? AND kind=? AND identity=?"
                        } else {
                            "UPDATE comment_index_ids SET sequence=MAX(sequence,?) WHERE task_id=? AND kind=? AND identity=?"
                        };
                        self.db
                            .execute(sql, params![sequence, self.task, kind, encoded])
                            .map_err(error)?;
                    }
                }
                if comment.get("purpose").is_some_and(|v| v.is_str("question"))
                    && comment
                        .get("author")
                        .and_then(|v| v.get("source"))
                        .is_some_and(|v| v.is_str("native"))
                    && let Some(run) = comment
                        .get("run")
                        .and_then(|v| v.get("objectId"))
                        .and_then(Json::as_js_string)
                {
                    let run = key(run)?;
                    let id = key(comment
                        .required("commentId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?)?;
                    if self
                        .db
                        .execute(
                            "INSERT OR IGNORE INTO comment_index_native_questions VALUES(?,?,?,?)",
                            params![self.task, run, sequence, id],
                        )
                        .map_err(error)?
                        == 1
                    {
                        bytes += (run.len() + id.len()) as i64 + 80;
                    }
                }
            }
            self.db
                .execute(
                    "UPDATE comment_index_roots SET bytes=bytes+? WHERE task_id=? AND root_key=?",
                    params![bytes, self.task, self.root],
                )
                .map_err(error)?;
            self.trim()
        })
    }
    fn finish(&self) -> IvyResult<()> {
        self.db.execute("UPDATE comment_index_roots SET complete=1 WHERE task_id=? AND root_key=? AND generation=?",params![self.task,self.root,self.generation.get()]).map_err(error)?;
        Ok(())
    }
}

impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString>
    JournalStore<'_, R, P, C, I>
{
    pub(crate) async fn appended_comment_summary(
        &self,
        tree: &Json,
        task: &JsString,
        comment: &Json,
    ) -> IvyResult<Json> {
        let mut summary = tree.required("summary")?.clone();
        let old_questions = summary
            .required("questions")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let response_ids = comment
            .required("responses")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|r| {
                Ok((
                    1,
                    r.required("requestId")?
                        .as_js_string()
                        .cloned()
                        .ok_or_else(IvyError::internal)?,
                ))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        let requested_ranges = if response_ids.is_empty() {
            vec![]
        } else {
            let mut ids = response_ids.clone();
            ids.extend(response_ids.iter().map(|(_, id)| (12, id.clone())));
            let positions = self.comment_positions(tree, task, &ids).await?;
            positions[..response_ids.len()]
                .iter()
                .zip(&positions[response_ids.len()..])
                .filter_map(|(last, first)| match (first, last) {
                    (Some(first), Some(last)) => Some(Ok(*first..=*last)),
                    (None, None) => None,
                    _ => Some(Err(IvyError::internal())),
                })
                .collect::<IvyResult<Vec<_>>>()?
        };
        let new_sequence = sequence(comment)?;
        let mut questions = Vec::with_capacity(old_questions.len() + 1);
        for item in old_questions {
            let id = item.required("commentId")?;
            let at = sequence(item)?;
            let affected = item.get("open").and_then(Json::as_bool) == Some(true)
                && (comment.get("replyTo") == Some(id)
                    || comment
                        .get("resolvesQuestions")
                        .and_then(Json::as_array)
                        .is_some_and(|ids| ids.contains(id))
                    || requested_ranges.iter().any(|range| range.contains(&at)));
            let mut closed = false;
            if affected {
                let question = self.comment_at(tree, task, at).await?;
                require(
                    question.get("commentId") == Some(id)
                        && question
                            .get("purpose")
                            .is_some_and(|v| v.is_str("question")),
                    "task_board_comment_mismatch",
                    "The indexed question differs from its current root.",
                )?;
                let pair = Json::object([(
                    "comments",
                    Json::Array(vec![question.clone(), comment.clone()]),
                )]);
                closed = !ivy_sdk::open_task_questions(&pair, None)?
                    .iter()
                    .any(|open| open.get("commentId") == Some(id));
                let requests = question
                    .required("requests")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?;
                if !closed
                    && !requests.is_empty()
                    && question.get("nativeInput").is_none_or(Json::is_null)
                {
                    let coverage = requests
                        .iter()
                        .map(|r| {
                            Ok((
                                2,
                                r.required("requestId")?
                                    .as_js_string()
                                    .cloned()
                                    .ok_or_else(IvyError::internal)?,
                            ))
                        })
                        .collect::<IvyResult<Vec<_>>>()?;
                    let previous = self.comment_positions(tree, task, &coverage).await?;
                    closed = coverage.iter().zip(previous).all(|((_, id), answered)| {
                        answered.is_some_and(|p| p > at)
                            || new_sequence > at
                                && response_ids.iter().any(|(_, response)| response == id)
                    });
                }
            }
            if !closed {
                questions.push(item.clone());
            }
        }
        let single = Json::object([("comments", Json::Array(vec![comment.clone()]))]);
        let added = super::persistent_reads::comment_projections::current_summary(&single, task)?;
        questions.extend(
            added
                .required("questions")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .cloned(),
        );
        let direct = summary
            .required("directCount")?
            .as_f64()
            .ok_or_else(IvyError::internal)?
            + added
                .required("directCount")?
                .as_f64()
                .ok_or_else(IvyError::internal)?;
        summary.insert("directCount", Json::Number(direct));
        summary.insert("questions", Json::Array(questions));
        Ok(summary)
    }
    async fn question_open(
        &self,
        tree: &Json,
        owner: &JsString,
        comments: &[Json],
    ) -> IvyResult<Vec<bool>> {
        let mut wanted = Vec::new();
        let mut lookups = Vec::with_capacity(comments.len());
        for comment in comments {
            let start = wanted.len();
            if comment.get("purpose").is_some_and(|v| v.is_str("question"))
                && comment.get("sourceTaskId").is_none_or(Json::is_null)
            {
                let id = comment
                    .required("commentId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?;
                if comment.get("nativeInput").is_some_and(|v| !v.is_null()) {
                    wanted.push((11, id.clone()));
                    lookups.push(start..wanted.len());
                    continue;
                }
                let recipient = comment.get("recipient");
                let reply = if recipient
                    .and_then(|v| v.get("kind"))
                    .is_some_and(|v| v.is_str("task"))
                {
                    (
                        9,
                        reply_identity(
                            recipient
                                .ok_or_else(IvyError::internal)?
                                .required("taskId")?
                                .as_js_string()
                                .ok_or_else(IvyError::internal)?,
                            id,
                        )?,
                    )
                } else {
                    (8, id.clone())
                };
                wanted.extend([reply, (10, id.clone())]);
                for request in comment
                    .required("requests")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                {
                    wanted.push((
                        2,
                        request
                            .required("requestId")?
                            .as_js_string()
                            .cloned()
                            .ok_or_else(IvyError::internal)?,
                    ));
                }
            }
            lookups.push(start..wanted.len());
        }
        let positions = if wanted.is_empty() {
            vec![]
        } else {
            self.comment_positions(tree, owner, &wanted).await?
        };
        comments
            .iter()
            .zip(lookups)
            .map(|(comment, range)| {
                let at = sequence(comment)?;
                if range.is_empty() {
                    return Ok(false);
                }
                let after = &positions[range];
                if after.len() == 1 {
                    return Ok(!after[0].is_some_and(|p| p > at));
                }
                Ok(!after[..2].iter().any(|p| p.is_some_and(|p| p > at))
                    && (after.len() == 2 || !after[2..].iter().all(|p| p.is_some_and(|p| p > at))))
            })
            .collect()
    }

    pub(crate) async fn retarget_question(
        &self,
        task: &Json,
        owner: &JsString,
        id: &Json,
    ) -> IvyResult<Option<Json>> {
        let Some(tree) = task
            .get("commentStore")
            .filter(|tree| tree.get("kind").is_some_and(|v| v.is_str("tree")))
        else {
            return Ok(ivy_sdk::open_task_questions(task, None)?
                .into_iter()
                .find(|c| ordinary_question(c) && c.get("commentId") == Some(id)));
        };
        let comments = self
            .selected_comments(task, owner, std::slice::from_ref(id))
            .await?;
        let open = self.question_open(tree, owner, &comments).await?;
        Ok(comments
            .into_iter()
            .zip(open)
            .find_map(|(comment, open)| (open && ordinary_question(&comment)).then_some(comment)))
    }

    /// Re-evaluate only changed questions against replies in the original root.
    /// A new recipient may close an open question, or reopen a repeated old ID.
    pub(crate) async fn retargeted_comment_summary(
        &self,
        task: &Json,
        owner: &JsString,
        changes: &[Json],
    ) -> IvyResult<(Json, Vec<Json>)> {
        use crate::engine_read::conversation::cross_agent_comment;
        let tree = task.required("commentStore")?;
        let ids = changes
            .iter()
            .map(|c| c.required("commentId").cloned())
            .collect::<IvyResult<Vec<_>>>()?;
        let original = self.selected_entries(task, owner, &ids).await?;
        require(
            !changes.is_empty() && original.len() == changes.len(),
            "task_board_comment_mismatch",
            "A recipient change needs all original entries of its selected identities.",
        )?;
        let mut summary = tree.required("summary")?.clone();
        let old_questions = summary
            .required("questions")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let changed_at = changes
            .iter()
            .map(sequence)
            .collect::<IvyResult<Vec<_>>>()?;
        require(
            changed_at.windows(2).all(|p| p[0] < p[1]),
            "task_board_comment_mismatch",
            "Recipient changes retain their original comment order.",
        )?;
        let mut direct = count(summary.required("directCount")?)? as i64;
        let mut entries = Vec::with_capacity(changes.len());
        for (entry, comment) in original.iter().zip(changes) {
            let old = entry.required("comment")?;
            let mut expected = old.clone();
            expected.insert("recipient", comment.required("recipient")?.clone());
            require(
                hash_json(&expected)? == hash_json(comment)?,
                "task_board_comment_mismatch",
                "A recipient change retains the original comment and sequence.",
            )?;
            direct += i64::from(!cross_agent_comment(comment, owner))
                - i64::from(!cross_agent_comment(old, owner));
            let mut entry = entry.clone();
            entry.insert("comment", comment.clone());
            entries.push(entry);
        }
        let open = self.question_open(tree, owner, changes).await?;
        let mut questions = old_questions
            .iter()
            .filter(|item| {
                item.get("sequence")
                    .and_then(Json::as_f64)
                    .is_none_or(|at| !changed_at.contains(&(at as usize)))
            })
            .cloned()
            .collect::<Vec<_>>();
        for (comment, open) in changes.iter().zip(open) {
            if open {
                questions.push(Json::object([
                    ("commentId", comment.required("commentId")?.clone()),
                    ("sequence", comment.required("sequence")?.clone()),
                    ("recipient", comment.required("recipient")?.clone()),
                    ("open", Json::Bool(true)),
                ]));
            }
        }
        questions.sort_by_key(|q| q.get("sequence").and_then(Json::as_f64).unwrap_or(0.0) as usize);
        require(
            direct >= 0,
            "task_board_comment_mismatch",
            "The direct comment count is invalid.",
        )?;
        summary.insert("directCount", Json::Number(direct as f64));
        summary.insert("questions", Json::Array(questions));
        Ok((summary, entries))
    }

    pub(crate) async fn comment_context(
        &self,
        task: &Json,
        id: &JsString,
        request: &Json,
    ) -> IvyResult<crate::engine_validation::CommentContext> {
        use crate::engine_validation::CommentContext;
        let Some(tree) = task
            .get("commentStore")
            .filter(|t| t.get("kind").is_some_and(|k| k.is_str("tree")))
        else {
            return CommentContext::from_task(task, request);
        };
        let text = |v: &Json| v.as_js_string().cloned().ok_or_else(IvyError::internal);
        let mut wanted = vec![(0, text(request.required("commentId")?)?)];
        if let Some(reply) = request.get("replyTo").filter(|v| !v.is_null()) {
            wanted.push((0, text(reply)?));
        }
        for resolution in request
            .get("resolvesQuestions")
            .and_then(Json::as_array)
            .unwrap_or(&[])
        {
            wanted.push((3, text(resolution)?));
        }
        for field in ["requests", "responses"] {
            for item in request
                .required(field)?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                let request_id = text(item.required("requestId")?)?;
                wanted.push((1, request_id.clone()));
                if field == "responses" {
                    wanted.push((2, request_id));
                }
            }
        }
        let positions = self.comment_positions(tree, id, &wanted).await?;
        let mut result = CommentContext {
            identity_used: positions[0].is_some(),
            reply_exists: request.get("replyTo").is_none_or(Json::is_null),
            resolutions: vec![],
            requests: std::collections::HashSet::new(),
            responses: std::collections::HashSet::new(),
        };
        for ((kind, identity), position) in wanted.iter().zip(positions) {
            let Some(position) = position else {
                continue;
            };
            if *kind == 0 {
                if request.get("replyTo").and_then(Json::as_js_string) == Some(identity) {
                    result.reply_exists = true;
                }
            } else if *kind == 3 {
                if request
                    .get("resolvesQuestions")
                    .and_then(Json::as_array)
                    .is_some_and(|ids| ids.contains(&Json::String(identity.clone())))
                {
                    let comment = self.comment_at(tree, id, position).await?;
                    require(
                        comment.get("commentId").and_then(Json::as_js_string) == Some(identity),
                        "task_board_comment_mismatch",
                        "The indexed comment identity differs from its current root.",
                    )?;
                    result.resolutions.push(comment);
                }
            } else if *kind == 1 {
                result.requests.insert(identity.clone());
            } else if *kind == 2 {
                result.responses.insert(identity.clone());
            }
        }
        Ok(result)
    }
    /// Only a complete index of this exact root can prove absence. Cold and
    /// oversized roots scan bounded pages and retain only requested matches.
    pub(crate) async fn comment_positions(
        &self,
        tree: &Json,
        task: &JsString,
        ids: &[Identity],
    ) -> IvyResult<Vec<Option<usize>>> {
        Ok(self
            .comment_projection(tree, task, ids, false, None)
            .await?
            .0)
    }
    pub(crate) async fn completion_comment_projection(
        &self,
        task: &Json,
        owner: &JsString,
        run: &JsString,
        inputs: &[Json],
    ) -> IvyResult<crate::operations::CompletionComments> {
        let Some(tree) = task
            .get("commentStore")
            .filter(|v| v.get("kind").is_some_and(|k| k.is_str("tree")))
        else {
            return crate::operations::completion_comments(task, run, inputs);
        };
        let mut identities = vec![(6, run.clone())];
        for input in inputs {
            identities.push((
                7,
                reply_identity(run, input.as_js_string().ok_or_else(IvyError::internal)?)?,
            ));
        }
        let positions = self.comment_positions(tree, owner, &identities).await?;
        let handoff = if let Some(position) = positions[0] {
            let comment = self.comment_at(tree, owner, position).await?;
            require(
                comment.get("purpose").is_some_and(|v| v.is_str("handoff"))
                    && comment
                        .get("run")
                        .and_then(|v| v.get("objectId"))
                        .and_then(Json::as_js_string)
                        == Some(run)
                    && sequence(&comment)? == position,
                "task_board_comment_mismatch",
                "The indexed handoff differs from its current root.",
            )?;
            Some(comment)
        } else {
            None
        };
        Ok(crate::operations::CompletionComments {
            handoff,
            answered: positions[1..].iter().any(Option::is_some),
        })
    }
    pub(crate) async fn queued_comment_ids(
        &self,
        task: &Json,
        owner: &JsString,
    ) -> IvyResult<Vec<Json>> {
        if let Some(tree) = task
            .get("commentStore")
            .filter(|tree| tree.get("kind").is_some_and(|v| v.is_str("tree")))
        {
            return Ok(self
                .comment_projection(tree, owner, &[], true, None)
                .await?
                .1);
        }
        task.required("commentDeliveries")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .filter(|receipt| receipt.get("state").is_some_and(|v| v.is_str("queued")))
            .map(|receipt| receipt.required("commentId").cloned())
            .collect()
    }
    async fn comment_projection(
        &self,
        tree: &Json,
        task: &JsString,
        ids: &[Identity],
        include_queued: bool,
        native_run: Option<(&JsString, Option<&JsString>)>,
    ) -> IvyResult<(Vec<Option<usize>>, Vec<Json>, NativeQuestions)> {
        let length = count(tree.required("length")?)?;
        let root = tree.required("root")?;
        require(
            root.is_null() && length == 0
                || !root.is_null() && length > 0 && count(root.required("length")?)? == length,
            "task_board_comment_mismatch",
            "The conversation index is incomplete.",
        )?;
        let index = Index {
            db: self.journal.connection,
            task: key(task)?,
            root: tree_key(tree)?,
            generation: std::cell::Cell::new(0),
        };
        if index.current(true)? {
            index.touch()?;
            return Ok((
                index.positions(ids)?,
                if include_queued {
                    index.queued_ids()?
                } else {
                    vec![]
                },
                native_run
                    .map(|(run, id)| index.native_questions(run, id))
                    .transpose()?
                    .unwrap_or_default(),
            ));
        }
        index.begin()?;
        let comments = super::persistent_reads::task_comments::TaskComments::new(
            std::sync::Arc::new(ivy_sdk::task_comments::RpcCommentSource::new(
                self.client.clone(),
            )),
            tokio::runtime::Handle::current(),
        );
        let result = async {
            let mut found = vec![None; ids.len()];
            let mut waiting = vec![];
            let mut native = vec![];
            for start in (0..length).step_by(BATCH) {
                let end = (start + BATCH).min(length);
                let entries = comments
                    .range(tree, task, Some(start), Some(end), None)
                    .await
                    .map_err(failure)?;
                require(
                    entries.len() == end - start,
                    "task_board_comment_mismatch",
                    "The conversation index is incomplete.",
                )?;
                for (offset, entry) in entries.iter().enumerate() {
                    let comment = entry.required("comment")?;
                    let position = sequence(comment)?;
                    require(
                        position == start + offset + 1,
                        "task_board_comment_mismatch",
                        "The conversation sequence is invalid.",
                    )?;
                    for identity in identities(comment)? {
                        for (at, wanted) in ids.iter().enumerate() {
                            if wanted == &identity
                                && (!matches!(wanted.0, 4 | 12) || found[at].is_none())
                            {
                                found[at] = Some(position);
                            }
                        }
                    }
                    if include_queued && queued(entry) {
                        waiting.push(comment.required("commentId")?.clone());
                    }
                    if native_run.is_some_and(|(run, id)| {
                        crate::native_comments::question_for_run(comment, run)
                            && id.is_none_or(|id| {
                                comment.get("commentId").and_then(Json::as_js_string) == Some(id)
                            })
                    }) {
                        native.push((
                            position,
                            comment
                                .required("commentId")?
                                .as_js_string()
                                .cloned()
                                .ok_or_else(IvyError::internal)?,
                        ));
                    }
                }
                index.add(&entries)?;
            }
            index.finish()?;
            Ok((found, waiting, native))
        }
        .await;
        comments.settled().await;
        result
    }
    /// This projection includes every native question for the Run, including
    /// questions already absent from the open-question summary. It shares the
    /// exact-root cache, completeness fence and byte budget with other lookups.
    pub(crate) async fn native_question_comments(
        &self,
        task: &Json,
        owner: &JsString,
        run_id: &JsString,
        comment_id: Option<&JsString>,
    ) -> IvyResult<Vec<Json>> {
        let Some(tree) = task
            .get("commentStore")
            .filter(|tree| tree.get("kind").is_some_and(|kind| kind.is_str("tree")))
        else {
            return Ok(task
                .required("comments")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .filter(|comment| crate::native_comments::question_for_run(comment, run_id))
                .filter(|comment| {
                    comment_id.is_none_or(|id| {
                        comment.get("commentId").and_then(Json::as_js_string) == Some(id)
                    })
                })
                .cloned()
                .collect());
        };
        let (_, _, positions) = self
            .comment_projection(tree, owner, &[], false, Some((run_id, comment_id)))
            .await?;
        let mut questions = Vec::with_capacity(positions.len());
        for (at, id) in positions {
            let comment = self.comment_at(tree, owner, at).await?;
            require(
                comment.get("commentId").and_then(Json::as_js_string) == Some(&id)
                    && sequence(&comment)? == at
                    && crate::native_comments::question_for_run(&comment, run_id),
                "task_board_comment_mismatch",
                "The indexed native question differs from its current root.",
            )?;
            questions.push(comment);
        }
        Ok(questions)
    }
    pub(crate) async fn comment_at(
        &self,
        tree: &Json,
        task: &JsString,
        sequence: usize,
    ) -> IvyResult<Json> {
        require(
            sequence > 0,
            "task_board_comment_mismatch",
            "The conversation sequence is invalid.",
        )?;
        let comments = super::persistent_reads::task_comments::TaskComments::new(
            std::sync::Arc::new(ivy_sdk::task_comments::RpcCommentSource::new(
                self.client.clone(),
            )),
            tokio::runtime::Handle::current(),
        );
        let result = comments
            .range(tree, task, Some(sequence - 1), Some(sequence), None)
            .await
            .map_err(failure);
        comments.settled().await;
        result?
            .into_iter()
            .next()
            .ok_or_else(IvyError::internal)?
            .required("comment")
            .cloned()
    }
    /// Select actual receipt-bearing entries. Repeated historical IDs require
    /// all matching leaves; the last occurrence alone is not their authority.
    pub(crate) async fn comment_entries(
        &self,
        task: &Json,
        owner: &JsString,
        ids: &[Json],
    ) -> IvyResult<Vec<Json>> {
        Ok(self
            .selected_entries(task, owner, ids)
            .await?
            .into_iter()
            .filter(|entry| {
                entry
                    .get("receipt")
                    .is_some_and(|receipt| !receipt.is_null())
            })
            .collect())
    }
    pub(crate) async fn selected_comments(
        &self,
        task: &Json,
        owner: &JsString,
        ids: &[Json],
    ) -> IvyResult<Vec<Json>> {
        if task
            .get("commentStore")
            .is_none_or(|tree| tree.get("kind").is_none_or(|kind| !kind.is_str("tree")))
        {
            return crate::operations::selected_task_comments(task, ids);
        }
        self.selected_entries(task, owner, ids)
            .await?
            .into_iter()
            .map(|entry| entry.required("comment").cloned())
            .collect()
    }
    async fn selected_entries(
        &self,
        task: &Json,
        owner: &JsString,
        ids: &[Json],
    ) -> IvyResult<Vec<Json>> {
        let Some(tree) = task
            .get("commentStore")
            .filter(|v| v.get("kind").is_some_and(|k| k.is_str("tree")))
        else {
            let receipts = task
                .required("commentDeliveries")?
                .as_array()
                .ok_or_else(IvyError::internal)?;
            return Ok(task
                .required("comments")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .filter(|comment| comment.get("commentId").is_some_and(|id| ids.contains(id)))
                .map(|comment| {
                    Json::object([
                        ("comment", comment.clone()),
                        (
                            "receipt",
                            receipts
                                .iter()
                                .find(|receipt| {
                                    receipt.get("commentId") == comment.get("commentId")
                                })
                                .cloned()
                                .unwrap_or(Json::Null),
                        ),
                    ])
                })
                .collect());
        };
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let wanted = ids
            .iter()
            .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
            .collect::<IvyResult<Vec<_>>>()?;
        let lookups = wanted
            .iter()
            .flat_map(|id| [(0, id.clone()), (4, id.clone())])
            .collect::<Vec<_>>();
        let positions = self.comment_positions(tree, owner, &lookups).await?;
        let mut repeated = false;
        let mut selected = Vec::new();
        for pair in positions.chunks_exact(2) {
            require(
                pair[0].is_some() == pair[1].is_some(),
                "task_board_comment_mismatch",
                "The conversation identity projection is incomplete.",
            )?;
            repeated |= pair[0] != pair[1];
            if let Some(at) = pair[0] {
                selected.push(at);
            }
        }
        selected.sort_unstable();
        selected.dedup();
        let reader = super::persistent_reads::task_comments::TaskComments::new(
            std::sync::Arc::new(ivy_sdk::task_comments::RpcCommentSource::new(
                self.client.clone(),
            )),
            tokio::runtime::Handle::current(),
        );
        let result = async {
            let mut found = vec![];
            let length = count(tree.required("length")?)?;
            let ranges: Box<dyn Iterator<Item = (usize, usize)>> = if repeated {
                Box::new(
                    (0..length)
                        .step_by(BATCH)
                        .map(move |at| (at, (at + BATCH).min(length))),
                )
            } else {
                Box::new(selected.into_iter().map(|at| (at - 1, at)))
            };
            for (start, end) in ranges {
                let batch = reader
                    .range(tree, owner, Some(start), Some(end), None)
                    .await
                    .map_err(failure)?;
                require(
                    batch.len() == end - start,
                    "task_board_comment_mismatch",
                    "The conversation index is incomplete.",
                )?;
                for entry in batch {
                    let id = entry
                        .required("comment")?
                        .required("commentId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?;
                    let matches = wanted.contains(id);
                    require(
                        repeated || matches,
                        "task_board_comment_mismatch",
                        "The indexed comment differs from its current root.",
                    )?;
                    if matches {
                        found.push(entry);
                    }
                }
            }
            Ok(found)
        }
        .await;
        reader.settled().await;
        result
    }
    /// Called only after constructing an append from the validated base root.
    /// Preparing a root is safe: readers still require exact root equality.
    pub(crate) fn extend_comment_index(
        &self,
        base: &Json,
        next: &Json,
        task: &JsString,
        entries: &[Json],
    ) -> IvyResult<()> {
        let index = Index {
            db: self.journal.connection,
            task: key(task)?,
            root: tree_key(base)?,
            generation: std::cell::Cell::new(0),
        };
        let owned = transaction(index.db, || {
            if !index.current(true)? {
                return Ok(false);
            }
            let generation = index
                .db
                .query_row(
                    "SELECT generation FROM comment_index_roots WHERE task_id=? AND root_key=?",
                    params![index.task, index.root],
                    |r| r.get(0),
                )
                .map_err(error)?;
            index.generation.set(generation);
            index
                .db
                .execute(
                    "UPDATE comment_index_roots SET complete=0 WHERE task_id=? AND root_key=?",
                    params![index.task, index.root],
                )
                .map_err(error)?;
            Ok(true)
        })?;
        if !owned {
            return Ok(());
        }
        index.add(entries)?;
        index.db.execute("UPDATE comment_index_roots SET root_key=?,complete=1 WHERE task_id=? AND root_key=? AND complete=0 AND generation=?",params![tree_key(next)?,index.task,index.root,index.generation.get()]).map_err(error)?;
        Ok(())
    }
}
