//! The runtime borrows the existing domain owners through a generation lease.
use super::{
    commands::{CommandFuture, DesktopCommands, WhatsAppCommands},
    journal::Inbox,
    journal_owner::Journal,
    listener::WhatsAppListener,
    messages::Translator,
    native::WhatsAppNative,
    runtime::RuntimeMedia,
};
use crate::{
    chat_main::ChatMain,
    generation::{ChatGeneration, ChatGenerationLease},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::{
    clock::automatic_notifications_allowed,
    operation_ids::scoped_operation_id,
    rpc::{RequestFuture, RequestOptions},
};
use std::{future::Future, pin::Pin, sync::Arc};

pub type BoolFuture<'a> = Pin<Box<dyn Future<Output = IvyResult<bool>> + Send + 'a>>;
pub trait RuntimeAttached: Send + Sync {
    fn definition(&self) -> &Json;
    fn expected(&self, sender: &JsString) -> Json;
    fn invalidate(&self, connection_changed: bool);
    fn set_busy(&self, busy: bool);
    fn last_dispatch_ms(&self) -> Option<u64>;
    fn prepare(&self) -> RequestFuture<'_>;
    fn bound(&self) -> RequestFuture<'_>;
    fn same_host<'a>(&'a self, host: &'a JsString) -> BoolFuture<'a>;
    fn send<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a>;
    fn command<'a>(&'a self, row: &'a Inbox, text: &'a JsString) -> CommandFuture<'a>;
    fn preferences(&self) -> IvyResult<Json>;
    fn turn_options(&self) -> IvyResult<Json>;
    fn operation_id(&self, scope: Json) -> RequestFuture<'_>;
    fn ensure<'a>(&'a self, sender: &'a JsString, operation: &'a Json) -> RequestFuture<'a>;
    fn queue_send<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a>;
    fn payload<'a>(&'a self, row: &'a Inbox, same_host: bool) -> RequestFuture<'a>;
    fn tick(&self, commentary: bool) -> BoolFuture<'_>;
    fn needs_recovery(&self) -> bool;
    fn notification<'a>(&'a self, event: &'a Json, commentary: bool) -> RequestFuture<'a>;
    fn notices(&self) -> RequestFuture<'_>;
    fn outbox(&self) -> RequestFuture<'_>;
    fn automatic_notifications(&self, now: i64) -> BoolFuture<'_>;
}
pub trait RuntimeAttachmentFactory: Send + Sync {
    fn create(&self, generation: Arc<ChatGeneration>) -> IvyResult<Arc<dyn RuntimeAttached>>;
}
pub struct RuntimeAssembly {
    pub journal: Journal,
    pub config: Json,
    pub media: Arc<dyn RuntimeMedia>,
    pub desktop: Arc<dyn DesktopCommands>,
    pub translator: Translator,
}
struct Attached {
    lease: Arc<ChatGenerationLease>,
    journal: Journal,
    native: WhatsAppNative,
    listener: WhatsAppListener,
    media: Arc<dyn RuntimeMedia>,
    desktop: Arc<dyn DesktopCommands>,
    translator: Translator,
}
impl RuntimeAttachmentFactory for RuntimeAssembly {
    fn create(&self, generation: Arc<ChatGeneration>) -> IvyResult<Arc<dyn RuntimeAttached>> {
        let lease = Arc::new(generation.accept()?);
        let native = WhatsAppNative::new(lease.clone(), self.journal.clone(), self.translator)?;
        let listener = WhatsAppListener::new(
            lease.clone(),
            self.journal.clone(),
            native.clone(),
            self.config.clone(),
            self.translator,
        );
        Ok(Arc::new(Attached {
            lease,
            journal: self.journal.clone(),
            native,
            listener,
            media: self.media.clone(),
            desktop: self.desktop.clone(),
            translator: self.translator,
        }))
    }
}
impl Attached {
    fn commands(&self) -> WhatsAppCommands<'_> {
        WhatsAppCommands {
            main: self.lease.as_ref(),
            journal: self.journal.clone(),
            native: &self.native,
            desktop: self.desktop.as_ref(),
            translator: self.translator,
        }
    }
}
impl RuntimeAttached for Attached {
    fn definition(&self) -> &Json {
        self.lease.config().admission.definition()
    }
    fn expected(&self, sender: &JsString) -> Json {
        self.lease.config().admission.expected(sender)
    }
    fn invalidate(&self, changed: bool) {
        self.native.invalidate(changed)
    }
    fn set_busy(&self, busy: bool) {
        self.native.set_busy(busy)
    }
    fn last_dispatch_ms(&self) -> Option<u64> {
        self.native.last_dispatch_ms()
    }
    fn prepare(&self) -> RequestFuture<'_> {
        Box::pin(async {
            self.native.prepare().await?;
            Ok(Json::Null)
        })
    }
    fn bound(&self) -> RequestFuture<'_> {
        Box::pin(self.native.bound())
    }
    fn same_host<'a>(&'a self, host: &'a JsString) -> BoolFuture<'a> {
        Box::pin(self.native.same_host(host))
    }
    fn send<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(self.native.send(sender, request))
    }
    fn command<'a>(&'a self, row: &'a Inbox, text: &'a JsString) -> CommandFuture<'a> {
        Box::pin(async move { self.commands().run(row, text).await })
    }
    fn preferences(&self) -> IvyResult<Json> {
        self.commands().preferences()
    }
    fn turn_options(&self) -> IvyResult<Json> {
        self.commands().turn_options()
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            Ok(Json::String(
                scoped_operation_id(self.lease.store().client.clone(), &scope).await?,
            ))
        })
    }
    fn ensure<'a>(&'a self, sender: &'a JsString, operation: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            let native = self.lease.native()?;
            let operations = self.lease.operations()?;
            let main = ChatMain::new(
                &operations,
                &native,
                &self.lease.config().notice_instructions,
            )?;
            main.ensure(
                sender,
                operation.as_js_string().ok_or_else(IvyError::internal)?,
            )
            .await
        })
    }
    fn queue_send<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            let native = self.lease.native()?;
            let operations = self.lease.operations()?;
            ChatMain::new(
                &operations,
                &native,
                &self.lease.config().notice_instructions,
            )?
            .queue()
            .send(sender, request)
            .await
        })
    }
    fn payload<'a>(&'a self, row: &'a Inbox, same_host: bool) -> RequestFuture<'a> {
        self.media.payload(row, &self.lease, same_host)
    }
    fn tick(&self, commentary: bool) -> BoolFuture<'_> {
        Box::pin(self.listener.tick(commentary))
    }
    fn needs_recovery(&self) -> bool {
        self.listener.needs_recovery()
    }
    fn notification<'a>(&'a self, event: &'a Json, commentary: bool) -> RequestFuture<'a> {
        Box::pin(self.listener.notification(event, commentary))
    }
    fn notices(&self) -> RequestFuture<'_> {
        Box::pin(async {
            self.listener.notices().await?;
            Ok(Json::Null)
        })
    }
    fn outbox(&self) -> RequestFuture<'_> {
        Box::pin(async {
            self.listener.outbox().await?;
            Ok(Json::Null)
        })
    }
    fn automatic_notifications(&self, now: i64) -> BoolFuture<'_> {
        Box::pin(async move {
            automatic_notifications_allowed(
                self.lease.store().client.as_ref(),
                now,
                RequestOptions::default(),
            )
            .await
        })
    }
}
