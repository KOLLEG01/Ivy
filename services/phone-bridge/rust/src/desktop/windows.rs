use super::{
    Control, ControlError, ControlSource, ControlWindow, VoiceControlIdentity, VoiceSource,
    select_named, validate_labels,
};
use ivy_contracts::json::JsString;
use std::{
    collections::{HashMap, hash_map::Entry},
    thread::{ThreadId, current},
    time::{Duration, Instant},
};
#[path = "windows_com.rs"]
mod com;
use com::{
    Automation, Cache, Element, Elements, Handle, Invoke, OwnedVariant, Raw, Reference, Value,
    Variant, Walker, check,
};

#[derive(Debug)]
pub struct AccessibilityError {
    pub hresult: i32,
    pub property_id: Option<i32>,
    message: &'static str,
}
impl std::fmt::Display for AccessibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for AccessibilityError {}
fn provider(hresult: i32, property_id: Option<i32>, message: &'static str) -> ControlError {
    ControlError::Provider(Box::new(AccessibilityError {
        hresult,
        property_id,
        message,
    }))
}
fn invalid(message: &'static str) -> ControlError {
    provider(0x80004005u32 as i32, None, message)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopInvokePhase {
    NotSubmitted,
    Submitted,
    OutcomeUnknown,
}
impl DesktopInvokePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSubmitted => "not_submitted",
            Self::Submitted => "submitted",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
}
#[derive(Clone, Debug)]
pub struct DesktopReadStage {
    pub stage: &'static str,
    pub elapsed: Duration,
    pub count: i32,
}

type WindowCallback = unsafe extern "system" fn(isize, isize) -> i32;
#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(callback: WindowCallback, data: isize) -> i32;
    fn GetWindowThreadProcessId(window: isize, pid: *mut u32) -> u32;
    fn IsWindowVisible(window: isize) -> i32;
    fn IsIconic(window: isize) -> i32;
}

/// Concrete UIAutomation2 source. The enclosing Desktop owner must retain and
/// recheck full process identity; this source filters windows by its original PID.
/// It is deliberately not Send: construction, reads, invokes and release share a thread.
pub struct WindowsControlSource {
    pid: i32,
    retain_voice: bool,
    thread: ThreadId,
    initialized: bool,
    closed: bool,
    references: Vec<Reference>,
    automation: Option<Handle>,
    control_cache: Option<Handle>,
    parent_cache: Option<Handle>,
    raw_walker: Option<Handle>,
    match_condition: Option<Handle>,
    prepared_labels: Option<Vec<JsString>>,
    controls: HashMap<Vec<i32>, (JsString, Handle)>,
    walkers: HashMap<JsString, Handle>,
    on_stage: Option<Box<dyn FnMut(DesktopReadStage)>>,
}
impl WindowsControlSource {
    pub fn new(
        pid: i32,
        retain_voice: bool,
        on_stage: Option<Box<dyn FnMut(DesktopReadStage)>>,
    ) -> Result<Self, ControlError> {
        let mut source = Self {
            pid,
            retain_voice,
            thread: current().id(),
            initialized: false,
            closed: false,
            references: Vec::new(),
            automation: None,
            control_cache: None,
            parent_cache: None,
            raw_walker: None,
            match_condition: None,
            prepared_labels: None,
            controls: HashMap::new(),
            walkers: HashMap::new(),
            on_stage,
        };
        let result = unsafe { com::CoInitializeEx(std::ptr::null_mut(), 0) };
        if result != 0 && result != 1 {
            check(result)?;
        }
        source.initialized = true;
        let automation = com::create()?;
        let handle = automation.handle();
        source.references.push(automation);
        source.automation = Some(handle);
        unsafe {
            let table = handle.table::<Automation>();
            check((table.auto_focus)(handle.raw(), 0))?;
            check((table.connection_timeout)(handle.raw(), 250))?;
            check((table.transaction_timeout)(handle.raw(), 500))?;
        }
        Ok(source)
    }
    fn check_thread(&self) -> Result<(), ControlError> {
        if current().id() != self.thread {
            Err(ControlError::WrongThread)
        } else if self.closed {
            Err(invalid("Desktop accessibility source closed."))
        } else {
            Ok(())
        }
    }
    fn keep(&mut self, pointer: Raw) -> Result<Handle, ControlError> {
        let owned = Reference::take(pointer)?;
        let handle = owned.handle();
        self.references.push(owned);
        Ok(handle)
    }
    fn out(&mut self, call: impl FnOnce(*mut Raw) -> i32) -> Result<Handle, ControlError> {
        let mut pointer = std::ptr::null_mut();
        check(call(&mut pointer))?;
        self.keep(pointer)
    }
    fn windows(&self) -> Result<Vec<(isize, bool)>, ControlError> {
        struct Inventory {
            pid: u32,
            windows: Vec<(isize, bool)>,
            overflow: bool,
        }
        unsafe extern "system" fn observe(window: isize, data: isize) -> i32 {
            let inventory = unsafe { &mut *(data as *mut Inventory) };
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(window, &mut pid);
            }
            if pid == inventory.pid && unsafe { IsWindowVisible(window) } != 0 {
                if inventory.windows.len() == 32 {
                    inventory.overflow = true;
                    return 0;
                }
                inventory
                    .windows
                    .push((window, unsafe { IsIconic(window) } != 0));
            }
            1
        }
        let mut inventory = Inventory {
            pid: self.pid as u32,
            windows: Vec::new(),
            overflow: false,
        };
        if unsafe { EnumWindows(observe, &mut inventory as *mut Inventory as isize) } == 0
            || inventory.overflow
        {
            return Err(invalid("Desktop window inventory unavailable."));
        }
        inventory.windows.sort_by_key(|w| w.0);
        Ok(inventory.windows)
    }
    fn owner(&self, window: isize) -> bool {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(window, &mut pid);
        }
        pid == self.pid as u32
    }
    fn property(&mut self, element: Handle, id: i32, cached: bool) -> Result<Value, ControlError> {
        let mut value = OwnedVariant::empty();
        let table = unsafe { element.table::<Element>() };
        let status = unsafe {
            if cached {
                (table.cached_ex)(element.raw(), id, 1, &mut value.0)
            } else {
                (table.current_ex)(element.raw(), id, 1, &mut value.0)
            }
        };
        check(status)?;
        let result = value.value(&mut self.references);
        value.clear()?;
        result
    }
    fn property_error(&self, error: ControlError, id: i32) -> ControlError {
        let hresult = match &error {
            ControlError::Provider(error) => error
                .downcast_ref::<AccessibilityError>()
                .map(|e| e.hresult)
                .unwrap_or(0x80004005u32 as i32),
            _ => 0x80004005u32 as i32,
        };
        provider(
            hresult,
            Some(id),
            "Required accessibility property unavailable.",
        )
    }
    fn string(&mut self, element: Handle, id: i32) -> Result<JsString, ControlError> {
        match self
            .property(element, id, true)
            .map_err(|e| self.property_error(e, id))?
        {
            Value::String(s) => Ok(s),
            _ => Err(provider(
                0x80004005u32 as i32,
                Some(id),
                "Required accessibility property unavailable.",
            )),
        }
    }
    fn ids(&mut self, element: Handle) -> Result<Vec<i32>, ControlError> {
        match self
            .property(element, 30000, true)
            .map_err(|e| self.property_error(e, 30000))?
        {
            Value::Integers(v) => Ok(v),
            _ => Err(provider(
                0x80004005u32 as i32,
                Some(30000),
                "Required accessibility property unavailable.",
            )),
        }
    }
    fn boolean(&mut self, element: Handle, id: i32) -> Result<bool, ControlError> {
        match self
            .property(element, id, true)
            .map_err(|e| self.property_error(e, id))?
        {
            Value::Boolean(v) => Ok(v),
            _ => Err(provider(
                0x80004005u32 as i32,
                Some(id),
                "Required accessibility property unavailable.",
            )),
        }
    }
    fn toggle(&mut self, element: Handle) -> Result<Option<i32>, ControlError> {
        Ok(match self.property(element, 30086, true)? {
            Value::Integer(v) => Some(v),
            _ => None,
        })
    }
    fn stage(&mut self, phase: &mut Instant, stage: &'static str, count: i32) {
        let value = DesktopReadStage {
            stage,
            elapsed: phase.elapsed(),
            count,
        };
        if let Some(callback) = &mut self.on_stage {
            let _ = super::controls::guarded(|| {
                callback(value);
                Ok(())
            });
        }
        *phase = Instant::now();
    }
    fn release_since(&mut self, retained: usize) {
        while self.references.len() > retained {
            drop(self.references.pop());
        }
    }
    pub fn invoke(
        &mut self,
        window: &JsString,
        control: &Control,
        current: &dyn Fn() -> bool,
    ) -> Result<DesktopInvokePhase, ControlError> {
        self.check_thread()?;
        let retained = self.references.len();
        let result = (|| {
            if !current() {
                return Ok(DesktopInvokePhase::NotSubmitted);
            }
            let windows = self.windows()?;
            let &(hwnd, minimized) = windows
                .iter()
                .find(|(h, _)| JsString::from(h.to_string()) == *window)
                .ok_or_else(|| invalid("Original Voice window unavailable."))?;
            let Some((bound_window, bound)) = self.controls.get(&control.id).cloned() else {
                return Ok(DesktopInvokePhase::NotSubmitted);
            };
            if minimized || bound_window != *window {
                return Ok(DesktopInvokePhase::NotSubmitted);
            }
            let cache = self
                .control_cache
                .ok_or_else(|| invalid("Desktop accessibility query unavailable."))?;
            let element = self.out(|out| unsafe {
                (bound.table::<Element>().update)(bound.raw(), cache.raw(), out)
            })?;
            if self.ids(element)? != control.id
                || self.string(element, 30005)? != control.name
                || !self.boolean(element, 30010)?
                || self.boolean(element, 30022)?
            {
                return Ok(DesktopInvokePhase::NotSubmitted);
            }
            if !current() || windows != self.windows()? {
                return Ok(DesktopInvokePhase::NotSubmitted);
            }
            let pattern = self.out(|out| unsafe {
                (element.table::<Element>().pattern)(element.raw(), 10000, &com::INVOKE_IID, out)
            })?;
            if !current() {
                return Ok(DesktopInvokePhase::NotSubmitted);
            }
            Ok(
                if check(unsafe { (pattern.table::<Invoke>().invoke)(pattern.raw()) }).is_ok() {
                    DesktopInvokePhase::Submitted
                } else {
                    DesktopInvokePhase::OutcomeUnknown
                },
            )
        })();
        self.release_since(retained);
        result
    }
}
impl ControlSource for WindowsControlSource {
    fn prepare(&mut self, labels: &[JsString]) -> Result<(), ControlError> {
        self.check_thread()?;
        validate_labels(labels)?;
        if let Some(prepared) = &self.prepared_labels {
            return if prepared == labels {
                Ok(())
            } else {
                Err(invalid("Original accessibility query cannot change."))
            };
        }
        let automation = self.automation.unwrap();
        self.match_condition = Some(self.out(|out| unsafe {
            (automation.table::<Automation>().property)(
                automation.raw(),
                30003,
                Variant::integer(50000),
                out,
            )
        })?);
        let cache = self.out(|out| unsafe {
            (automation.table::<Automation>().cache)(automation.raw(), out)
        })?;
        self.control_cache = Some(cache);
        for id in [30000, 30005, 30010, 30022, 30086] {
            check(unsafe { (cache.table::<Cache>().add)(cache.raw(), id) })?;
        }
        let parents = self.out(|out| unsafe {
            (automation.table::<Automation>().cache)(automation.raw(), out)
        })?;
        check(unsafe { (parents.table::<Cache>().add)(parents.raw(), 30000) })?;
        self.parent_cache = Some(parents);
        self.raw_walker = Some(self.out(|out| unsafe {
            (automation.table::<Automation>().raw_walker)(automation.raw(), out)
        })?);
        self.prepared_labels = Some(labels.to_vec());
        Ok(())
    }
    fn read(&mut self, labels: &[JsString]) -> Result<Vec<ControlWindow>, ControlError> {
        self.prepare(labels)?;
        let mut phase = Instant::now();
        let windows = self.windows()?;
        self.stage(&mut phase, "window_inventory", windows.len() as i32);
        let (automation, cache, parents, walker, condition) = (
            self.automation.unwrap(),
            self.control_cache.unwrap(),
            self.parent_cache.unwrap(),
            self.raw_walker.unwrap(),
            self.match_condition.unwrap(),
        );
        let mut result = Vec::new();
        for &(window, minimized) in &windows {
            if !self.owner(window) {
                return Err(invalid("Desktop window changed."));
            }
            let element = self.out(|out| unsafe {
                (automation.table::<Automation>().element)(automation.raw(), window, out)
            })?;
            self.stage(&mut phase, "window_element", 1);
            let window_name = JsString::from(window.to_string());
            if self.retain_voice {
                let value = i32::try_from(window)
                    .map_err(|_| invalid("Desktop window handle exceeds Int32."))?;
                let condition = self.out(|out| unsafe {
                    (automation.table::<Automation>().property)(
                        automation.raw(),
                        30020,
                        Variant::integer(value),
                        out,
                    )
                })?;
                let bound_walker = self.out(|out| unsafe {
                    (automation.table::<Automation>().tree)(automation.raw(), condition.raw(), out)
                })?;
                match self.walkers.entry(window_name.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(bound_walker);
                    }
                    Entry::Occupied(_) => return Err(invalid("Duplicate Desktop window.")),
                }
            }
            let found = self.out(|out| unsafe {
                (element.table::<Element>().find_all)(
                    element.raw(),
                    4,
                    condition.raw(),
                    cache.raw(),
                    out,
                )
            })?;
            let mut count = 0;
            check(unsafe { (found.table::<Elements>().length)(found.raw(), &mut count) })?;
            self.stage(&mut phase, "button_cache", count);
            // Each enumerated interface remains owned until source release. Read
            // only names of unmatched controls, exactly as the cached source.
            let candidates = std::cell::RefCell::new(&mut *self);
            let matching = select_named(
                count,
                |index| {
                    candidates.borrow_mut().out(|out| unsafe {
                        (found.table::<Elements>().at)(found.raw(), index, out)
                    })
                },
                |candidate| candidates.borrow_mut().string(*candidate, 30005).map(Some),
                labels,
            )?;
            let this = candidates.into_inner();
            this.stage(&mut phase, "cached_names", matching.len() as i32);
            let mut controls = Vec::new();
            for control in matching {
                let parent = this.out(|out| unsafe {
                    (walker.table::<Walker>().parent_cached)(
                        walker.raw(),
                        control.raw(),
                        parents.raw(),
                        out,
                    )
                })?;
                let toggle = this.toggle(control)?;
                let id = this.ids(control)?;
                if this.retain_voice {
                    match this.controls.entry(id.clone()) {
                        Entry::Vacant(entry) => {
                            entry.insert((window_name.clone(), control));
                        }
                        Entry::Occupied(_) => return Err(invalid("Duplicate Desktop control.")),
                    }
                }
                controls.push(Control {
                    id,
                    parent_id: this.ids(parent)?,
                    name: this.string(control, 30005)?,
                    enabled: this.boolean(control, 30010)?,
                    offscreen: this.boolean(control, 30022)?,
                    toggle,
                });
            }
            if !this.owner(window) {
                return Err(invalid("Desktop window changed."));
            }
            result.push(ControlWindow {
                handle: window_name,
                minimized,
                controls,
            });
            this.stage(
                &mut phase,
                "matching_properties",
                result.last().unwrap().controls.len() as i32,
            );
        }
        if windows != self.windows()? {
            return Err(invalid("Desktop window inventory changed."));
        }
        self.stage(&mut phase, "window_recheck", windows.len() as i32);
        Ok(result)
    }
    fn close(&mut self) -> Result<(), ControlError> {
        if current().id() != self.thread {
            return Err(ControlError::WrongThread);
        }
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.release_since(0);
        self.controls.clear();
        self.walkers.clear();
        if self.initialized {
            self.initialized = false;
            unsafe {
                com::CoUninitialize();
            }
        }
        Ok(())
    }
}
impl VoiceSource for WindowsControlSource {
    fn refresh(
        &mut self,
        identity: VoiceControlIdentity,
    ) -> Result<Vec<ControlWindow>, ControlError> {
        self.check_thread()?;
        let retained = self.references.len();
        let result = (|| {
            let windows = self.windows()?;
            let &(window, minimized) = windows
                .iter()
                .find(|(w, _)| JsString::from(w.to_string()) == identity.window)
                .ok_or_else(|| invalid("Original Voice window unavailable."))?;
            if minimized {
                return Err(invalid("Original Voice window is minimized."));
            }
            let (cache, parents, walker) = (
                self.control_cache
                    .ok_or_else(|| invalid("Desktop accessibility query unavailable."))?,
                self.parent_cache.unwrap(),
                self.raw_walker.unwrap(),
            );
            let mut refreshed = Vec::new();
            for id in [&identity.microphone_id, &identity.stop_id] {
                let (bound_window, bound) = self
                    .controls
                    .get(id)
                    .cloned()
                    .filter(|(w, _)| *w == identity.window)
                    .ok_or_else(|| invalid("Original Voice element unavailable."))?;
                let _ = bound_window;
                let element = self.out(|out| unsafe {
                    (bound.table::<Element>().update)(bound.raw(), cache.raw(), out)
                })?;
                let parent = self.out(|out| unsafe {
                    (walker.table::<Walker>().parent_cached)(
                        walker.raw(),
                        element.raw(),
                        parents.raw(),
                        out,
                    )
                })?;
                let actual_id = self.ids(element)?;
                let parent_id = self.ids(parent)?;
                if actual_id != *id || parent_id != identity.parent_id {
                    return Err(invalid("Original Voice group changed."));
                }
                let window_walker = *self
                    .walkers
                    .get(&identity.window)
                    .ok_or_else(|| invalid("Original Voice window unavailable."))?;
                let ancestor = self.out(|out| unsafe {
                    (window_walker.table::<Walker>().normalize)(
                        window_walker.raw(),
                        element.raw(),
                        out,
                    )
                })?;
                let expected = i32::try_from(window)
                    .map_err(|_| invalid("Desktop window handle exceeds Int32."))?;
                let actual = match self
                    .property(ancestor, 30020, false)
                    .map_err(|e| self.property_error(e, 30020))?
                {
                    Value::Integer(value) => value,
                    _ => {
                        return Err(provider(
                            0x80004005u32 as i32,
                            Some(30020),
                            "Required accessibility property unavailable.",
                        ));
                    }
                };
                if actual != expected {
                    return Err(invalid("Original Voice window changed."));
                }
                let toggle = self.toggle(element)?;
                refreshed.push(Control {
                    id: actual_id,
                    parent_id,
                    name: self.string(element, 30005)?,
                    enabled: self.boolean(element, 30010)?,
                    offscreen: self.boolean(element, 30022)?,
                    toggle,
                });
            }
            if windows != self.windows()? {
                return Err(invalid("Desktop window inventory changed."));
            }
            Ok(vec![ControlWindow {
                handle: identity.window,
                minimized: false,
                controls: refreshed,
            }])
        })();
        self.release_since(retained);
        result
    }
}
impl Drop for WindowsControlSource {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_windows_controls.rs"]
mod tests;
