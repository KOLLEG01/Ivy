//! Local host time replies. Approval, auth and tool inputs stay caller-owned.
use crate::native_journal::{Identity, NativeJournal};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, hash::hash_json, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::codex_rpc::{HookFuture, NativeRpc};
use ivy_sdk::native_evidence::native_instant;
use std::sync::{Arc, Mutex};

pub const NATIVE_CLOCK_PRINCIPAL: &str = "ivy-native-clock";
fn field<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    value.get(name).ok_or_else(IvyError::internal)
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
/// Run outside a callback on this same RPC actor. Startup queues clock inputs
/// until the launch owner supplies its initialized RPC, as in the original.
pub fn answer_native_clock(
    memory: Arc<Mutex<NativeJournal>>,
    rpc: &NativeRpc,
    input: &IvyJson,
) -> impl std::future::Future<Output = IvyResult<()>> + Send + use<> {
    // Like the original synchronous answer call, dispatch belongs to the native
    // owner once invoked. Dropping the returned waiter cannot cancel that work.
    let worker = tokio::spawn(answer_clock(memory, rpc.clone(), input.clone()));
    async move { worker.await.map_err(|_| IvyError::internal())? }
}
async fn answer_clock(
    memory: Arc<Mutex<NativeJournal>>,
    rpc: NativeRpc,
    input: IvyJson,
) -> IvyResult<()> {
    require(
        input.get("method") == Some(&IvyJson::from("currentTime/read")),
        "native_clock_unavailable",
        "A host clock answer requires its exact live native request.",
    )?;
    let input_identity = field(&input, "identity")?.clone();
    if input_identity == IvyJson::Null {
        return Err(IvyError::internal());
    }
    let (identity, reply) = {
        let mut journal = memory.lock().unwrap();
        require(
            input_identity.get("epoch")
                == Some(
                    &journal
                        .epoch()
                        .cloned()
                        .map(IvyJson::String)
                        .unwrap_or(IvyJson::Null),
                )
                && rpc.connected(),
            "native_clock_unavailable",
            "A host clock answer requires its exact live native request.",
        )?;
        let original = journal.input(&input_identity)?;
        require(
            original.as_ref().is_some_and(|original| {
                original.get("method") == input.get("method")
                    && original.get("observedAt") == input.get("observedAt")
            }) && original
                .as_ref()
                .map(|original| canonical(field(original, "params")?, MANAGEMENT_FRAME_BYTES))
                .transpose()?
                == Some(canonical(field(&input, "params")?, MANAGEMENT_FRAME_BYTES)?),
            "native_clock_conflict",
            "The host clock must retain its original observed request and time.",
        )?;
        // observedAt is created by NativeJournal's canonical ISO clock.
        let original = original.unwrap();
        let millis = field(&original, "observedAt")?
            .as_js_string()
            .and_then(|v| native_instant(v, None).ok());
        let millis = millis.ok_or_else(|| {
            IvyError::new(
                "native_clock_invalid",
                "The original host clock observation must be a finite whole Unix time.",
            )
        })?;
        let identity = Identity {
            caller_principal_id: NATIVE_CLOCK_PRINCIPAL.into(),
            operation_id: format!("clock:{}", &hash_json(&input_identity)?[7..]).into(),
        };
        let reply = IvyJson::object([(
            "result",
            IvyJson::object([(
                "currentTimeAt",
                IvyJson::Number(millis.div_euclid(1000) as f64),
            )]),
        )]);
        let (_, created) = journal.accept(
            &identity,
            &"agent.answer".into(),
            &IvyJson::object([
                ("identity", input_identity.clone()),
                ("reply", reply.clone()),
            ]),
        )?;
        if !created {
            return Ok(());
        }
        (identity, reply)
    };
    let hook = {
        let memory = memory.clone();
        let identity = identity.clone();
        let input_identity = input_identity.clone();
        let reply = reply.clone();
        Arc::new(move |_| {
            let memory = memory.clone();
            let identity = identity.clone();
            let input_identity = input_identity.clone();
            let reply = reply.clone();
            Box::pin(async move {
                memory
                    .lock()
                    .unwrap()
                    .dispatch_answer(&identity, &input_identity, &reply)?;
                Ok(())
            }) as HookFuture
        })
    };
    if let Err(error) = rpc
        .answer(
            "currentTime/read".into(),
            field(&input_identity, "requestId")?.clone(),
            reply,
            hook,
        )
        .await
    {
        let mut journal = memory.lock().unwrap();
        if journal
            .get(&identity)?
            .is_some_and(|v| v.get("phase") == Some(&IvyJson::from("accepted")))
        {
            journal.reject_before_dispatch(&identity, &error.code_js())?;
        }
        return Err(error);
    }
    Ok(())
}
