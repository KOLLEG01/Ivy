//! Desktop application and control operations. Native resources are constructed
//! and released on their executing worker.
use super::{
    ActionResult, AudioProcessOwnership, CaptureObservation, ControlsObservation,
    CurrentApplication, DesktopActionError, DesktopApplication, DesktopCaptureCorrelation,
    DesktopLaunchResult, DesktopObservation, launch_desktop, validate_application,
    validate_capture_text, validate_labels,
};
use crate::media::AudioDesktopIdentity;
use ivy_contracts::json::JsString;
use std::sync::Arc;

type Factory =
    dyn Fn(Option<&str>) -> ActionResult<Option<Box<dyn DesktopApplication>>> + Send + Sync;
type Controls = dyn Fn(&AudioDesktopIdentity, &[JsString]) -> ActionResult<Option<ControlsObservation>>
    + Send
    + Sync;
type Capture = dyn Fn(&JsString) -> ActionResult<CaptureObservation> + Send + Sync;
type Process =
    dyn Fn(&AudioDesktopIdentity, i32) -> ActionResult<Option<AudioProcessOwnership>> + Send + Sync;
type CaptureOwner = dyn Fn(&AudioDesktopIdentity, &JsString) -> ActionResult<Option<DesktopCaptureCorrelation>>
    + Send
    + Sync;

#[derive(Clone)]
pub struct PhoneDesktopRuntime {
    application: Arc<Factory>,
    controls: Arc<Controls>,
    capture: Arc<Capture>,
    process: Arc<Process>,
    capture_owner: Arc<CaptureOwner>,
}
impl PhoneDesktopRuntime {
    pub fn with_application(
        application: impl Fn(Option<&str>) -> ActionResult<Option<Box<dyn DesktopApplication>>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            application: Arc::new(application),
            controls: Arc::new(platform_controls),
            capture: Arc::new(platform_capture),
            process: Arc::new(platform_process),
            capture_owner: Arc::new(platform_capture_owner),
        }
    }
    pub fn with_capture(
        mut self,
        capture: impl Fn(&JsString) -> ActionResult<CaptureObservation> + Send + Sync + 'static,
    ) -> Self {
        self.capture = Arc::new(capture);
        self
    }
    pub fn capture(&self, endpoint_id: &JsString) -> ActionResult<CaptureObservation> {
        validate_capture_text(endpoint_id)?;
        (self.capture)(endpoint_id)
    }
    pub fn with_process(
        mut self,
        process: impl Fn(&AudioDesktopIdentity, i32) -> ActionResult<Option<AudioProcessOwnership>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.process = Arc::new(process);
        self
    }
    pub fn process(
        &self,
        desktop: &AudioDesktopIdentity,
        pid: i32,
    ) -> ActionResult<Option<AudioProcessOwnership>> {
        if pid <= 0 {
            return Err(DesktopActionError::Argument(
                "Positive audio process ID required.",
            ));
        }
        (self.process)(desktop, pid)
    }
    pub fn with_capture_owner(
        mut self,
        capture_owner: impl Fn(
            &AudioDesktopIdentity,
            &JsString,
        ) -> ActionResult<Option<DesktopCaptureCorrelation>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.capture_owner = Arc::new(capture_owner);
        self
    }
    pub fn capture_owner(
        &self,
        desktop: &AudioDesktopIdentity,
        endpoint_id: &JsString,
    ) -> ActionResult<Option<DesktopCaptureCorrelation>> {
        validate_capture_text(endpoint_id)?;
        self.voice_capture(desktop, endpoint_id)
    }
    pub(super) fn voice_capture(
        &self,
        desktop: &AudioDesktopIdentity,
        endpoint_id: &JsString,
    ) -> ActionResult<Option<DesktopCaptureCorrelation>> {
        // The original internal VoiceCapture delegates directly. The public
        // capture-owner command above owns its separate endpoint validation.
        (self.capture_owner)(desktop, endpoint_id)
    }
    pub fn with_controls(
        mut self,
        controls: impl Fn(
            &AudioDesktopIdentity,
            &[JsString],
        ) -> ActionResult<Option<ControlsObservation>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.controls = Arc::new(controls);
        self
    }
    pub fn controls(
        &self,
        desktop: &AudioDesktopIdentity,
        labels: &[JsString],
    ) -> ActionResult<Option<ControlsObservation>> {
        validate_labels(labels)
            .map_err(|_| DesktopActionError::Argument("Bounded exact control labels required."))?;
        (self.controls)(desktop, labels)
    }

    #[cfg(all(windows, target_pointer_width = "64"))]
    pub fn windows() -> Self {
        Self::with_application(|id| {
            Ok(Some(Box::new(super::WindowsDesktopApplication::new(
                id.ok_or(DesktopActionError::Argument(
                    "An exact packaged application ID is required.",
                ))?,
            )?)))
        })
    }
    pub fn observe(&self, id: &str) -> ActionResult<DesktopObservation> {
        validate_application(id)?;
        let mut application =
            (self.application)(Some(id))?.ok_or(DesktopActionError::Unavailable)?;
        Ok(application
            .observe()?
            .unwrap_or_else(DesktopObservation::unavailable))
    }
    pub fn launch(
        &self,
        id: Option<&str>,
        start_if_missing: bool,
        current: &dyn Fn() -> ActionResult<bool>,
    ) -> ActionResult<DesktopLaunchResult> {
        // Unlike ObserveAsync, the original Launch delegates ID validation to
        // the application factory. Preserve null/custom-factory behavior too.
        let mut application = Application((self.application)(id)?);
        Ok(launch_desktop(
            &mut CurrentApplication {
                application: &mut application,
                current,
            },
            start_if_missing,
        ))
    }
}
#[cfg(all(windows, target_pointer_width = "64"))]
fn platform_process(
    root: &AudioDesktopIdentity,
    pid: i32,
) -> ActionResult<Option<AudioProcessOwnership>> {
    super::WindowsAudioProcessSnapshot::observe(root, pid).map(Some)
}
#[cfg(not(all(windows, target_pointer_width = "64")))]
fn platform_process(
    _: &AudioDesktopIdentity,
    _: i32,
) -> ActionResult<Option<AudioProcessOwnership>> {
    Err(DesktopActionError::Platform("Windows x64 required."))
}
#[cfg(all(windows, target_pointer_width = "64"))]
fn platform_capture_owner(
    root: &AudioDesktopIdentity,
    endpoint: &JsString,
) -> ActionResult<Option<DesktopCaptureCorrelation>> {
    super::WindowsAudioProcessSnapshot::capture_owner(root, endpoint).map(Some)
}
#[cfg(not(all(windows, target_pointer_width = "64")))]
fn platform_capture_owner(
    _: &AudioDesktopIdentity,
    _: &JsString,
) -> ActionResult<Option<DesktopCaptureCorrelation>> {
    Err(DesktopActionError::Platform("Windows x64 required."))
}
#[cfg(all(windows, target_pointer_width = "64"))]
fn platform_capture(id: &JsString) -> ActionResult<CaptureObservation> {
    super::WindowsCaptureEndpoint::observe(id)
}
#[cfg(not(all(windows, target_pointer_width = "64")))]
fn platform_capture(_: &JsString) -> ActionResult<CaptureObservation> {
    Err(DesktopActionError::Platform("Windows x64 required."))
}
#[cfg(all(windows, target_pointer_width = "64"))]
fn platform_controls(
    desktop: &AudioDesktopIdentity,
    labels: &[JsString],
) -> ActionResult<Option<ControlsObservation>> {
    use super::{DesktopOwner, WindowsControlSource, WindowsDesktopProcessOwner, read_controls};
    let owner = std::cell::RefCell::new(WindowsDesktopProcessOwner::new(
        desktop.pid(),
        desktop.creation_ticks(),
        desktop.image(),
    )?);
    read_controls(
        labels,
        &|| owner.borrow_mut().is_current().unwrap_or(false),
        || WindowsControlSource::new(desktop.pid(), false, None),
        None,
        None,
    )
    .map(Some)
    .map_err(|_| DesktopActionError::Argument("Bounded exact control labels required."))
}
#[cfg(not(all(windows, target_pointer_width = "64")))]
fn platform_controls(
    _: &AudioDesktopIdentity,
    _: &[JsString],
) -> ActionResult<Option<ControlsObservation>> {
    Err(DesktopActionError::Platform("Windows x64 required."))
}
struct Application(Option<Box<dyn DesktopApplication>>);
impl DesktopApplication for Application {
    fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
        self.0
            .as_mut()
            .ok_or(DesktopActionError::Unavailable)?
            .observe()
    }
    fn activate(&mut self) -> ActionResult<i32> {
        self.0
            .as_mut()
            .ok_or(DesktopActionError::Unavailable)?
            .activate()
    }
}
