//! The narrow UIAutomation2 ABI used by the original companion. Unused SDK
//! slots are opaque. Interfaces/VARIANTs stay on their owning COM thread.
use super::{ControlError, provider};
use ivy_contracts::json::JsString;
use std::{ffi::c_void, marker::PhantomData, ptr::NonNull, rc::Rc};
use windows_sys::core::GUID;

pub(super) type Raw = *mut c_void;
type Hr = i32;
const EMPTY: u16 = 0;
pub(super) const CLSID: GUID = GUID::from_u128(0xe22ad333_b25f_460c_83d0_0581107395c9);
pub(super) const IID: GUID = GUID::from_u128(0x34723aff_0c9d_49d0_9896_7ab52df8cd8a);
pub(super) const INVOKE_IID: GUID = GUID::from_u128(0xfb377fbe_8ea6_46d5_9c73_6499642d3059);
#[repr(C)]
pub(super) struct Unknown {
    pub query: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> Hr,
    pub add_ref: unsafe extern "system" fn(Raw) -> u32,
    pub release: unsafe extern "system" fn(Raw) -> u32,
}
#[repr(C)]
pub(super) struct Automation {
    pub unknown: Unknown,
    _before_element: [usize; 3],
    pub element: unsafe extern "system" fn(Raw, isize, *mut Raw) -> Hr,
    _before_tree: [usize; 6],
    pub tree: unsafe extern "system" fn(Raw, Raw, *mut Raw) -> Hr,
    _before_raw: [usize; 2],
    pub raw_walker: unsafe extern "system" fn(Raw, *mut Raw) -> Hr,
    _before_cache: [usize; 3],
    pub cache: unsafe extern "system" fn(Raw, *mut Raw) -> Hr,
    _before_property: [usize; 2],
    pub property: unsafe extern "system" fn(Raw, i32, Variant, *mut Raw) -> Hr,
    _before_focus: [usize; 35],
    pub auto_focus: unsafe extern "system" fn(Raw, i32) -> Hr,
    _before_connection: usize,
    pub connection_timeout: unsafe extern "system" fn(Raw, u32) -> Hr,
    _before_transaction: usize,
    pub transaction_timeout: unsafe extern "system" fn(Raw, u32) -> Hr,
}
#[repr(C)]
pub(super) struct Element {
    pub unknown: Unknown,
    _before_find: [usize; 5],
    pub find_all: unsafe extern "system" fn(Raw, i32, Raw, Raw, *mut Raw) -> Hr,
    pub update: unsafe extern "system" fn(Raw, Raw, *mut Raw) -> Hr,
    _current: usize,
    pub current_ex: unsafe extern "system" fn(Raw, i32, i32, *mut Variant) -> Hr,
    _cached: usize,
    pub cached_ex: unsafe extern "system" fn(Raw, i32, i32, *mut Variant) -> Hr,
    pub pattern: unsafe extern "system" fn(Raw, i32, *const GUID, *mut Raw) -> Hr,
}
#[repr(C)]
pub(super) struct Elements {
    pub unknown: Unknown,
    pub length: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
    pub at: unsafe extern "system" fn(Raw, i32, *mut Raw) -> Hr,
}
#[repr(C)]
pub(super) struct Cache {
    pub unknown: Unknown,
    pub add: unsafe extern "system" fn(Raw, i32) -> Hr,
}
#[repr(C)]
pub(super) struct Walker {
    pub unknown: Unknown,
    _parent: usize,
    _siblings: [usize; 4],
    pub normalize: unsafe extern "system" fn(Raw, Raw, *mut Raw) -> Hr,
    pub parent_cached: unsafe extern "system" fn(Raw, Raw, Raw, *mut Raw) -> Hr,
}
#[repr(C)]
pub(super) struct Invoke {
    pub unknown: Unknown,
    pub invoke: unsafe extern "system" fn(Raw) -> Hr,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) union Data {
    pub integer: i32,
    pub boolean: i16,
    pub pointer: Raw,
    _alignment: [u64; 2],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct Variant {
    pub kind: u16,
    _reserved: [u16; 3],
    pub data: Data,
}
impl Variant {
    pub fn empty() -> Self {
        Self {
            kind: EMPTY,
            _reserved: [0; 3],
            data: Data { _alignment: [0; 2] },
        }
    }
    pub fn integer(value: i32) -> Self {
        Self {
            kind: 3,
            data: Data { integer: value },
            ..Self::empty()
        }
    }
}
#[link(name = "ole32")]
unsafe extern "system" {
    pub(super) fn CoInitializeEx(reserved: Raw, mode: u32) -> Hr;
    pub(super) fn CoUninitialize();
    fn CoCreateInstance(
        class: *const GUID,
        outer: Raw,
        context: u32,
        iid: *const GUID,
        result: *mut Raw,
    ) -> Hr;
}
#[link(name = "oleaut32")]
unsafe extern "system" {
    fn VariantClear(value: *mut Variant) -> Hr;
    fn VariantCopyInd(destination: *mut Variant, source: *const Variant) -> Hr;
    fn SysStringLen(value: *const u16) -> u32;
    fn SafeArrayGetDim(array: Raw) -> u32;
    fn SafeArrayGetLBound(array: Raw, dimension: u32, bound: *mut i32) -> Hr;
    fn SafeArrayGetUBound(array: Raw, dimension: u32, bound: *mut i32) -> Hr;
    fn SafeArrayAccessData(array: Raw, data: *mut Raw) -> Hr;
    fn SafeArrayUnaccessData(array: Raw) -> Hr;
}

pub(super) fn check(result: Hr) -> Result<(), ControlError> {
    if result == 0 {
        Ok(())
    } else {
        Err(provider(
            if result < 0 {
                result
            } else {
                0x80004005u32 as i32
            },
            None,
            "Desktop accessibility call failed.",
        ))
    }
}
#[derive(Clone, Copy)]
pub(super) struct Handle(NonNull<c_void>);
impl Handle {
    pub fn raw(self) -> Raw {
        self.0.as_ptr()
    }
    /// SAFETY: the handle came from the corresponding documented COM interface;
    /// its owner retains the reference for every vtable call in this scope.
    pub unsafe fn table<T>(&self) -> &T {
        unsafe { &**(self.raw() as *const *const T) }
    }
}
pub(super) struct Reference {
    handle: Handle,
    _thread: PhantomData<Rc<()>>,
}
impl Reference {
    /// Transfer one native out-parameter reference to the source owner.
    pub fn take(raw: Raw) -> Result<Self, ControlError> {
        NonNull::new(raw)
            .map(|ptr| Self {
                handle: Handle(ptr),
                _thread: PhantomData,
            })
            .ok_or_else(|| {
                provider(
                    0x80004005u32 as i32,
                    None,
                    "Desktop accessibility unavailable.",
                )
            })
    }
    pub fn handle(&self) -> Handle {
        self.handle
    }
}
impl Drop for Reference {
    fn drop(&mut self) {
        unsafe {
            (self.handle.table::<Unknown>().release)(self.handle.raw());
        }
    }
}
pub(super) fn create() -> Result<Reference, ControlError> {
    let mut pointer = std::ptr::null_mut();
    // CLSCTX_SERVER matches the original COM activation; no remote machine is supplied.
    let status =
        unsafe { CoCreateInstance(&CLSID, std::ptr::null_mut(), 0x15, &IID, &mut pointer) };
    let owned = Reference::take(pointer);
    check(status)?;
    owned
}
pub(super) enum Value {
    Integer(i32),
    Boolean(bool),
    String(JsString),
    Integers(Vec<i32>),
    Other,
}
pub(super) struct OwnedVariant(pub Variant);
impl OwnedVariant {
    pub fn empty() -> Self {
        Self(Variant::empty())
    }
    pub fn clear(&mut self) -> Result<(), ControlError> {
        check(unsafe { VariantClear(&mut self.0) })
    }
    pub fn value(&mut self, references: &mut Vec<Reference>) -> Result<Value, ControlError> {
        // CLR property type checks see the dereferenced value, rather than its
        // VARIANT storage form. Let OLE copy it before the provider can reuse it.
        if self.0.kind & 0x4000 != 0 {
            let mut copied = Self::empty();
            check(unsafe { VariantCopyInd(&mut copied.0, &self.0) })?;
            return copied.value(references);
        }
        // Every pointer and VARTYPE is supplied by the Windows COM marshaler.
        // Keep native unknown properties for the same enclosing source scope.
        Ok(unsafe {
            match self.0.kind {
                3 | 22 => Value::Integer(self.0.data.integer),
                11 => Value::Boolean(self.0.data.boolean != 0),
                8 => {
                    let ptr = self.0.data.pointer as *const u16;
                    let len = SysStringLen(ptr) as usize;
                    Value::String(JsString(if len == 0 {
                        Vec::new()
                    } else {
                        std::slice::from_raw_parts(ptr, len).to_vec()
                    }))
                }
                0x2003 | 0x2016 => {
                    let ptr = self.0.data.pointer;
                    if ptr.is_null() || SafeArrayGetDim(ptr) != 1 {
                        return Ok(Value::Other);
                    }
                    let (mut lower, mut upper) = (0, 0);
                    check(SafeArrayGetLBound(ptr, 1, &mut lower))?;
                    check(SafeArrayGetUBound(ptr, 1, &mut upper))?;
                    // CLR's int[] property check requires a zero-based vector.
                    if lower != 0 {
                        return Ok(Value::Other);
                    }
                    let count = i64::from(upper) - i64::from(lower) + 1;
                    let count = usize::try_from(count).map_err(|_| {
                        provider(0x80004005u32 as i32, None, "Invalid accessibility array.")
                    })?;
                    if count > isize::MAX as usize / std::mem::size_of::<i32>() {
                        return Err(provider(
                            0x80004005u32 as i32,
                            None,
                            "Invalid accessibility array.",
                        ));
                    }
                    let mut data = std::ptr::null_mut();
                    check(SafeArrayAccessData(ptr, &mut data))?;
                    let values = if count == 0 {
                        Vec::new()
                    } else if data.is_null() {
                        let _ = SafeArrayUnaccessData(ptr);
                        return Err(provider(
                            0x80004005u32 as i32,
                            None,
                            "Invalid accessibility array.",
                        ));
                    } else {
                        std::slice::from_raw_parts(data as *const i32, count).to_vec()
                    };
                    check(SafeArrayUnaccessData(ptr))?;
                    Value::Integers(values)
                }
                9 | 13 => {
                    let raw = self.0.data.pointer;
                    if !raw.is_null() {
                        let owned = Reference::take(raw)?;
                        self.0.kind = EMPTY;
                        references.push(owned);
                    }
                    Value::Other
                }
                _ => Value::Other,
            }
        })
    }
}
impl Drop for OwnedVariant {
    fn drop(&mut self) {
        unsafe {
            VariantClear(&mut self.0);
        }
    }
}
