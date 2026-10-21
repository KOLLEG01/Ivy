//! Isolated original-library auth boundary. Not a socket or production binding.
//! Values stay opaque; the existing account owner supplies operation identities.
use super::auth::{AuthMutation, AuthStore};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    json::{parse, stringify},
};
use ivy_host_runtime::{
    exec_file::SpawnOptions,
    node_runtime::NodeRuntime,
    portable_native_files::NativeHelperLayout,
    process::{self, Command, LaunchContext, OwnedProcess},
};
use ivy_sdk::service::ServiceFuture;
use ivy_store::sqlite::OwnerLock;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStdin, ChildStdout},
    sync::{Mutex as AsyncMutex, watch},
    task::JoinHandle,
};

const FRAME_BYTES: usize = 65536;
const CHUNK_BYTES: usize = 45 * 1024;
const WORKER: &str = "services/chat-bridge/rust/node/auth-worker.mjs";

fn unknown() -> IvyError {
    IvyError::new(
        "outcome_unknown",
        "Authentication generation is fenced; reconcile its original operation.",
    )
    .with_outcome(Outcome::Unknown)
}
fn storage() -> IvyError {
    IvyError::new(
        "storage_unavailable",
        "Authentication storage did not confirm completion.",
    )
    .with_outcome(Outcome::Unknown)
}

/// Reuses the original executor-lock.sqlite pathname and kernel lock mechanism.
/// The caller creates/protects the account directories before admission.
pub struct AuthAccount {
    store: Option<AuthStore>,
    lock: Option<OwnerLock>,
    release: bool,
}
impl AuthAccount {
    pub fn acquire(auth: &Path, executor_lock: &Path) -> IvyResult<Self> {
        let lock = OwnerLock::acquire(executor_lock, true).map_err(|_| {
            IvyError::new(
                "executor_already_running",
                "Another owner holds the account lock.",
            )
        })?;
        let store = AuthStore::open(auth).map_err(|_| storage())?;
        Ok(Self {
            store: Some(store),
            lock: Some(lock),
            release: true,
        })
    }
    fn store(&self) -> &AuthStore {
        self.store.as_ref().expect("owned auth store")
    }
    fn release(&mut self) -> IvyResult<()> {
        self.store
            .take()
            .ok_or_else(unknown)?
            .close()
            .map_err(|_| storage())?;
        self.lock.take();
        self.release = true;
        Ok(())
    }
}
impl Drop for AuthAccount {
    fn drop(&mut self) {
        // Dropping a future/handle cannot prove the child has stopped. Keep the
        // kernel fence until this Rust process dies if explicit retirement or
        // original-operation reconciliation was not established. No IPC table.
        if !self.release {
            if let Some(store) = self.store.take() {
                std::mem::forget(store);
            }
            if let Some(lock) = self.lock.take() {
                std::mem::forget(lock);
            }
        }
    }
}

pub struct AuthLaunch {
    pub node: NodeRuntime,
    pub artifact_root: PathBuf,
    pub helper_layout: NativeHelperLayout,
    /// Explicit host/test workload environment; ambient Node options are not added.
    pub environment: BTreeMap<OsString, OsString>,
    pub deadline: Duration,
}

type Retirement = watch::Receiver<Option<IvyResult<()>>>;
struct AccountAuthInner {
    node: AsyncMutex<AuthNode>,
    retirement: Mutex<Option<Retirement>>,
}

/// Shared account lifetime; callers still supply and retain operation identities.
/// A retirement observer never owns or cancels the actual child retirement.
#[derive(Clone)]
pub struct AccountAuth(Arc<AccountAuthInner>);
impl AccountAuth {
    pub async fn launch(
        account: AuthAccount,
        generation: u64,
        init_operation: JsString,
        launch: AuthLaunch,
    ) -> IvyResult<Self> {
        let node = AuthNode::launch(account, generation, init_operation, launch).await?;
        Ok(Self(Arc::new(AccountAuthInner {
            node: AsyncMutex::new(node),
            retirement: Mutex::new(None),
        })))
    }

    pub fn call(
        &self,
        operation: JsString,
        method: AuthMethod,
        encoded_arguments: String,
    ) -> ServiceFuture<AuthReply> {
        let owner = self.clone();
        Box::pin(async move {
            owner.admit()?;
            let mut node = owner.0.node.lock().await;
            // A queued call may acquire this guard after retire was invoked.
            owner.admit()?;
            node.call(operation, method, encoded_arguments).await
        })
    }

    fn admit(&self) -> IvyResult<()> {
        if self.0.retirement.lock().unwrap().is_some() {
            Err(
                IvyError::new("auth_retiring", "Authentication account is retiring.")
                    .with_outcome(Outcome::NotExecuted),
            )
        } else {
            Ok(())
        }
    }

    pub fn retire(&self) -> ServiceFuture<()> {
        let mut retirement = self.0.retirement.lock().unwrap();
        let receive = if let Some(receive) = &*retirement {
            receive.clone()
        } else {
            let (done, receive) = watch::channel(None);
            // Set admission synchronously, even for a never-polled observer.
            *retirement = Some(receive.clone());
            let owner = self.clone();
            tokio::spawn(async move {
                let result = owner.0.node.lock().await.shutdown().await;
                done.send_replace(Some(result));
            });
            receive
        };
        Box::pin(account_retired(receive))
    }
}

async fn account_retired(mut receive: Retirement) -> IvyResult<()> {
    loop {
        if let Some(result) = receive.borrow_and_update().clone() {
            return result;
        }
        // A lost cleanup owner is not proof of stop or ownership release.
        receive.changed().await.map_err(|_| unknown())?;
    }
}

#[derive(Clone, Copy)]
pub enum AuthMethod {
    InjectE2ESession,
    EncryptMessage,
    DecryptMessage,
    ProcessSenderKeyDistributionMessage,
    EncryptGroupMessage,
    DecryptGroupMessage,
    GetKeys,
    SetKeys,
    Credentials,
    SaveCredentials,
}
impl AuthMethod {
    fn name(self) -> &'static str {
        match self {
            Self::InjectE2ESession => "injectE2ESession",
            Self::EncryptMessage => "encryptMessage",
            Self::DecryptMessage => "decryptMessage",
            Self::ProcessSenderKeyDistributionMessage => "processSenderKeyDistributionMessage",
            Self::EncryptGroupMessage => "encryptGroupMessage",
            Self::DecryptGroupMessage => "decryptGroupMessage",
            Self::GetKeys => "getKeys",
            Self::SetKeys => "setKeys",
            Self::Credentials => "credentials",
            Self::SaveCredentials => "saveCredentials",
        }
    }
}

/// Deliberately no Debug: an encoded result can contain auth keys or plaintext.
pub enum AuthReply {
    Value(Option<String>),
    LibraryError(String),
}

#[derive(Clone, Copy, Default)]
pub struct AuthStatistics {
    pub sent_frames: u64,
    pub received_frames: u64,
    pub maximum_frame_bytes: usize,
    pub committed_batches: u64,
}

struct Wire {
    input: ChildStdout,
    output: ChildStdin,
    generation: String,
    sent: u64,
    received: u64,
    stats: AuthStatistics,
}
impl Wire {
    async fn send(&mut self, request: &str, value: Json) -> IvyResult<()> {
        let bytes = stringify(&value, usize::MAX)
            .map_err(|_| unknown())?
            .into_bytes();
        self.sent = self.sent.checked_add(1).ok_or_else(unknown)?;
        for (part, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
            let frame = Json::object([
                ("v", Json::Number(1.0)),
                ("g", self.generation.as_str().into()),
                ("r", request.into()),
                ("m", self.sent.to_string().into()),
                ("p", Json::Number(part as f64)),
                ("e", ((part + 1) * CHUNK_BYTES >= bytes.len()).into()),
                ("b", STANDARD.encode(chunk).into()),
            ]);
            let frame = stringify(&frame, FRAME_BYTES - 4)
                .map_err(|_| unknown())?
                .into_bytes();
            self.output
                .write_all(&(frame.len() as u32).to_be_bytes())
                .await
                .map_err(|_| unknown())?;
            self.output.write_all(&frame).await.map_err(|_| unknown())?;
            self.stats.sent_frames += 1;
            self.stats.maximum_frame_bytes = self.stats.maximum_frame_bytes.max(frame.len());
        }
        self.output.flush().await.map_err(|_| unknown())
    }
    async fn receive(&mut self, request: &str) -> IvyResult<Json> {
        let message = self
            .received
            .checked_add(1)
            .ok_or_else(unknown)?
            .to_string();
        let mut bytes = Vec::new();
        for part in 0_usize.. {
            let length = self.input.read_u32().await.map_err(|_| unknown())? as usize;
            if length == 0 || length > FRAME_BYTES - 4 {
                return Err(unknown());
            }
            let mut frame = vec![0; length];
            self.input
                .read_exact(&mut frame)
                .await
                .map_err(|_| unknown())?;
            self.stats.received_frames += 1;
            self.stats.maximum_frame_bytes = self.stats.maximum_frame_bytes.max(length);
            let frame = parse(
                std::str::from_utf8(&frame).map_err(|_| unknown())?,
                FRAME_BYTES,
            )
            .map_err(|_| unknown())?;
            if frame.get("v").and_then(Json::as_f64) != Some(1.0)
                || frame.get("g").and_then(Json::as_js_string)
                    != Some(&JsString::from(self.generation.as_str()))
                || frame.get("r").and_then(Json::as_js_string) != Some(&JsString::from(request))
                || frame.get("m").and_then(Json::as_js_string)
                    != Some(&JsString::from(message.as_str()))
                || frame.get("p").and_then(Json::as_f64) != Some(part as f64)
            {
                return Err(unknown());
            }
            let encoded = frame
                .required("b")
                .and_then(Json::as_str)
                .map_err(|_| unknown())?;
            let chunk = STANDARD.decode(&encoded).map_err(|_| unknown())?;
            if chunk.len() > CHUNK_BYTES || STANDARD.encode(&chunk) != encoded {
                return Err(unknown());
            }
            bytes.extend(chunk);
            match frame.get("e").and_then(Json::as_bool) {
                Some(true) => {
                    self.received += 1;
                    return parse(
                        std::str::from_utf8(&bytes).map_err(|_| unknown())?,
                        usize::MAX,
                    )
                    .map_err(|_| unknown());
                }
                Some(false) => {}
                None => return Err(unknown()),
            }
        }
        Err(unknown())
    }
}

/// Exactly one child owns this account/generation. &mut self admits one command
/// and the worker serializes callbacks; no detached caller retry/crypto loop.
pub struct AuthNode {
    account: AuthAccount,
    process: OwnedProcess,
    wire: Wire,
    diagnostics: JoinHandle<()>,
    deadline: Duration,
    request: u64,
    fenced: bool,
    retired: bool,
    unknown_operation: Option<JsString>,
}

impl AuthNode {
    pub async fn launch(
        mut account: AuthAccount,
        generation: u64,
        operation_id: JsString,
        launch: AuthLaunch,
    ) -> IvyResult<Self> {
        if launch.deadline.is_zero() {
            return Err(IvyError::new(
                "invalid_arguments",
                "Auth operations need a finite deadline.",
            ));
        }
        let options = SpawnOptions::for_component(&launch.artifact_root, launch.helper_layout)?;
        let worker = ivy_host_runtime::paths::realpath(&launch.artifact_root.join(WORKER))
            .map_err(|_| unknown())?;
        if !worker.is_file()
            || !ivy_host_runtime::paths::inside(&launch.artifact_root, &worker)
                .map_err(|_| unknown())?
        {
            return Err(IvyError::new(
                "artifact_missing",
                "Authentication worker is outside its admitted artifact.",
            ));
        }
        let creds = account
            .store()
            .get("creds", "default")
            .map_err(|_| storage())?;
        // From here even cancellation/lost launch acknowledgement retains the fence.
        account.release = false;
        let mut child = match process::spawn(
            &Command {
                executable: "$node".into(),
                args: vec![worker.into_os_string(), generation.to_string().into()],
                cwd: None,
                timeout_ms: 0,
            },
            &launch.artifact_root,
            &BTreeMap::from([("$node".into(), launch.node.executable)]),
            LaunchContext {
                environment: launch.environment,
                job_launcher: options.job_launcher,
                raw_output: true,
                ..Default::default()
            },
        )
        .await
        {
            Ok(child) => child,
            Err(error) => {
                if error.outcome == Outcome::NotExecuted {
                    account.release = true;
                }
                return Err(error);
            }
        };
        let diagnostics = child.stderr.take().ok_or_else(unknown)?;
        let diagnostics = tokio::spawn(async move {
            let mut pipe = diagnostics;
            let mut bytes = [0; 8192];
            // Dependency errors can include keys. Drain without retaining arguments.
            while pipe.read(&mut bytes).await.is_ok_and(|length| length != 0) {}
        });
        let wire = Wire {
            input: child.stdout.take().ok_or_else(unknown)?,
            output: child.stdin.take().ok_or_else(unknown)?,
            generation: generation.to_string(),
            sent: 0,
            received: 0,
            stats: AuthStatistics::default(),
        };
        let mut node = Self {
            account,
            process: child,
            wire,
            diagnostics,
            deadline: launch.deadline,
            request: 0,
            fenced: false,
            retired: false,
            unknown_operation: None,
        };
        let result = node
            .exchange(
                operation_id,
                Json::object([
                    ("kind", "init".into()),
                    ("creds", creds.map(Json::from).unwrap_or(Json::Null)),
                ]),
            )
            .await;
        match result {
            Ok(AuthReply::Value(Some(_))) => Ok(node),
            _ => {
                let _ = node.shutdown().await;
                Err(unknown())
            }
        }
    }
    pub fn pid(&self) -> u32 {
        self.process.pid
    }
    pub fn statistics(&self) -> AuthStatistics {
        self.wire.stats
    }
    pub fn unknown_operation(&self) -> Option<&JsString> {
        self.unknown_operation.as_ref()
    }
    pub fn is_fenced(&self) -> bool {
        self.fenced
    }
    pub fn ownership_released(&self) -> bool {
        self.account.release
    }

    /// The existing owner retains this identity in its journal before calling.
    /// A dropped/expired future leaves fenced=true and the original identity.
    pub async fn call(
        &mut self,
        operation_id: JsString,
        method: AuthMethod,
        encoded_arguments: String,
    ) -> IvyResult<AuthReply> {
        self.exchange(
            operation_id,
            Json::object([
                ("kind", "call".into()),
                ("method", method.name().into()),
                ("arguments", encoded_arguments.into()),
            ]),
        )
        .await
    }
    async fn exchange(
        &mut self,
        operation_id: JsString,
        mut command: Json,
    ) -> IvyResult<AuthReply> {
        if self.fenced || self.retired || self.account.release {
            return Err(unknown());
        }
        self.fenced = true;
        self.unknown_operation = Some(operation_id.clone());
        self.request = self.request.checked_add(1).ok_or_else(unknown)?;
        command.insert("operationId", Json::String(operation_id.clone()));
        let deadline = self.deadline;
        let result = tokio::time::timeout(deadline, self.exchange_inner(operation_id, command))
            .await
            .unwrap_or_else(|_| Err(unknown()));
        if result.is_ok() {
            self.fenced = false;
            self.unknown_operation = None;
        }
        result
    }
    async fn exchange_inner(
        &mut self,
        operation_id: JsString,
        command: Json,
    ) -> IvyResult<AuthReply> {
        let request = self.request.to_string();
        let stopping =
            command.get("kind").and_then(Json::as_js_string) == Some(&JsString::from("stop"));
        self.wire.send(&request, command).await?;
        let mut callback = 0_u64;
        loop {
            let message = self.wire.receive(&request).await?;
            let kind = message
                .required("kind")
                .and_then(Json::as_str)
                .map_err(|_| unknown())?;
            match kind.as_str() {
                "get" | "set" => {
                    if stopping {
                        return Err(unknown());
                    }
                    callback = callback.checked_add(1).ok_or_else(unknown)?;
                    if message.get("callback").and_then(Json::as_js_string)
                        != Some(&JsString::from(callback.to_string()))
                    {
                        return Err(unknown());
                    }
                    let entries = message
                        .required("entries")
                        .map_err(|_| unknown())?
                        .as_array()
                        .ok_or_else(unknown)?;
                    let response = if kind == "get" {
                        let mut rows = Vec::new();
                        for entry in entries {
                            let entry = entry
                                .as_array()
                                .filter(|entry| entry.len() == 2)
                                .ok_or_else(unknown)?;
                            let category = sql_text(&entry[0])?;
                            let id = sql_text(&entry[1])?;
                            rows.push(
                                self.account
                                    .store()
                                    .get(&category, &id)
                                    .map_err(|_| storage())?
                                    .map(Json::from)
                                    .unwrap_or(Json::Null),
                            );
                        }
                        Json::object([("kind", "rows".into()), ("rows", Json::Array(rows))])
                    } else {
                        // Validate and reassemble the entire supplied batch before
                        // beginning its one transaction; never mutate per frame.
                        let mut values = Vec::new();
                        for entry in entries {
                            let entry = entry
                                .as_array()
                                .filter(|entry| entry.len() == 3)
                                .ok_or_else(unknown)?;
                            values.push((
                                sql_text(&entry[0])?,
                                sql_text(&entry[1])?,
                                if entry[2].is_null() {
                                    None
                                } else {
                                    Some(sql_text(&entry[2])?)
                                },
                            ));
                        }
                        self.account
                            .store
                            .as_mut()
                            .ok_or_else(unknown)?
                            .set_batch(
                                &values
                                    .iter()
                                    .map(|(category, id, value)| AuthMutation {
                                        category,
                                        id,
                                        value: value.as_deref(),
                                    })
                                    .collect::<Vec<_>>(),
                            )
                            .map_err(|_| storage())?;
                        self.wire.stats.committed_batches += 1;
                        Json::object([("kind", "committed".into())])
                    };
                    let mut response = response;
                    response.insert("callback", callback.to_string().into());
                    self.wire.send(&request, response).await?;
                }
                "result" | "error" | "stopped" => {
                    if (kind == "stopped" && !stopping) || (kind == "result" && stopping) {
                        return Err(unknown());
                    }
                    if message.get("operationId").and_then(Json::as_js_string)
                        != Some(&operation_id)
                    {
                        return Err(unknown());
                    }
                    return match kind.as_str() {
                        "result" => Ok(AuthReply::Value(
                            match message.required("result").map_err(|_| unknown())? {
                                Json::Null => None,
                                value => Some(value.as_str().map_err(|_| unknown())?),
                            },
                        )),
                        "stopped" => Ok(AuthReply::Value(None)),
                        _ => {
                            let name = message
                                .required("name")
                                .and_then(Json::as_str)
                                .map_err(|_| unknown())?;
                            if ![
                                "TypeError",
                                "SessionError",
                                "MessageCounterError",
                                "PreKeyError",
                                "Error",
                            ]
                            .contains(&name.as_str())
                            {
                                return Err(unknown());
                            }
                            Ok(AuthReply::LibraryError(name))
                        }
                    };
                }
                _ => return Err(unknown()),
            }
        }
    }

    /// No implicit crypto replay or child restart. Unresolved operations retain
    /// account ownership even after a confirmed child stop, until the original
    /// Rust owner is reconciled/retired. Unknown process stop never releases it.
    pub async fn shutdown(&mut self) -> IvyResult<()> {
        if self.account.release {
            return Ok(());
        }
        if !self.retired {
            let stop_identity = self.unknown_operation.clone().unwrap_or_else(|| {
                JsString::from(format!(
                    "auth-stop:{}:{}",
                    self.wire.generation, self.request
                ))
            });
            let mut acknowledged = false;
            if !self.fenced {
                let reply = self
                    .exchange(
                        stop_identity.clone(),
                        Json::object([("kind", "stop".into())]),
                    )
                    .await;
                acknowledged = matches!(reply, Ok(AuthReply::Value(None)));
            }
            // Cancellation or any error in the native stop/retirement/drain
            // awaits below must also fence later calls and preserve identity.
            self.fenced = true;
            self.unknown_operation.get_or_insert(stop_identity.clone());
            let _ = self.wire.output.shutdown().await;
            let completion = if !acknowledged {
                None
            } else {
                tokio::time::timeout(self.deadline, self.process.completion.wait())
                    .await
                    .ok()
            };
            let result = match completion {
                Some(result) => result,
                None => self.process.stop(Duration::ZERO).await?,
            };
            if result.error_code.is_some()
                || !process::owned_process_tree_stopped(&self.process).await?
            {
                return Err(unknown());
            }
            self.retired = true;
            if !matches!(
                tokio::time::timeout(self.deadline, &mut self.diagnostics).await,
                Ok(Ok(()))
            ) {
                return Err(unknown());
            }
            if acknowledged {
                self.fenced = false;
                self.unknown_operation = None;
            }
        }
        if self.fenced {
            return Err(unknown());
        }
        self.account.release()
    }
}

fn sql_text(value: &Json) -> IvyResult<String> {
    // Node SQLite binds JS strings as UTF-8 (isolated surrogates become U+FFFD).
    // Encoded BufferJSON value text itself contains escaped surrogate code units.
    value
        .as_js_string()
        .map(|value| String::from_utf16_lossy(&value.0))
        .ok_or_else(unknown)
}
