//! UTF-16 locale ordering and case mapping using the packaged ICU 78.3 runtime. Load it from the
//! selected executable directory; callers must stage the matching full data and
//! libraries. There is no system-library, ordinal or replacement-text fallback.
use crate::{
    IvyError, IvyResult, JsString,
    collation_loader::{Library, unavailable},
};
use std::{
    cmp::Ordering,
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

type Open = unsafe extern "C" fn(*const c_char, *mut i32) -> *mut c_void;
type Close = unsafe extern "C" fn(*mut c_void);
type Attribute = unsafe extern "C" fn(*mut c_void, i32, i32, *mut i32);
type Compare = unsafe extern "C" fn(*const c_void, *const u16, i32, *const u16, i32) -> i32;
type DefaultLocale = unsafe extern "C" fn() -> *const c_char;
type Lowercase =
    unsafe extern "C" fn(*mut u16, i32, *const u16, i32, *const c_char, *mut i32) -> i32;
type Version = unsafe extern "C" fn(*mut u8);

/// One loaded, version-checked locale runtime shared by independent collators.
pub struct LocaleRuntime {
    open: Open,
    close: Close,
    attribute: Attribute,
    compare: Compare,
    default_locale: DefaultLocale,
    lowercase: Lowercase,
    // Dependents unload before common/data; collators retain this entire owner.
    _libraries: [Library; 3],
}

impl LocaleRuntime {
    /// Load only the named libraries in an explicit, absolute package directory.
    /// The build/package path owns their source identity and full-data selection.
    pub fn load(directory: &Path) -> IvyResult<Arc<Self>> {
        #[cfg(windows)]
        let names = ["icudt78.dll", "icuuc78.dll", "icuin78.dll"];
        #[cfg(not(windows))]
        let names = ["libicudata.so.78", "libicuuc.so.78", "libicui18n.so.78"];
        let data = Library::open(&directory.join(names[0]))?;
        let common = Library::open(&directory.join(names[1]))?;
        let international = Library::open(&directory.join(names[2]))?;
        macro_rules! symbol {
            ($library:expr, $name:literal, $signature:ty) => {
                // Names and signatures are from the pinned ICU 78 C ABI.
                unsafe { std::mem::transmute::<*mut c_void, $signature>($library.symbol($name)?) }
            };
        }
        let version = symbol!(common, c"u_getVersion_78", Version);
        let unicode = symbol!(common, c"u_getUnicodeVersion_78", Version);
        let mut observed = [0; 4];
        unsafe {
            version(observed.as_mut_ptr());
        }
        if observed != [78, 3, 0, 0] {
            return Err(unavailable());
        }
        unsafe {
            unicode(observed.as_mut_ptr());
        }
        if observed != [17, 0, 0, 0] {
            return Err(unavailable());
        }
        Ok(Arc::new(Self {
            open: symbol!(international, c"ucol_open_78", Open),
            close: symbol!(international, c"ucol_close_78", Close),
            attribute: symbol!(international, c"ucol_setAttribute_78", Attribute),
            compare: symbol!(international, c"ucol_strcoll_78", Compare),
            default_locale: symbol!(common, c"uloc_getDefault_78", DefaultLocale),
            lowercase: symbol!(common, c"u_strToLower_78", Lowercase),
            _libraries: [international, common, data],
        }))
    }

    pub fn from_current_executable() -> IvyResult<Arc<Self>> {
        let executable = std::env::current_exe().map_err(|_| unavailable())?;
        Self::load(executable.parent().ok_or_else(unavailable)?)
    }

    /// String.localeCompare without an explicit locale, using the executing ICU
    /// default and V8's c/en_US_POSIX translation. Resolve once, like Intl.Collator.
    pub fn executing_default(self: &Arc<Self>) -> IvyResult<LocaleComparator> {
        let raw = unsafe { (self.default_locale)() };
        if raw.is_null() {
            return Err(unavailable());
        }
        let locale = unsafe { CStr::from_ptr(raw) };
        self.collator(match locale.to_bytes() {
            b"c" | b"en_US_POSIX" => c"en-US",
            _ => locale,
        })
    }

    /// String.localeCompare with the explicit English locale and default options.
    pub fn english(self: &Arc<Self>) -> IvyResult<LocaleComparator> {
        self.collator(c"en")
    }

    fn collator(self: &Arc<Self>, locale: &CStr) -> IvyResult<LocaleComparator> {
        let mut status = 0;
        let raw = NonNull::new(unsafe { (self.open)(locale.as_ptr(), &mut status) })
            .ok_or_else(unavailable)?;
        let native = NativeCollator {
            raw,
            close: self.close,
        };
        if status > 0 {
            return Err(unavailable());
        }
        status = 0;
        unsafe {
            // V8 default sort sensitivity=variant, with normalization enabled.
            (self.attribute)(raw.as_ptr(), 4, 17, &mut status);
            (self.attribute)(raw.as_ptr(), 5, 2, &mut status);
        }
        if status > 0 {
            return Err(unavailable());
        }
        Ok(LocaleComparator {
            native: Mutex::new(native),
            locale: locale.to_owned(),
            runtime: self.clone(),
        })
    }
}

struct NativeCollator {
    raw: NonNull<c_void>,
    close: Close,
}
// ICU collators are not thread-affine. The mutex serializes every access, so no
// concurrent-call assumption or cross-thread use of a mutable ICU object is needed.
unsafe impl Send for NativeCollator {}
impl Drop for NativeCollator {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.raw.as_ptr());
        }
    }
}

/// Reusable Send+Sync locale owner. Equal collation keys stay equal; stable sorting
/// remains the caller's job, with no ordinal tie-break or UTF-16 replacement.
pub struct LocaleComparator {
    // Close the collator before releasing its runtime libraries.
    native: Mutex<NativeCollator>,
    locale: CString,
    runtime: Arc<LocaleRuntime>,
}
impl LocaleComparator {
    /// Lowercase a whole UTF-16 string using the locale captured for this owner.
    /// Context-sensitive mappings may change its length. Unpaired units remain
    /// unchanged; no UTF-8 conversion, case folding or locale fallback occurs.
    pub fn lowercase(&self, value: &JsString) -> IvyResult<JsString> {
        let too_large = || {
            IvyError::new(
                "content_too_large",
                "String exceeds the locale runtime limit.",
            )
        };
        let length = i32::try_from(value.0.len()).map_err(|_| too_large())?;
        let mut status = 0;
        // Preflight provides the exact expanded size, without writing output.
        let required = unsafe {
            (self.runtime.lowercase)(
                std::ptr::null_mut(),
                0,
                value.0.as_ptr(),
                length,
                self.locale.as_ptr(),
                &mut status,
            )
        };
        const BUFFER_OVERFLOW: i32 = 15;
        if required < 0 || (status > 0 && status != BUFFER_OVERFLOW) {
            return Err(unavailable());
        }
        let capacity = required.checked_add(1).ok_or_else(too_large)?;
        let mut units = vec![0; capacity as usize];
        status = 0;
        let written = unsafe {
            (self.runtime.lowercase)(
                units.as_mut_ptr(),
                capacity,
                value.0.as_ptr(),
                length,
                self.locale.as_ptr(),
                &mut status,
            )
        };
        if status > 0 || written < 0 || written >= capacity {
            return Err(unavailable());
        }
        units.truncate(written as usize);
        Ok(JsString(units))
    }

    pub fn compare(&self, left: &JsString, right: &JsString) -> Ordering {
        // JavaScript's maximum string length is below ICU's signed-i32 length.
        let left_len = i32::try_from(left.0.len()).expect("JavaScript string length");
        let right_len = i32::try_from(right.0.len()).expect("JavaScript string length");
        let native = self.native.lock().unwrap();
        let compared = unsafe {
            (self.runtime.compare)(
                native.raw.as_ptr(),
                left.0.as_ptr(),
                left_len,
                right.0.as_ptr(),
                right_len,
            )
        };
        compared.cmp(&0)
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/sdk-locale-case.rs"]
mod casing_tests;
