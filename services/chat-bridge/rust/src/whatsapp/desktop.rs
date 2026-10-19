//! Fixed desktop activation and its durable at-most-once attempt marker.
use super::{commands::DesktopCommands, journal_owner::Journal, messages::Translator};
use crate::main_context::truthy;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::{rpc::RequestFuture, service::ServiceFuture};
use std::sync::Arc;

pub struct DesktopInvocation {
    pub executable: &'static str,
    pub arguments: Vec<JsString>,
    pub windows_hide: bool,
    pub shell: bool,
    pub ignored_stdio: bool,
    pub timeout_ms: u64,
}
/// The Host process owner supplies launch, timeout, exit and retirement. Native
/// errors use IvyError::internal(), matching the original Error conversion.
pub trait DesktopHost: Send + Sync {
    fn platform(&self) -> &'static str;
    fn run(&self, invocation: DesktopInvocation) -> ServiceFuture<()>;
}

#[derive(Clone)]
pub struct WhatsAppDesktop {
    journal: Journal,
    host: Arc<dyn DesktopHost>,
    translator: Translator,
}
impl WhatsAppDesktop {
    pub fn new(journal: Journal, host: Arc<dyn DesktopHost>, translator: Translator) -> Self {
        Self {
            journal,
            host,
            translator,
        }
    }
    async fn command(&self, action: &JsString, key: &str) -> IvyResult<Json> {
        let restart = action == &JsString::from("restart");
        if !restart && action != &JsString::from("ensure") {
            return Err(self.error("invalid_arguments", "desktop.usage")?);
        }
        if self.host.platform() != "win32" {
            return Err(self.error("desktop_unavailable", "desktop.platform")?);
        }
        let key = format!("desktop:{key}");
        // Original synchronous get/set cannot interleave. Preserve that property
        // when callers run on different Rust executor threads.
        let cached = self.journal.access(|journal| {
            let saved = journal.get(&key)?;
            if let Some(result) = saved.get("result").filter(|v| truthy(v)) {
                return Ok(Some(result.clone()));
            }
            if saved.get("attempted").is_some_and(truthy) {
                return Err(self.error("desktop_outcome_unknown", "desktop.unclear")?);
            }
            journal.set(&key, &Json::object([("attempted", true.into())]))?;
            Ok(None)
        })?;
        if let Some(result) = cached {
            return Ok(result);
        }
        let owner = self.clone();
        // A started original Promise outlives its observer. Keep the process and
        // result publication alive if an individual Rust caller stops waiting.
        tokio::spawn(async move {
            owner.journal.flush().await?;
            owner.host.run(invocation(restart)).await?;
            let result = Json::String(owner.translator.translate(
                if restart {
                    "desktop.restart"
                } else {
                    "desktop.ensure"
                },
                None,
            )?);
            owner.journal.set(
                &key,
                &Json::object([("attempted", true.into()), ("result", result.clone())]),
            )?;
            Ok(result)
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    fn error(&self, code: &str, message: &str) -> IvyResult<IvyError> {
        Ok(IvyError::new_js(
            code.into(),
            self.translator.translate(message, None)?,
        ))
    }
}
impl DesktopCommands for WhatsAppDesktop {
    fn run<'a>(&'a self, action: &'a JsString, key: &'a str) -> RequestFuture<'a> {
        Box::pin(self.command(action, key))
    }
}
fn invocation(restart: bool) -> DesktopInvocation {
    let close = if restart {
        "foreach($target in $targets){$target.CloseMainWindow() | Out-Null}\nforeach($target in $targets){if(-not $target.WaitForExit(10000)){throw 'Desktop did not close gracefully'}}"
    } else {
        ""
    };
    let script = include_str!("desktop-command.ps1")
        .trim_end_matches('\n')
        .replace("{{CLOSE}}", close)
        .replace(
            "{{START}}",
            if restart {
                "$true"
            } else {
                "$targets.Count -eq 0"
            },
        );
    let encoded = STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    DesktopInvocation {
        executable: "powershell.exe",
        arguments: vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            encoded.into(),
        ],
        windows_hide: true,
        shell: false,
        ignored_stdio: true,
        timeout_ms: 30000,
    }
}
