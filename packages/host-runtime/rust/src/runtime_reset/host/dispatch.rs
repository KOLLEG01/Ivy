//! Host-owned phases only. Preview bundle admission and Hive clear/publication
//! stay at their owning boundaries; neither is replaced with an empty adapter.
use super::*;
use crate::{node_runtime::NodeRuntime, process_output::Utf8Decoder};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::limits::MANAGEMENT_FRAME_BYTES;

#[derive(Clone, Copy, Debug)]
pub enum ResetHostPhase {
    Prepare,
    Complete,
    Bootstrap,
    Release,
    Confirm,
}
impl ResetHostPhase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Complete => "complete",
            Self::Bootstrap => "bootstrap",
            Self::Release => "release",
            Self::Confirm => "confirm",
        }
    }
}

#[derive(Clone, Default)]
pub struct ResetPhaseArguments {
    pub reset_id: Option<JsString>,
    pub runtime_epoch: Option<JsString>,
    pub scope_roots: Option<JsString>,
    pub publisher_digest: Option<JsString>,
}

/// The coordinator supplies one retained identity and authority for every host.
#[derive(Clone)]
pub struct CoordinatedReset {
    pub reset_id: JsString,
    pub runtime_epoch: JsString,
    pub scope_roots: JsString,
    pub publisher_digest: JsString,
}

pub fn encode_scope_roots(ids: &[JsString]) -> IvyResult<JsString> {
    let value = J::Array(ids.iter().cloned().map(J::String).collect());
    Ok(URL_SAFE_NO_PAD
        .encode(json::canonical(&value, MANAGEMENT_FRAME_BYTES)?)
        .into())
}

pub fn decode_scope_roots(value: &JsString) -> IvyResult<Vec<JsString>> {
    let invalid = || IvyError::new("invalid_arguments", "Invalid coordinated scope roots.");
    // Buffer.from(string, "base64url") narrows UTF-16 units, accepts both
    // alphabets, skips invalid bytes and stops at padding. A strict decoder
    // would reject inputs the existing CLI accepts, including trailing bits.
    let mut bytes = Vec::new();
    let (mut bits, mut count) = (0u32, 0u32);
    for unit in &value.0 {
        let sextet = match *unit as u8 {
            b'A'..=b'Z' => *unit as u8 - b'A',
            b'a'..=b'z' => *unit as u8 - b'a' + 26,
            b'0'..=b'9' => *unit as u8 - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(sextet);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
        }
    }
    let text = JsString(Utf8Decoder::preserving_bom().decode(&bytes, true));
    let decoded =
        json::parse(&text.to_utf8().map_err(|_| invalid())?, usize::MAX).map_err(|_| invalid())?;
    let ids = decoded.as_array().ok_or_else(invalid)?;
    require(
        ids.iter().all(|id| {
            id.as_js_string()
                .is_some_and(|id| !id.0.is_empty() && id.utf16_len() <= 256)
        }),
        "invalid_arguments",
        "Invalid coordinated scope roots.",
    )?;
    let canonical = json::canonical(&decoded, MANAGEMENT_FRAME_BYTES)?;
    let sorted = J::Array(
        ids.iter()
            .map(|id| id.as_js_string().unwrap().clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(J::String)
            .collect(),
    );
    require(
        canonical == json::canonical(&sorted, MANAGEMENT_FRAME_BYTES)?,
        "invalid_arguments",
        "Invalid coordinated scope roots.",
    )?;
    Ok(ids
        .iter()
        .map(|id| id.as_js_string().unwrap().clone())
        .collect())
}

/// Exact coordinator argv; only prepare carries scope and publisher authority.
/// Preview is assembled by its owner as ["--phase", "preview"].
pub fn reset_phase_arguments(phase: &JsString, reset: &CoordinatedReset) -> Vec<JsString> {
    let mut args = vec![
        "--phase".into(),
        phase.clone(),
        "--reset-id".into(),
        reset.reset_id.clone(),
        "--runtime-epoch".into(),
        reset.runtime_epoch.clone(),
    ];
    if phase == &JsString::from("prepare") {
        args.extend([
            "--scope-roots".into(),
            reset.scope_roots.clone(),
            "--publisher-digest".into(),
            reset.publisher_digest.clone(),
        ]);
    }
    args
}

fn admit_plan(mut plan: J, phase: ResetHostPhase, args: &ResetPhaseArguments) -> IvyResult<J> {
    let present = |value: &Option<JsString>| value.as_ref().is_some_and(|v| !v.0.is_empty());
    require(
        present(&args.reset_id) && present(&args.runtime_epoch),
        "invalid_arguments",
        "Mutating runtime reset phases require reset and epoch identities.",
    )?;
    if matches!(phase, ResetHostPhase::Prepare) {
        require(
            present(&args.scope_roots) && present(&args.publisher_digest),
            "invalid_arguments",
            "Reset prepare requires coordinated scope roots and ui publisher identity.",
        )?;
        plan.insert(
            "scopeRootIds",
            J::Array(
                decode_scope_roots(args.scope_roots.as_ref().unwrap())?
                    .into_iter()
                    .map(J::String)
                    .collect(),
            ),
        );
    }
    Ok(plan)
}

impl ResetHost {
    /// Plan first, then admit phase identities, then retain the actual phase's
    /// administrator check. Caller supplies the real storage/runtime bindings.
    pub async fn run_phase(
        &self,
        config: J,
        config_path: JsString,
        phase: ResetHostPhase,
        args: ResetPhaseArguments,
        node: impl Into<Option<NodeRuntime>>,
        storage: Arc<layout::StorageContext>,
    ) -> IvyResult<J> {
        let plan = local_runtime_reset_plan(
            config.clone(),
            config_path,
            self.0.distribution.clone(),
            storage,
        )
        .await?;
        self.dispatch_plan(config, plan, phase, args, node.into()).await
    }

    async fn dispatch_plan(
        &self,
        config: J,
        plan: J,
        phase: ResetHostPhase,
        args: ResetPhaseArguments,
        node: Option<NodeRuntime>,
    ) -> IvyResult<J> {
        let plan = admit_plan(plan, phase, &args)?;
        let (id, epoch) = (args.reset_id.unwrap(), args.runtime_epoch.unwrap());
        match phase {
            ResetHostPhase::Prepare => {
                self.prepare(config, plan, id, epoch, args.publisher_digest.unwrap())
                    .await
            }
            ResetHostPhase::Complete => self.complete(config, id, epoch).await,
            ResetHostPhase::Bootstrap => self.bootstrap(config, id, epoch, node).await,
            ResetHostPhase::Release => self.release(config, id, epoch, node).await,
            ResetHostPhase::Confirm => self.confirm(config, id, epoch).await,
        }
    }

    /// Match the coordinator's host-ID selection and reuse the concrete remote
    /// execution/envelope path. Local planning is never used for a remote host.
    pub async fn invoke_phase(
        &self,
        config: J,
        config_path: JsString,
        host: J,
        phase: ResetHostPhase,
        reset: CoordinatedReset,
        node: impl Into<Option<NodeRuntime>>,
        storage: Arc<layout::StorageContext>,
    ) -> IvyResult<Option<J>> {
        if host.get("hostId") == config.get("hostId") {
            self.run_phase(
                config,
                config_path,
                phase,
                ResetPhaseArguments {
                    reset_id: Some(reset.reset_id),
                    runtime_epoch: Some(reset.runtime_epoch),
                    scope_roots: matches!(phase, ResetHostPhase::Prepare)
                        .then_some(reset.scope_roots),
                    publisher_digest: matches!(phase, ResetHostPhase::Prepare)
                        .then_some(reset.publisher_digest),
                },
                node,
                storage,
            )
            .await
            .map(Some)
        } else {
            self.remote(
                config,
                host,
                reset_phase_arguments(&phase.name().into(), &reset),
            )
            .await
        }
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/rust/host-reset-dispatch-cases.rs"]
mod dispatch_cases;
