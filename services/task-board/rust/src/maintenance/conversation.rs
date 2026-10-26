//! Publish through the same immutable conversation and reference-page writers
//! as ordinary TaskBoard writes; only the owning Task head is replaced by CAS.
use super::*;
use crate::engine_read::read_failure::ReadCause;
use crate::journal_store::{
    comment_writes::tree::save_node,
    mutation_ports::{LocalMutationSource, WriteFailure},
    persistent_reads::comment_port::CommentPort,
    reference_writes::{task_attachment_pins, task_references},
};

struct Requests<'a>(&'a dyn RpcClient);
impl LocalMutationSource<IvyError> for Requests<'_> {
    async fn request_owned(
        &self,
        method: &str,
        params: Option<Json>,
    ) -> Result<Json, WriteFailure<IvyError>> {
        self.0
            .request(
                method,
                params.ok_or_else(IvyError::internal)?,
                RequestOptions::default(),
            )
            .await
            .map_err(Into::into)
    }
}
fn failure(error: WriteFailure<IvyError>) -> IvyError {
    match error.cause() {
        ReadCause::Source(error) | ReadCause::Ivy(error) => error.clone(),
    }
}
impl<S: MaintenanceStore> Retention<'_, S> {
    pub(super) async fn publish(
        &self,
        id: &str,
        previous: &Json,
        task: &Json,
        scope: &str,
        attachments: bool,
    ) -> IvyResult<()> {
        let version = self.store.contracts().version("task-board/task")?;
        require(
            ["2.2.0", "2.3.0", "3.0.0", "3.1.0"].contains(&version.as_str()),
            "task_board_contract_mismatch",
            "Maintenance requires an indexed conversation.",
        )?;
        self.store
            .contracts()
            .validate_record("task-board/task", &version.clone().into(), task)?;
        crate::journal_store::persistent_reads::task_comments::validate_task_content(
            task,
            &id.into(),
        )?;
        let current = self
            .request("objects.read", Json::object([("objectId", id.into())]))
            .await?;
        require(
            pin(&current)? == pin(previous)?,
            "revision_conflict",
            "The Task changed before maintenance publication.",
        )?;
        let client = self.store.client();
        let source = Requests(client.as_ref());
        let source_failure = |error: IvyError| WriteFailure::<IvyError>::from(error);
        let comments = CommentPort {
            comments: &self.comments,
            source_failure: &source_failure,
        };
        let identity = |step: Json| async move {
            self.store
                .mutation(&format!("{scope}:{}", step.as_str()?))
                .await
                .map(Json::String)
                .map_err(WriteFailure::<IvyError>::from)
        };
        let owner = Json::from(id);
        let node_version = Json::from(if version.starts_with('3') {
            "2.0.0"
        } else {
            "1.0.0"
        });
        let mut receipts = HashMap::new();
        for receipt in items(task, "commentDeliveries")? {
            receipts.insert(hash_json(receipt.required("commentId")?)?, receipt);
        }
        let mut level = Vec::new();
        for (index, comment) in items(task, "comments")?.iter().enumerate() {
            require(
                comment.get("sequence") == Some(&Json::Number((index + 1) as f64)),
                "task_board_comment_mismatch",
                "Maintenance preserves the contiguous conversation sequence.",
            )?;
            let receipt = receipts
                .get(&hash_json(comment.required("commentId")?)?)
                .copied()
                .cloned()
                .unwrap_or(Json::Null);
            let node = Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("taskId", owner.clone()),
                ("kind", "comment".into()),
                (
                    "entry",
                    Json::object([("comment", comment.clone()), ("receipt", receipt)]),
                ),
            ]);
            let saved = save_node(
                &source,
                &identity,
                &comments,
                owner.clone(),
                node,
                node_version.clone(),
            )
            .await
            .map_err(failure)?;
            level.push(Json::object([
                ("object", saved),
                ("length", Json::Number(1.0)),
            ]));
        }
        while level.len() > 1 {
            let mut parents = Vec::new();
            for children in level.chunks(64) {
                let node = Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("taskId", owner.clone()),
                    ("kind", "page".into()),
                    ("children", Json::Array(children.to_vec())),
                ]);
                let saved = save_node(
                    &source,
                    &identity,
                    &comments,
                    owner.clone(),
                    node,
                    node_version.clone(),
                )
                .await
                .map_err(failure)?;
                let length = children
                    .iter()
                    .map(|c| {
                        c.required("length")
                            .and_then(|v| v.as_f64().ok_or_else(IvyError::internal))
                    })
                    .collect::<IvyResult<Vec<_>>>()?
                    .iter()
                    .sum();
                parents.push(Json::object([
                    ("object", saved),
                    ("length", Json::Number(length)),
                ]));
            }
            level = parents;
        }
        let mut tree = Json::object([
            ("kind", "tree".into()),
            ("root", level.into_iter().next().unwrap_or(Json::Null)),
            (
                "length",
                Json::Number(items(task, "comments")?.len() as f64),
            ),
        ]);
        if version != "2.2.0" {
            tree.insert(
                "summary",
                crate::journal_store::persistent_reads::comment_projections::current_summary(
                    task,
                    &id.into(),
                )?,
            );
        }
        let mut named = previous
            .required("revision")?
            .required("references")?
            .clone();
        if attachments {
            named = Json::Object(
                named
                    .as_object()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .filter(|(key, _)| {
                        !key.to_utf8().is_ok_and(|key| {
                            key.starts_with("attachment-")
                                || key.starts_with("reference-page-")
                                || key == "conversation"
                        })
                    })
                    .cloned()
                    .collect(),
            );
        }
        if let Some(root) = tree.get("root").filter(|v| !v.is_null()) {
            named.insert("conversation", root.required("object")?.clone());
        } else {
            named.remove("conversation");
        }
        let references = if attachments {
            task_references(
                &source,
                &identity,
                &owner,
                &named,
                &task_attachment_pins(task).map_err(failure)?,
            )
            .await
            .map_err(failure)?
        } else {
            named
        };
        let mut stored = task.clone();
        stored.insert("comments", Json::Array(vec![]));
        stored.insert("commentDeliveries", Json::Array(vec![]));
        stored.insert("commentStore", tree);
        self.store.contracts().validate_record(
            "task-board/task",
            &version.clone().into(),
            &stored,
        )?;
        let mutation = self
            .store
            .mutation(&format!("{scope}:task:{}", hash_json(&pin(previous)?)?))
            .await?;
        self.request(
            "objects.write",
            Json::object([
                ("objectId", owner),
                (
                    "expectedRevision",
                    previous.required("revision")?.required("revision")?.clone(),
                ),
                (
                    "expectedArchived",
                    previous
                        .required("object")?
                        .required("effectivelyArchived")?
                        .clone(),
                ),
                ("contractVersion", version.into()),
                ("mutationId", Json::String(mutation)),
                ("references", references),
                (
                    "content",
                    Json::object([("encoding", "json".into()), ("value", stored)]),
                ),
            ]),
        )
        .await?;
        Ok(())
    }
}
