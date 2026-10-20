//! Original SDK call identity and bounded cryptographic integer sampling.
//! SIPSorcery 10.0.15 notice: native/phone-runtime/licenses/SIPSorcery-10.0.15.md.
use super::{MediaError, SipMediaIdentity};
use std::fmt::Write;

/// Fill the complete requested buffer or fail. Implementations must finish
/// bounded work and must not re-enter the admitted call owner.
pub trait SipMediaEntropy: Send + Sync + 'static {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), MediaError>;
}

#[derive(Default)]
pub struct SystemSipMediaEntropy;
impl SipMediaEntropy for SystemSipMediaEntropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), MediaError> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Security::Cryptography::{
                BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
            };
            let length = u32::try_from(bytes.len()).map_err(|_| MediaError::EntropyUnavailable)?;
            // SAFETY: the system provider writes exactly this borrowed buffer;
            // it retains no pointer and requires a null algorithm handle.
            let status = unsafe {
                BCryptGenRandom(
                    std::ptr::null_mut(),
                    bytes.as_mut_ptr(),
                    length,
                    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                )
            };
            if status >= 0 {
                return Ok(());
            }
        }
        #[cfg(unix)]
        {
            use std::io::Read;
            if std::fs::File::open("/dev/urandom")
                .and_then(|mut file| file.read_exact(bytes))
                .is_ok()
            {
                return Ok(());
            }
        }
        Err(MediaError::EntropyUnavailable)
    }
}

/// Keep the SDK's inclusive rejection boundary and ten-attempt limit. The
/// upper accepted boundary deliberately maps to the bottom of the range.
pub(super) fn random_range<E>(
    min: u32,
    max: u32,
    mut random: impl FnMut() -> Result<u32, E>,
    unavailable: E,
) -> Result<u32, E> {
    if min > max {
        return Err(unavailable);
    }
    if min == max {
        return Ok(min);
    }
    let width = u64::from(max) - u64::from(min) + 1;
    let limit = 1_u64 << 32;
    for _ in 0..10 {
        let value = u64::from(random()?);
        if value <= limit - limit % width {
            return Ok(min + (value % width) as u32);
        }
    }
    Err(unavailable)
}

impl SipMediaIdentity {
    /// Called once inside the reserved media factory, before opening RTP/PCM.
    pub fn generate(entropy: &dyn SipMediaEntropy) -> Result<Self, MediaError> {
        let integer = |min, max| {
            random_range(
                min,
                max,
                || {
                    let mut bytes = [0; 4];
                    entropy.fill(&mut bytes)?;
                    Ok(u32::from_ne_bytes(bytes))
                },
                MediaError::EntropyUnavailable,
            )
        };
        let session_id = integer(1_000_000_000, i32::MAX as u32)?;
        let source = integer(0, i32::MAX as u32)?;
        let sequence = integer(0, u16::MAX.into())? as u16;
        let mut guid = [0; 16];
        entropy.fill(&mut guid)?;
        guid[6] = (guid[6] & 0x0f) | 0x40;
        guid[8] = (guid[8] & 0x3f) | 0x80;
        let mut text = String::with_capacity(36);
        for (i, byte) in guid.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                text.push('-');
            }
            write!(text, "{byte:02x}").expect("String formatting");
        }
        Ok(Self {
            session_id,
            source,
            sequence,
            timestamp: 0,
            cname: text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    struct Entropy {
        calls: AtomicUsize,
        data: Mutex<Vec<u8>>,
    }
    impl SipMediaEntropy for Entropy {
        fn fill(&self, bytes: &mut [u8]) -> Result<(), MediaError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut data = self.data.lock().unwrap();
            if data.len() < bytes.len() {
                return Err(MediaError::EntropyUnavailable);
            }
            for byte in bytes {
                *byte = data.remove(0);
            }
            Ok(())
        }
    }
    #[test]
    fn identity_retains_original_bounds_zero_timestamp_and_guid_bits() {
        let mut bytes = Vec::new();
        for value in [u32::MAX, 1_147_483_647, i32::MAX as u32, 65_535] {
            bytes.extend(value.to_ne_bytes());
        }
        bytes.extend([0xff; 16]);
        let source = Entropy {
            calls: AtomicUsize::new(0),
            data: Mutex::new(bytes),
        };
        let identity = SipMediaIdentity::generate(&source).unwrap();
        assert_eq!(identity.session_id, i32::MAX as u32);
        assert_eq!(identity.source, i32::MAX as u32);
        assert_eq!(identity.sequence, u16::MAX);
        assert_eq!(identity.timestamp, 0);
        assert_eq!(identity.cname, "ffffffff-ffff-4fff-bfff-ffffffffffff");
        assert_eq!(source.calls.load(Ordering::SeqCst), 5); // One rejected session sample.
        assert!(SipMediaIdentity::generate(&source).is_err());
    }
    #[test]
    fn inclusive_boundary_and_ten_rejections_match_original_integer_policy() {
        let min = 1_000_000_000;
        let width = i32::MAX as u64 - u64::from(min) + 1;
        let edge = ((1_u64 << 32) - (1_u64 << 32) % width) as u32;
        assert_eq!(random_range(min, i32::MAX as u32, || Ok(edge), ()), Ok(min));
        let mut attempts = 0;
        assert_eq!(
            random_range(
                min,
                i32::MAX as u32,
                || {
                    attempts += 1;
                    Ok(u32::MAX)
                },
                ()
            ),
            Err(())
        );
        assert_eq!(attempts, 10);
        assert_eq!(
            random_range(
                5,
                5,
                || -> Result<u32, ()> { panic!("No entropy for fixed range") },
                ()
            ),
            Ok(5)
        );
        assert_eq!(random_range(5, 4, || Ok(0), ()), Err(()));
        assert_eq!(random_range(0, u32::MAX, || Ok(u32::MAX), ()), Ok(u32::MAX));
        assert_eq!(
            random_range(0, 10, || Err("provider"), "exhausted"),
            Err("provider")
        );
    }
}
