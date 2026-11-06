//! In-process Claude protocol lifetime and its exact private vendor process tree.
use crate::{
    claude::{
        error::AdapterError,
        runtime::{self, Launch},
    },
    native_connection::{AgentNativeConnection, NativeCompletion, NativeConnectionSettings},
    native_handshake::{AgentHandshake, initialize_agent},
};
use ivy_contracts::{IvyError, IvyResult, Outcome};
use ivy_host_runtime::{
    codex_rpc::{Events, NativeRpc},
    process::{ProcessResult, owned_process_tree_stopped},
};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::{oneshot, watch};

fn error(value: AdapterError) -> IvyError {
    IvyError::new_js(value.name.into(), value.message).with_outcome(
        if value.name == "outcome_unknown" {
            Outcome::Unknown
        } else {
            Outcome::NotExecuted
        },
    )
}
pub fn connect_agent_claude(
    launch: Launch,
    settings: NativeConnectionSettings,
    events: Events,
) -> impl Future<Output = IvyResult<AgentNativeConnection>> + Send {
    let (mut ready, response) = oneshot::channel();
    tokio::spawn(async move {
        let setup_failed = events.on_close.clone();
        let (rpc, mut outgoing, incoming) =
            match NativeRpc::start_frames(settings.catalog.clone(), events) {
                Ok(parts) => parts,
                Err(error) => {
                    let _ = setup_failed("native_protocol_setup_failed".into()).await;
                    let _ = ready.send(Err(error));
                    return;
                }
            };
        let runtime = runtime::start(
            launch,
            Arc::new(move |_, frame| {
                incoming
                    .try_send(frame)
                    .map_err(|e| AdapterError::typed("native_transport_error", e.message_js()))
            }),
        )
        .await;
        let mut runtime = match runtime {
            Ok(runtime) => runtime,
            Err(failure) => {
                rpc.close("native_launch_failed").await;
                let _ = ready.send(Err(error(failure)));
                return;
            }
        };
        let handle = runtime.handle.clone();
        let mut forward = tokio::spawn(async move {
            while let Some(frame) = outgoing.recv().await {
                handle.receive_frame("manager".into(), frame).await?;
            }
            Ok::<_, AdapterError>(())
        });
        let mut finished = None;
        let initialized = tokio::select! {
            result = initialize_agent(&rpc, AgentHandshake {
                client_version: &settings.client_version, native_version: &settings.native_version,
                expected_home: &settings.expected_home, mode: settings.mode, daemon: None,
            }) => result,
            result = &mut runtime.owner => {
                finished = Some(result);
                Err(IvyError::new("native_initialization_failed", "Claude owner closed during initialization."))
            },
            _ = &mut forward => Err(IvyError::new("native_initialization_failed", "Claude input owner closed during initialization.")),
            _ = ready.closed() => Err(IvyError::new("native_owner_stopping", "Native connection startup was cancelled.")),
        };
        let (stop, mut stopping) = watch::channel(false);
        let (completed, completion) = watch::channel(None);
        let mut startup_error = None;
        match initialized {
            Ok(initialized) => {
                let connection = AgentNativeConnection {
                    rpc: rpc.clone(),
                    catalog: settings.catalog.clone(),
                    epoch: settings.epoch.clone(),
                    initialized: initialized.initialized,
                    native_home: initialized.native_home,
                    actual_server_version: initialized.actual_server_version,
                    mode: settings.mode,
                    home_source: settings.home_source,
                    owns_native_home: settings.owns_native_home,
                    executable: settings.executable.clone(),
                    windows_shell: settings.windows_shell.clone(),
                    socket_path: None,
                    daemon: None,
                    launcher_pid: runtime.process.pid,
                    completion: NativeCompletion(completion),
                    stop,
                };
                if ready.send(Ok(connection)).is_ok() {
                    tokio::select! {
                        _ = stopping.changed() => {},
                        _ = rpc.closed() => {},
                        result = &mut runtime.owner => { finished = Some(result); },
                        _ = &mut forward => {},
                    }
                }
            }
            Err(failure) => startup_error = Some((ready, failure)),
        }
        forward.abort();
        // Let the protocol owner finish its current effects and durable turn
        // cleanup before closing its sink. Closing the sink first can race an
        // already accepted SDK event and turn a requested stop into a failure.
        runtime.handle.stop().await;
        rpc.close("native_owner_stopping").await;
        let cleanup = match finished {
            Some(result) => result,
            None => runtime.owner.await,
        }
        .map_err(|_| {
            IvyError::new(
                "outcome_unknown",
                "Claude owner did not report its final state.",
            )
            .with_outcome(Outcome::Unknown)
        })
        .and_then(|result| result.map_err(|failure| error(failure).with_outcome(Outcome::Unknown)));
        // Also covers a failed protocol/bridge owner: retaining its process
        // handle must not turn an owner panic into an unbounded completion wait.
        let mut process_result = match runtime.process.stop(Duration::from_secs(2)).await {
            Ok(result) => result,
            Err(failure) => {
                if let Some((ready, _)) = startup_error {
                    let _ = ready.send(Err(failure.clone()));
                }
                completed.send_replace(Some(Err(failure)));
                return;
            }
        };
        if !owned_process_tree_stopped(&runtime.process)
            .await
            .unwrap_or(false)
        {
            process_result = Arc::new(ProcessResult {
                error_code: Some("outcome_unknown".into()),
                ..(*process_result).clone()
            });
        }
        let evidence = (settings.record_stop)(
            settings.epoch,
            &runtime.process,
            settings.mode,
            process_result.clone(),
        )
        .await;
        let stopped = evidence.and(cleanup).map(|()| process_result);
        if let Some((ready, failure)) = startup_error {
            let _ = ready.send(Err(stopped.err().unwrap_or(failure)));
        } else {
            completed.send_replace(Some(stopped));
        }
    });
    async move { response.await.map_err(|_| IvyError::internal())? }
}
