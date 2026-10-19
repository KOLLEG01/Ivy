//! The original .NET local/UTC clock pair and NTP arithmetic. SIPSorcery
//! 10.0.15 attribution/notice: native/phone-runtime/licenses/SIPSorcery-10.0.15.md.
use super::MediaError;

/// .NET DateTime ticks (100 ns since 0001-01-01). Local ticks retain the
/// original wall-clock subtraction and local comparison at the NTP era edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtcpWallTime {
    local: i64,
    utc: i64,
}
impl RtcpWallTime {
    pub const UNIX_TICKS: i64 = 621_355_968_000_000_000;
    pub const MAX_TICKS: i64 = 3_155_378_975_999_999_999;
    const EPOCH_1900: i64 = Self::UNIX_TICKS - 22_089_888_000_000_000;
    const EPOCH_2036: i64 = Self::UNIX_TICKS + 20_859_784_960_000_000;
    pub fn new(local_ticks: i64, utc_ticks: i64) -> Result<Self, MediaError> {
        if !(0..=Self::MAX_TICKS).contains(&local_ticks)
            || !(0..=Self::MAX_TICKS).contains(&utc_ticks)
        {
            return Err(MediaError::ClockUnavailable);
        }
        Ok(Self {
            local: local_ticks,
            utc: utc_ticks,
        })
    }
    pub fn local_ticks(self) -> i64 {
        self.local
    }
    pub fn utc_ticks(self) -> i64 {
        self.utc
    }
    pub fn ntp(self) -> u64 {
        let epoch = if self.local >= Self::EPOCH_2036 {
            Self::EPOCH_2036
        } else {
            Self::EPOCH_1900
        };
        let elapsed = if self.local > epoch {
            self.utc - epoch
        } else {
            epoch - self.utc
        };
        let seconds = elapsed as f64 / 10_000_000.0;
        let whole = seconds as u64;
        whole.wrapping_shl(32) | ((seconds - whole as f64) * 4_294_967_296.0) as u64
    }
    pub fn ntp32(self) -> u32 {
        (self.ntp() >> 16) as u32
    }
    pub fn arrival(self, created: Self, sample_rate: i32) -> u32 {
        if sample_rate == 0 {
            return self.ntp32();
        }
        // The native application leaves this rate0. Retain the public original
        // override's floating-point units without changing its default policy.
        let value = (self.local - created.local) as f64 / 10_000_000.0 * f64::from(sample_rate);
        value as u32
    }
}

/// Providers must finish bounded clock/entropy work and must not re-enter the
/// media owner. A supplied provider also permits deterministic clock failures.
pub trait RtcpRuntime: Send + Sync + 'static {
    fn now(&self) -> Result<RtcpWallTime, MediaError>;
    fn interval_ms(&self) -> Result<u16, MediaError>;
}

/// Retain the original UInt32 rejection boundary, including its inclusive
/// comparison and ten-attempt limit, before mapping to [2500,7500].
pub fn rtcp_interval(
    mut random: impl FnMut() -> Result<u32, MediaError>,
) -> Result<u16, MediaError> {
    const RANGE: u64 = 5001;
    const MAX: u64 = 1_u64 << 32;
    for _ in 0..10 {
        let value = u64::from(random()?);
        if value <= MAX - MAX % RANGE {
            return Ok(2500 + (value % RANGE) as u16);
        }
    }
    Err(MediaError::ClockUnavailable)
}

#[cfg(windows)]
#[derive(Default)]
pub struct WindowsRtcpRuntime;
#[cfg(windows)]
impl RtcpRuntime for WindowsRtcpRuntime {
    fn now(&self) -> Result<RtcpWallTime, MediaError> {
        use windows_sys::Win32::{
            Foundation::FILETIME, Storage::FileSystem::FileTimeToLocalFileTime,
            System::SystemInformation::GetSystemTimePreciseAsFileTime,
        };
        let mut utc = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut local = utc;
        // SAFETY: both output pointers borrow initialized stack FILETIMEs;
        // the APIs retain no pointer or resource. Conversion uses this UTC
        // instant and the current OS local-zone rule, preserving sub-ms ticks.
        unsafe {
            GetSystemTimePreciseAsFileTime(&mut utc);
            if FileTimeToLocalFileTime(&utc, &mut local) == 0 {
                return Err(MediaError::ClockUnavailable);
            }
        }
        let ticks = |v: FILETIME| {
            let raw = (u64::from(v.dwHighDateTime) << 32) | u64::from(v.dwLowDateTime);
            i64::try_from(raw)
                .ok()
                .and_then(|t| t.checked_add(504_911_232_000_000_000))
                .ok_or(MediaError::ClockUnavailable)
        };
        RtcpWallTime::new(ticks(local)?, ticks(utc)?)
    }
    fn interval_ms(&self) -> Result<u16, MediaError> {
        use windows_sys::Win32::Security::Cryptography::{
            BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
        };
        rtcp_interval(|| {
            let mut bytes = [0_u8; 4];
            // SAFETY: the system provider writes exactly the borrowed array;
            // a null algorithm handle is required by this provider flag.
            let status = unsafe {
                BCryptGenRandom(
                    std::ptr::null_mut(),
                    bytes.as_mut_ptr(),
                    4,
                    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                )
            };
            if status < 0 {
                return Err(MediaError::ClockUnavailable);
            }
            Ok(u32::from_ne_bytes(bytes))
        })
    }
}
