//! The same original call owns preparation, generation and cleanup for digital
//! audio. A command observer never owns its route or grants a replacement call.
use super::*;
use crate::media::realtime::{RealtimeError, Result as RealtimeResult, route::RealtimeOperation};

impl CallAudioPort {
    #[cfg(test)]
    pub(crate) fn test_realtime_factory(
        mut self,
        factory: Arc<crate::media::realtime::route::RouteFactory>,
    ) -> Self {
        Arc::get_mut(&mut Arc::get_mut(&mut self.handle).unwrap().0)
            .unwrap()
            .realtime_factory = factory;
        self
    }
    pub(crate) fn realtime_observation(&self) -> RealtimeResult<Option<String>> {
        let state = self
            .handle
            .0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state.realtime.as_ref().map(|r| r.observation()).transpose()
    }
    pub(crate) fn realtime_offer(&self, generation: i32) -> RealtimeResult<String> {
        let state = self
            .handle
            .0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if state.realtime_generation != generation {
            return Err(RealtimeError::NotReady);
        }
        state
            .realtime
            .as_ref()
            .map(|r| r.offer().to_owned())
            .ok_or(RealtimeError::NotReady)
    }
    pub(crate) fn prepare_realtime(
        &self,
        generation: i32,
        queue_ms: i32,
        prebuffer_ms: Option<i32>,
    ) -> RealtimeResult<RealtimeOperation> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| RealtimeError::NotReady)?;
        let inner = self.handle.0.clone();
        let preparing = AudioOperation::new();
        let (completion, operation) = RealtimeOperation::channel();
        {
            let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            inner.refresh(&mut state);
            if inner.lease.0.revoked.load(Ordering::Acquire)
                || state.closed
                || state.failed
                || state.preparation.is_some()
                || state.route.is_some()
                || generation != state.realtime_generation + 1
                || generation > 128
            {
                return Err(RealtimeError::NotReady);
            }
            state.realtime_generation = generation;
            state.preparation = Some(preparing.clone());
        }
        runtime.spawn(async move {
            let work = inner.clone();
            let joined = tokio::spawn(async move {
                tokio::task::yield_now().await;
                let queue = u16::try_from(queue_ms).map_err(|_| RealtimeError::InvalidRequest)?;
                let prebuffer = prebuffer_ms
                    .map(u16::try_from)
                    .transpose()
                    .map_err(|_| RealtimeError::InvalidRequest)?;
                let lease = work.lease.clone();
                let opened = (work.realtime_factory)(
                    queue,
                    prebuffer,
                    Arc::new(move || lease.permitted()),
                    work.stop.clone(),
                )
                .await;
                let route = match opened {
                    Ok(route) => route,
                    Err(error) => {
                        return Err(match error {
                            RealtimeError::InvalidRequest | RealtimeError::CleanupUnconfirmed => {
                                error
                            }
                            _ => RealtimeError::PreparationFailed,
                        });
                    }
                };
                let published = {
                    let mut state = work.state.lock().unwrap_or_else(|p| p.into_inner());
                    if work.lease.0.revoked.load(Ordering::Acquire) || state.closed {
                        Err(RealtimeError::Cancelled)
                    } else {
                        state.route = Some(route.clone());
                        state.realtime = Some(route.clone());
                        state.route_epoch += 1;
                        work.refresh(&mut state);
                        Ok(())
                    }
                };
                if published.is_err() {
                    route
                        .close()
                        .await
                        .map_err(|_| RealtimeError::CleanupUnconfirmed)?;
                }
                published
            })
            .await;
            let result = joined.unwrap_or(Err(RealtimeError::CleanupUnconfirmed));
            if result == Err(RealtimeError::CleanupUnconfirmed) {
                inner
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .cleanup_error = Some(CallAudioError::CleanupUnconfirmed);
            }
            preparing.complete(match result {
                Ok(()) => Ok(()),
                Err(RealtimeError::CleanupUnconfirmed) => Err(CallAudioError::CleanupUnconfirmed),
                Err(RealtimeError::InvalidRequest) => Err(CallAudioError::InvalidRequest),
                Err(_) => Err(CallAudioError::PreparationReleased),
            });
            completion.send_replace(Some(result));
        });
        Ok(operation)
    }
    pub(crate) fn accept_realtime(
        &self,
        generation: i32,
        sdp: String,
    ) -> RealtimeResult<RealtimeOperation> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| RealtimeError::NotReady)?;
        let inner = self.handle.0.clone();
        let accepted = {
            let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if generation != state.realtime_generation {
                return Err(RealtimeError::NotReady);
            }
            let route = state.realtime.clone().ok_or(RealtimeError::NotReady)?;
            // This prefix runs only after the command checks the original SIP
            // call's Connected and trusted/authenticated access observations.
            state.permit.grant().map_err(|_| RealtimeError::NotReady)?;
            state.expires = None;
            inner.refresh(&mut state);
            inner
                .activate(&mut state)
                .map_err(|_| RealtimeError::NotReady)?;
            route.answer(sdp)?
        };
        let (completion, operation) = RealtimeOperation::channel();
        runtime.spawn(async move {
            let result = match accepted.wait().await {
                Err(error) => Err(error),
                Ok(()) => {
                    let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                    let open = inner.refresh(&mut state).open;
                    if state.closed || inner.lease.0.revoked.load(Ordering::Acquire) || !open {
                        Err(RealtimeError::NotReady)
                    } else {
                        Ok(())
                    }
                }
            };
            completion.send_replace(Some(result));
        });
        Ok(operation)
    }
    pub(crate) fn stop_realtime(&self, generation: i32) -> RealtimeResult<AudioOperation> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| RealtimeError::NotReady)?;
        let inner = self.handle.0.clone();
        let operation = AudioOperation::new();
        let preparing = {
            let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if generation != state.realtime_generation {
                return Err(RealtimeError::NotReady);
            }
            clear(&inner, &mut state).map_err(|_| RealtimeError::Failed)?;
            if let Some(stopping) = &state.realtime_stopping {
                if !stopping.finished() {
                    return Ok(stopping.clone());
                }
            }
            state.realtime_stopping = Some(operation.clone());
            state.preparation.clone()
        };
        let completion = operation.clone();
        runtime.spawn(async move {
            let work = inner.clone();
            let result = tokio::spawn(async move {
                if let Some(preparing) = preparing {
                    let _ = preparing.wait().await;
                }
                let selected = {
                    let mut state = work.state.lock().unwrap_or_else(|p| p.into_inner());
                    let selected = state.realtime.take();
                    if selected.is_some() {
                        state.route = None;
                        state.route_epoch += 1;
                    }
                    selected
                };
                if let Some(selected) = selected {
                    selected.close().await?;
                }
                let mut state = work.state.lock().unwrap_or_else(|p| p.into_inner());
                if !state.closed && !work.lease.0.revoked.load(Ordering::Acquire) {
                    state.preparation = None;
                }
                work.refresh(&mut state);
                Ok(())
            })
            .await
            .unwrap_or(Err(CallAudioError::CleanupUnconfirmed));
            if let Err(error) = result {
                let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                state.cleanup_error = Some(error);
                state.failed = true;
            }
            completion.complete(result);
        });
        Ok(operation)
    }
}
