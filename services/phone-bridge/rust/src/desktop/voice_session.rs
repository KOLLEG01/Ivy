use super::controls::guarded;
use super::{
    ControlError, ControlSource, ControlWindow, VoiceControlIdentity, VoiceControlObservation,
    classify_voice, read_controls, voice_labels,
};
use ivy_contracts::json::JsString;
use std::{
    sync::Arc,
    thread::{ThreadId, current},
    time::{Duration, Instant},
};

pub trait VoiceSource: ControlSource {
    fn refresh(
        &mut self,
        identity: VoiceControlIdentity,
    ) -> Result<Vec<ControlWindow>, ControlError>;
}
#[derive(Clone, Debug)]
pub struct TimedVoiceControlObservation {
    pub timestamp: Instant,
    pub observation: VoiceControlObservation,
}
struct Borrowed<'a, S> {
    source: &'a mut S,
    identity: Option<VoiceControlIdentity>,
}
impl<S: VoiceSource> ControlSource for Borrowed<'_, S> {
    fn read(&mut self, labels: &[JsString]) -> Result<Vec<ControlWindow>, ControlError> {
        match &self.identity {
            Some(identity) => self.source.refresh(identity.clone()),
            None => self.source.read(labels),
        }
    }
}

/// Retains one provider and original control identity on its owning thread.
/// Discovery can be slow; it never supplies a timed positive to the call owner.
pub struct DesktopVoiceSession<S: VoiceSource> {
    source: S,
    current: Arc<dyn Fn() -> bool + Send + Sync>,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
    thread: ThreadId,
    identity: VoiceControlIdentity,
    disposed: bool,
}
impl<S: VoiceSource> DesktopVoiceSession<S> {
    pub fn open(
        open: impl FnOnce() -> Result<S, ControlError>,
        current: Arc<dyn Fn() -> bool + Send + Sync>,
        now: Option<Arc<dyn Fn() -> Instant + Send + Sync>>,
    ) -> Result<Self, ControlError> {
        if !current() {
            return Err(ControlError::NotCurrent);
        }
        let mut source = open()?;
        let discovered = guarded(|| {
            let observation = read_controls(
                &voice_labels(),
                &*current,
                || {
                    Ok(Borrowed {
                        source: &mut source,
                        identity: None,
                    })
                },
                None,
                None,
            )?;
            classify_voice(Some(&observation))
                .identity
                .ok_or(ControlError::VoiceGroup)
        });
        match discovered {
            Ok(identity) => Ok(Self {
                source,
                current,
                now: now.unwrap_or_else(|| Arc::new(Instant::now)),
                thread: current_thread(),
                identity,
                disposed: false,
            }),
            Err(error) => {
                guarded(|| source.close())?;
                Err(error)
            }
        }
    }
    fn check_thread(&self) -> Result<(), ControlError> {
        if current_thread() == self.thread {
            Ok(())
        } else {
            Err(ControlError::WrongThread)
        }
    }
    pub fn read(&mut self) -> Result<TimedVoiceControlObservation, ControlError> {
        self.check_thread()?;
        let timestamp = (self.now)();
        let unavailable = || TimedVoiceControlObservation {
            timestamp,
            observation: VoiceControlObservation::unavailable(),
        };
        if self.disposed {
            return Ok(unavailable());
        }
        let refreshed = read_controls(
            &voice_labels(),
            &*self.current,
            || {
                Ok(Borrowed {
                    source: &mut self.source,
                    identity: Some(self.identity.clone()),
                })
            },
            None,
            None,
        )?;
        let classified = classify_voice(Some(&refreshed));
        let age = (self.now)().checked_duration_since(timestamp);
        if age.is_none_or(|age| age >= Duration::from_millis(500))
            || classified.identity.as_ref() != Some(&self.identity)
        {
            return Ok(unavailable());
        }
        Ok(TimedVoiceControlObservation {
            timestamp,
            observation: classified,
        })
    }
    pub fn close(&mut self) -> Result<(), ControlError> {
        self.check_thread()?;
        if self.disposed {
            return Ok(());
        }
        self.disposed = true;
        guarded(|| self.source.close())
    }
}
fn current_thread() -> ThreadId {
    current().id()
}
impl<S: VoiceSource> Drop for DesktopVoiceSession<S> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
