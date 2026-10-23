//! Narrow ABI 2 boundary around the separately packaged reference codec.
use super::{AudioFormat, Codec, CodecError, evs_header_full, evs_sender};
use std::{cell::Cell, ffi::c_void, marker::PhantomData, path::Path, ptr::NonNull, sync::Arc};

type Create = unsafe extern "C" fn(i32, i32, i32, i32, *mut *mut c_void) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);
type Encode = unsafe extern "C" fn(*mut c_void, *const i16, i32, *mut u8, i32, i32) -> i32;
type Decode = unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut i16, i32, i32) -> i32;

struct Functions {
    create: Create,
    destroy: Destroy,
    encode: Encode,
    decode: Decode,
}

pub struct EvsLibrary {
    functions: Functions,
    #[cfg(windows)]
    _module: Option<Module>,
}

impl EvsLibrary {
    pub const FILE_NAME: &'static str = "ivy_phone_evs.dll";

    /// Load only the fixed file in an absolute, verified package directory.
    /// Dependencies are restricted to that directory and the system directory.
    ///
    /// # Safety
    /// The caller must verify the immutable package's native library and its
    /// dependencies before loading. Its exports must implement phone_evs.h ABI
    /// 2, including buffer bounds, serialized independent handles and destroy
    /// on partial create failure. An ABI number cannot authenticate machine code.
    pub unsafe fn load_packaged(directory: &Path) -> Result<Arc<Self>, CodecError> {
        if !directory.is_absolute() {
            return Err(CodecError::InvalidFormat);
        }
        #[cfg(windows)]
        {
            let module = Module::load(&directory.join(Self::FILE_NAME))?;
            // SAFETY: The caller authenticates the native code/signatures. Each
            // address is checked for NULL; module remains owned through every call.
            let abi: unsafe extern "C" fn() -> i32 =
                unsafe { std::mem::transmute(module.symbol(b"ivy_phone_evs_abi\0")?) };
            if unsafe { abi() } != 2 {
                return Err(CodecError::Failed);
            }
            let functions = Functions {
                create: unsafe { std::mem::transmute(module.symbol(b"ivy_phone_evs_create\0")?) },
                destroy: unsafe { std::mem::transmute(module.symbol(b"ivy_phone_evs_destroy\0")?) },
                encode: unsafe { std::mem::transmute(module.symbol(b"ivy_phone_evs_encode\0")?) },
                decode: unsafe { std::mem::transmute(module.symbol(b"ivy_phone_evs_decode\0")?) },
            };
            Ok(Arc::new(Self {
                functions,
                _module: Some(module),
            }))
        }
        #[cfg(not(windows))]
        {
            Err(CodecError::Failed)
        }
    }
}

#[cfg(windows)]
struct Module(NonNull<c_void>);
#[cfg(windows)]
impl Module {
    fn load(path: &Path) -> Result<Self, CodecError> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
        };
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.is_empty() || wide.len() >= 32767 || wide.contains(&0) {
            return Err(CodecError::InvalidFormat);
        }
        wide.push(0);
        // SAFETY: Absolute path, bounded NUL-terminated UTF16, no borrowed
        // native state; ownership of the returned module reference is unique.
        NonNull::new(unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        })
        .map(Self)
        .ok_or(CodecError::Failed)
    }
    fn symbol(
        &self,
        name: &'static [u8],
    ) -> Result<unsafe extern "system" fn() -> isize, CodecError> {
        // SAFETY: All names are fixed NUL-terminated ASCII and this module is live.
        unsafe {
            windows_sys::Win32::System::LibraryLoader::GetProcAddress(
                self.0.as_ptr(),
                name.as_ptr(),
            )
        }
        .ok_or(CodecError::Failed)
    }
}
#[cfg(windows)]
impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: Exactly this owned LoadLibrary reference is released once,
        // after the last codec handle has called its destroy function.
        unsafe {
            windows_sys::Win32::Foundation::FreeLibrary(self.0.as_ptr());
        }
    }
}
// SAFETY: The module/function table is immutable. The ABI permits independent
// codec handles; each handle below is exclusively mutable and is never Sync.
unsafe impl Send for EvsLibrary {}
unsafe impl Sync for EvsLibrary {}

struct Handle {
    pointer: Option<NonNull<c_void>>,
    library: Arc<EvsLibrary>,
    _exclusive: PhantomData<Cell<()>>,
}
// SAFETY: ABI2 handles have no thread affinity. Moving transfers sole ownership;
// all use/destroy requires &mut, and Cell keeps this type !Sync.
unsafe impl Send for Handle {}
impl Handle {
    fn new(
        library: Arc<EvsLibrary>,
        encoding: bool,
        bitrate: u32,
        bandwidth: u8,
        channels: u8,
    ) -> Result<Self, CodecError> {
        if !matches!(channels, 1 | 2)
            || ![
                7200, 8000, 9600, 13200, 16400, 24400, 32000, 48000, 64000, 96000, 128000,
            ]
            .contains(&bitrate)
            || bandwidth > 3
            || bandwidth == 0 && bitrate > 24400
            || bandwidth == 2 && bitrate < 9600
            || bandwidth == 3 && bitrate < 16400
        {
            return Err(CodecError::InvalidFormat);
        }
        let mut pointer = std::ptr::null_mut();
        // SAFETY: Valid fixed-width ABI arguments and one writable out-pointer.
        // The validated module is retained by this constructor and the handle.
        let result = unsafe {
            (library.functions.create)(
                encoding.into(),
                bitrate as i32,
                bandwidth.into(),
                channels.into(),
                &mut pointer,
            )
        };
        let mut handle = Self {
            pointer: NonNull::new(pointer),
            library,
            _exclusive: PhantomData,
        };
        if result != 0 || handle.pointer.is_none() {
            handle.close();
            return Err(CodecError::Failed);
        }
        Ok(handle)
    }
    fn pointer(&self) -> Result<*mut c_void, CodecError> {
        self.pointer.map(NonNull::as_ptr).ok_or(CodecError::Failed)
    }
    fn close(&mut self) {
        if let Some(pointer) = self.pointer.take() {
            // SAFETY: Pointer belongs exclusively to this handle. Take clears
            // it before destroy, so failure/drop cannot free it twice. Library
            // remains retained until after the function returns.
            unsafe {
                (self.library.functions.destroy)(pointer.as_ptr());
            }
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.close();
    }
}

pub struct EvsEncoder {
    handle: Handle,
    channels: u8,
    bitrate: u32,
    full: bool,
}
pub struct EvsDecoder {
    handle: Handle,
    full: bool,
}
impl EvsEncoder {
    pub fn new(library: Arc<EvsLibrary>, format: &AudioFormat) -> Result<Self, CodecError> {
        if format.codec() != Codec::Evs {
            return Err(CodecError::InvalidFormat);
        }
        let profile = evs_sender(format.parameters()).map_err(|_| CodecError::InvalidFormat)?;
        let full = evs_header_full(format.parameters()).map_err(|_| CodecError::InvalidFormat)?;
        let channels = format.channels();
        let handle = Handle::new(library, true, profile.bitrate, profile.bandwidth, channels)?;
        Ok(Self {
            handle,
            channels,
            bitrate: profile.bitrate,
            full,
        })
    }
    pub fn encode_20ms(&mut self, pcm: &[i16; 960]) -> Result<Vec<u8>, CodecError> {
        let pointer = self.handle.pointer()?;
        let expected =
            self.channels as usize * (self.bitrate as usize / 400 + usize::from(self.full));
        let mut packet = vec![0; self.channels as usize * 321];
        // SAFETY: Exclusive live encoding handle; exact960-sample read-only PCM,
        // and writable output capacity explicitly passed with both buffers live.
        let count = unsafe {
            (self.handle.library.functions.encode)(
                pointer,
                pcm.as_ptr(),
                960,
                packet.as_mut_ptr(),
                packet.len() as i32,
                self.full.into(),
            )
        };
        if count != expected as i32 {
            packet.fill(0);
            self.handle.close();
            return Err(CodecError::Failed);
        }
        packet.truncate(expected);
        Ok(packet)
    }
}
impl EvsDecoder {
    pub fn new(library: Arc<EvsLibrary>, format: &AudioFormat) -> Result<Self, CodecError> {
        if format.codec() != Codec::Evs {
            return Err(CodecError::InvalidFormat);
        }
        let full = evs_header_full(format.parameters()).map_err(|_| CodecError::InvalidFormat)?;
        let handle = Handle::new(library, false, 128000, 3, format.channels())?;
        Ok(Self { handle, full })
    }
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, CodecError> {
        if packet.is_empty() || packet.len() > 4096 {
            return Err(CodecError::InvalidFrame);
        }
        let pointer = self.handle.pointer()?;
        let mut pcm = vec![0; 5760];
        // SAFETY: Exclusive live decoding handle, bounded immutable packet and
        // explicitly sized writable PCM allocation; ABI never borrows either.
        let count = unsafe {
            (self.handle.library.functions.decode)(
                pointer,
                packet.as_ptr(),
                packet.len() as i32,
                pcm.as_mut_ptr(),
                5760,
                self.full.into(),
            )
        };
        if (960..=5760).contains(&count) && count % 960 == 0 {
            pcm.truncate(count as usize);
            return Ok(pcm);
        }
        pcm.fill(0);
        if count == -3 {
            return Err(CodecError::InvalidFrame);
        } // Validated bad payload leaves native history unchanged.
        self.handle.close();
        Err(CodecError::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering::SeqCst};
    static MODE: AtomicI32 = AtomicI32::new(0);
    static CREATED: AtomicI32 = AtomicI32::new(0);
    static DESTROYED: AtomicI32 = AtomicI32::new(0);
    unsafe extern "C" fn create(_: i32, _: i32, _: i32, _: i32, out: *mut *mut c_void) -> i32 {
        let mode = MODE.load(SeqCst);
        if mode == 1 {
            return 0;
        }
        // SAFETY: The wrapper supplies a valid writable out-pointer.
        unsafe {
            *out = Box::into_raw(Box::new(0_u8)).cast();
        }
        CREATED.fetch_add(1, SeqCst);
        if mode == 2 { -2 } else { 0 }
    }
    unsafe extern "C" fn destroy(pointer: *mut c_void) {
        // SAFETY: This test constructor allocated exactly this pointer once.
        unsafe {
            drop(Box::from_raw(pointer.cast::<u8>()));
        }
        DESTROYED.fetch_add(1, SeqCst);
    }
    unsafe extern "C" fn encode(
        _: *mut c_void,
        _: *const i16,
        _: i32,
        out: *mut u8,
        _: i32,
        _: i32,
    ) -> i32 {
        // SAFETY: The wrapper supplies at least321 writable bytes.
        unsafe {
            *out = 123;
        }
        match MODE.load(SeqCst) {
            3 => 643,
            4 => -4,
            _ => 320,
        }
    }
    unsafe extern "C" fn decode(
        _: *mut c_void,
        _: *const u8,
        _: i32,
        out: *mut i16,
        _: i32,
        _: i32,
    ) -> i32 {
        // SAFETY: The wrapper supplies5760 writable samples.
        unsafe {
            *out = 123;
        }
        match MODE.load(SeqCst) {
            3 => 959,
            4 => -4,
            5 => -3,
            _ => 960,
        }
    }
    #[test]
    fn evs_partial_create_invalid_result_and_internal_failure_release_once() {
        let library = Arc::new(EvsLibrary {
            functions: Functions {
                create,
                destroy,
                encode,
                decode,
            },
            #[cfg(windows)]
            _module: None,
        });
        let format = Codec::Evs.offered_formats().remove(0);
        MODE.store(1, SeqCst);
        assert!(EvsEncoder::new(library.clone(), &format).is_err());
        MODE.store(2, SeqCst);
        assert!(EvsEncoder::new(library.clone(), &format).is_err());
        assert_eq!(CREATED.load(SeqCst), 1);
        assert_eq!(DESTROYED.load(SeqCst), 1);
        for mode in [3, 4] {
            MODE.store(0, SeqCst);
            let mut encoder = EvsEncoder::new(library.clone(), &format).unwrap();
            let mut decoder = EvsDecoder::new(library.clone(), &format).unwrap();
            MODE.store(mode, SeqCst);
            assert!(encoder.encode_20ms(&[0; 960]).is_err());
            assert!(decoder.decode(&[1]).is_err());
            let destroyed = DESTROYED.load(SeqCst);
            MODE.store(0, SeqCst);
            assert!(encoder.encode_20ms(&[0; 960]).is_err());
            assert!(decoder.decode(&[1]).is_err());
            drop(encoder);
            drop(decoder);
            assert_eq!(DESTROYED.load(SeqCst), destroyed);
        }
        MODE.store(0, SeqCst);
        let mut decoder = EvsDecoder::new(library, &format).unwrap();
        let destroyed = DESTROYED.load(SeqCst);
        MODE.store(5, SeqCst);
        assert_eq!(decoder.decode(&[1]), Err(CodecError::InvalidFrame));
        assert_eq!(DESTROYED.load(SeqCst), destroyed);
        MODE.store(0, SeqCst);
        assert_eq!(decoder.decode(&[1]).unwrap().len(), 960);
        drop(decoder);
        assert_eq!(CREATED.load(SeqCst), DESTROYED.load(SeqCst));
    }
}
