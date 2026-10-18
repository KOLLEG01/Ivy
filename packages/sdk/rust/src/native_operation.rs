//! Reconcile one original native operation without implicit dispatch authority.
use crate::native_evidence::{native_operation_terminal, validate_native_progress};
use crate::{IvyError, IvyJson, IvyResult, JsString, rpc::RequestFuture};
use std::{future::Future, pin::Pin};

#[derive(Clone, Debug)]
pub struct NativeOperationCall {
    pub operation_id: JsString,
    pub method: JsString,
    pub params: IvyJson,
    pub definition_hash: JsString,
}
/// Implementations return owner-validated Operation or OperationAbsence evidence,
/// bound to the original caller. Generic not-found errors are not absence evidence.
pub trait NativeOperationOwner: Send + Sync {
    fn operation<'a>(&'a self, call: &'a NativeOperationCall) -> RequestFuture<'a>;
    fn check_operation(&self, observation: &IvyJson, call: &NativeOperationCall) -> IvyResult<()>;
}
pub type SubmissionFuture<'a> =
    Pin<Box<dyn Future<Output = IvyResult<Option<IvyJson>>> + Send + 'a>>;
/// Called only on confirmed absence of work never observed by this journal.
/// Domain/epoch guards and permission to dispatch remain mandatory in the handler.
pub trait NativeAbsentHandler: Send + Sync {
    fn on_absent<'a>(
        &'a self,
        call: NativeOperationCall,
        absence: &'a IvyJson,
    ) -> SubmissionFuture<'a>;
}
pub struct NativeOperationJournal<T, R> {
    pub current: T,
    pub previous: Option<IvyJson>,
    /// A compact retained marker forbids replay even without the complete receipt.
    pub was_observed: bool,
    pub retain: R,
}
#[derive(Debug)]
pub enum NativeReconciliation<T> {
    Observed { value: T, observation: IvyJson },
    Absent { value: T, absence: IvyJson },
}
pub async fn reconcile_native_operation<T, R, F>(
    owner: &dyn NativeOperationOwner,
    call: &NativeOperationCall,
    mut journal: NativeOperationJournal<T, R>,
    on_absent: Option<&dyn NativeAbsentHandler>,
    absence_conflict_code: Option<&str>,
) -> IvyResult<NativeReconciliation<T>>
where
    R: FnMut(IvyJson) -> F,
    F: Future<Output = IvyResult<T>>,
{
    let call = call.clone();
    if let Some(previous) = &journal.previous {
        owner.check_operation(previous, &call)?;
        if native_operation_terminal(previous) {
            return Ok(NativeReconciliation::Observed {
                value: journal.current,
                observation: previous.clone(),
            });
        }
    }
    let mut observed = owner.operation(&call).await?;
    if observed.get("kind").is_some() {
        if journal.previous.is_some() || journal.was_observed {
            return Err(IvyError::new(
                absence_conflict_code.unwrap_or("native_absence_conflict"),
                "Owner absence cannot erase a previously observed operation or authorize replay.",
            ));
        }
        let Some(handler) = on_absent else {
            return Ok(NativeReconciliation::Absent {
                value: journal.current,
                absence: observed,
            });
        };
        observed = match handler.on_absent(call.clone(), &observed).await? {
            Some(submitted) => submitted,
            None => owner.operation(&call).await?,
        };
        if observed.get("kind").is_some() {
            return Ok(NativeReconciliation::Absent {
                value: journal.current,
                absence: observed,
            });
        }
    }
    owner.check_operation(&observed, &call)?;
    if let Some(previous) = &journal.previous {
        validate_native_progress(previous, &observed, None)?;
    }
    let unchanged = journal.previous.as_ref().is_some_and(|previous| {
        previous.get("updatedAt") == observed.get("updatedAt")
            && previous.get("phase") == observed.get("phase")
    });
    let value = if unchanged {
        journal.current
    } else {
        (journal.retain)(observed.clone()).await?
    };
    Ok(NativeReconciliation::Observed {
        value,
        observation: observed,
    })
}
