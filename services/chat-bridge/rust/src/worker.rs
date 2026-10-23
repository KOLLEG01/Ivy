//! Bounded owner loop. Durable Main/input state selects the next lane; this
//! instance only retains progress hints and the collector/publication caches.
use crate::{
    cancel_worker::ChatCancelWorker,
    collector::{ChatCollector, CollectionRuntime},
    delivery::ChatDelivery,
    operations::contention,
    publisher::ChatPublisher,
    store::Reference,
};
use ivy_contracts::{IvyError, IvyResult, JsString};
use ivy_sdk::clock::{Clock, SystemClock};
use std::{sync::Arc, time::Duration};

pub struct ChatWorker<'a> {
    pub idle: bool,
    pub runnable: bool,
    pub turn_id: Option<JsString>,
    pub collector: &'a ChatCollector<'a>,
    pub publisher: ChatPublisher<'a>,
    pub cancellation: ChatCancelWorker<'a>,
    delivery: &'a ChatDelivery<'a>,
    clock: Arc<dyn Clock>,
    turn_observer: Option<Arc<dyn Fn(Option<JsString>) + Send + Sync>>,
}
impl<'a> ChatWorker<'a> {
    /// The generation owns these existing components in dependency order. Keep
    /// the collector's cache between passes without a self-referential owner.
    pub fn new(
        delivery: &'a ChatDelivery<'a>,
        collector: &'a ChatCollector<'a>,
    ) -> IvyResult<Self> {
        Self::with_clock(delivery, collector, Arc::new(SystemClock))
    }
    pub fn with_clock(
        delivery: &'a ChatDelivery<'a>,
        collector: &'a ChatCollector<'a>,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        let runtime: &dyn CollectionRuntime = delivery;
        if !std::ptr::eq(collector.operations, delivery.main.operations)
            || !std::ptr::addr_eq(collector.runtime, runtime)
        {
            return Err(IvyError::new(
                "chat_scope_mismatch",
                "Worker lanes must share the same Main operations and live delivery runtime.",
            ));
        }
        Ok(Self {
            idle: false,
            runnable: false,
            turn_id: None,
            collector,
            publisher: ChatPublisher::new(collector.operations, collector),
            cancellation: ChatCancelWorker::new(
                collector,
                delivery.main.native,
                delivery.main,
                delivery.main.notice_instructions(),
            ),
            delivery,
            clock,
            turn_observer: None,
        })
    }
    pub(crate) fn observe_turns(&mut self, observer: Arc<dyn Fn(Option<JsString>) + Send + Sync>) {
        observer(self.turn_id.clone());
        self.turn_observer = Some(observer);
    }
    fn selected_turn(&mut self, value: Option<JsString>) {
        self.turn_id = value.clone();
        if let Some(observer) = &self.turn_observer {
            observer(value);
        }
    }
    pub async fn drain<F: FnMut() -> bool + Send>(
        &mut self,
        mut should_continue: F,
    ) -> IvyResult<()> {
        self.runnable = false;
        let deadline = self.clock.monotonic() + Duration::from_millis(50);
        for _ in 0..16 {
            if !should_continue() {
                self.runnable = false;
                return Ok(());
            }
            let before = self.delivery.store().change_version();
            self.runnable = false;
            self.step().await?;
            self.runnable = !self.idle && self.delivery.store().change_version() != before;
            if !self.runnable || self.clock.monotonic() >= deadline {
                return Ok(());
            }
        }
        Ok(())
    }
    pub async fn step(&mut self) -> IvyResult<()> {
        self.idle = false;
        let work: IvyResult<()> = async {
            self.delivery.main.recover_pending().await?;
            let current = self.delivery.main.find().await?;
            let ticket = current
                .as_ref()
                .and_then(|m| m.value.get("queue"))
                .and_then(|q| q.as_array())
                .and_then(|q| q.first());
            let Some(ticket) = ticket else {
                self.idle = true;
                self.selected_turn(None);
                return Ok(());
            };
            if current
                .as_ref()
                .unwrap()
                .value
                .get("publication")
                .is_some_and(|value| !value.is_null())
            {
                return self.publisher.step().await;
            }
            let input = self
                .delivery
                .store()
                .read(
                    "chat-bridge/input",
                    Reference::Current(&ticket.required("inputId")?.as_str()?),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            self.selected_turn(input.value.required("turnId")?.as_js_string().cloned());
            if input
                .value
                .get("finishedAt")
                .is_some_and(|value| !value.is_null())
                && !input
                    .value
                    .get("turnId")
                    .is_some_and(|value| !value.is_null())
            {
                return self.cancellation.settle().await;
            }
            if input
                .value
                .get("turnId")
                .is_some_and(|value| !value.is_null())
            {
                let collection = self.collector.step(&input.pin.object_id).await?;
                if collection
                    .as_ref()
                    .is_some_and(|c| c.value.get("state") == Some(&"complete".into()))
                {
                    self.publisher.step().await?;
                } else if input
                    .value
                    .get("cancellation")
                    .is_some_and(|value| !value.is_null())
                {
                    self.cancellation.step().await?;
                }
            } else if input
                .value
                .get("cancellation")
                .is_some_and(|value| !value.is_null())
            {
                self.cancellation.step().await?;
            } else {
                self.delivery.step().await?;
            }
            Ok(())
        }
        .await;
        match work {
            Err(e) if contention(&e) => Ok(()),
            other => other,
        }
    }
}
