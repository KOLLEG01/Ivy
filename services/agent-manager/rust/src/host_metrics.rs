//! Host load beside each host: whole-host memory, averaged CPU and the volumes of the
//! configured project roots. Readers share one bounded snapshot; a read older than
//! `SNAPSHOT_AGE` takes the next native sample, so idle hosts are never polled.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::service::ToolHandler;
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Readers polling once a minute each take one sample; faster readers share it.
pub const SNAPSHOT_AGE: Duration = Duration::from_secs(55);
/// Time constant of the exponentially weighted CPU average.
pub const CPU_AVERAGE: Duration = Duration::from_secs(300);
/// A longer interval no longer describes the last minutes; the average restarts.
pub const CPU_GAP: Duration = Duration::from_secs(900);

/// Cumulative native counters over all logical CPUs, in any common unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

/// Time-weighted EWMA on counter differences: alpha = 1 - exp(-elapsed / CPU_AVERAGE).
/// The first difference initializes the average; no earlier history is assumed.
#[derive(Default)]
pub struct CpuAverage {
    previous: Option<(Instant, CpuTimes)>,
    average: Option<f64>,
    covered: Duration,
}

impl CpuAverage {
    pub fn observe(&mut self, at: Instant, times: CpuTimes) {
        if let Some((before, last)) = self.previous {
            let elapsed = at.saturating_duration_since(before);
            match (
                times.total.checked_sub(last.total),
                times.busy.checked_sub(last.busy),
            ) {
                (Some(0), Some(_)) => return,
                (Some(total), Some(busy)) if elapsed <= CPU_GAP => {
                    let sample = busy.min(total) as f64 / total as f64;
                    let alpha = 1.0 - (-elapsed.as_secs_f64() / CPU_AVERAGE.as_secs_f64()).exp();
                    self.average = Some(
                        self.average
                            .map_or(sample, |average| average + alpha * (sample - average)),
                    );
                    self.covered = (self.covered + elapsed).min(CPU_GAP);
                }
                // A restarted counter or a long gap cannot describe the last minutes.
                _ => {
                    self.average = None;
                    self.covered = Duration::ZERO;
                }
            }
        }
        self.previous = Some((at, times));
    }
    /// Percent of all logical CPUs, or None while the first interval is warming up.
    pub fn percent(&self) -> Option<f64> {
        self.average
            .map(|value| (value.clamp(0.0, 1.0) * 1000.0).round() / 10.0)
    }
    pub fn covered(&self) -> Duration {
        self.covered
    }
    fn json(&self) -> IvyJson {
        IvyJson::object([
            (
                "usedPercent",
                self.percent().map_or(IvyJson::Null, IvyJson::Number),
            ),
            (
                "averageSeconds",
                IvyJson::Number(CPU_AVERAGE.as_secs() as f64),
            ),
            (
                "coveredSeconds",
                IvyJson::Number(if self.average.is_some() {
                    self.covered.as_secs() as f64
                } else {
                    0.0
                }),
            ),
        ])
    }
}

/// Linux `/proc/stat`: idle and iowait are idle; guest time is already in user/nice.
pub fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let values = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }
    let total = values
        .iter()
        .try_fold(0u64, |sum, value| sum.checked_add(*value))?;
    let idle = values[3].checked_add(values.get(4).copied().unwrap_or(0))?;
    Some(CpuTimes {
        busy: total.checked_sub(idle)?,
        total,
    })
}

/// Linux `/proc/meminfo`: used memory is MemTotal minus MemAvailable, in bytes.
pub fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|value| value.trim().strip_suffix("kB"))
            .and_then(|value| value.trim().parse::<u64>().ok())
            .and_then(|value| value.checked_mul(1024))
    };
    let total = field("MemTotal")?;
    Some((total, total.saturating_sub(field("MemAvailable")?)))
}

pub struct Volume {
    /// Identity used to report each filesystem once.
    pub key: String,
    /// Mount point when the platform reports one.
    pub label: Option<String>,
    pub total: u64,
    pub free: u64,
}

/// A project root may not exist yet; its nearest existing parent shares its volume.
fn existing(path: &Path) -> Option<&Path> {
    path.ancestors().find(|candidate| candidate.exists())
}

#[cfg(windows)]
mod platform {
    use super::{CpuTimes, Volume};
    use std::{os::windows::ffi::OsStrExt, path::Path};
    use windows_sys::Win32::{
        Foundation::FILETIME,
        Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW},
        System::{
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::GetSystemTimes,
        },
    };
    fn ticks(time: FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    }
    /// Kernel time includes idle time; both cover every logical CPU.
    pub fn cpu_times() -> Option<CpuTimes> {
        let empty = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut idle, mut kernel, mut user) = (empty, empty, empty);
        // SAFETY: valid out pointers for the duration of the call.
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return None;
        }
        let total = ticks(kernel).checked_add(ticks(user))?;
        Some(CpuTimes {
            busy: total.checked_sub(ticks(idle))?,
            total,
        })
    }
    pub fn memory() -> Option<(u64, u64)> {
        // SAFETY: plain data structure; dwLength is set before the call.
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
            return None;
        }
        Some((
            status.ullTotalPhys,
            status.ullTotalPhys.saturating_sub(status.ullAvailPhys),
        ))
    }
    /// The volume API accepts only backslash separators.
    pub fn volume(path: &Path) -> Option<Volume> {
        let wide = path
            .as_os_str()
            .encode_wide()
            .map(|unit| {
                if unit == u16::from(b'/') {
                    u16::from(b'\\')
                } else {
                    unit
                }
            })
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut root = vec![0u16; wide.len().max(261) + 1];
        // SAFETY: NUL-terminated input; the output length is the buffer length.
        if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
            return None;
        }
        let end = root.iter().position(|unit| *unit == 0)?;
        let (mut total, mut free) = (0u64, 0u64);
        // SAFETY: `root` is NUL-terminated at `end`.
        if unsafe {
            GetDiskFreeSpaceExW(root.as_ptr(), std::ptr::null_mut(), &mut total, &mut free)
        } == 0
        {
            return None;
        }
        let label = String::from_utf16_lossy(&root[..end]);
        Some(Volume {
            key: label.to_lowercase(),
            label: Some(label),
            total,
            free,
        })
    }
}

#[cfg(unix)]
mod platform {
    use super::{CpuTimes, Volume};
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::MetadataExt},
        path::Path,
    };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn cpu_times() -> Option<CpuTimes> {
        super::parse_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn memory() -> Option<(u64, u64)> {
        super::parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
    }
    /// Other Unix hosts report these values as unavailable instead of estimating them.
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    pub fn cpu_times() -> Option<CpuTimes> {
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    pub fn memory() -> Option<(u64, u64)> {
        None
    }
    pub fn volume(path: &Path) -> Option<Volume> {
        let device = std::fs::metadata(path).ok()?.dev();
        let native = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: plain data structure filled by statvfs for a NUL-terminated path.
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(native.as_ptr(), &mut stats) } != 0 {
            return None;
        }
        let unit = if stats.f_frsize > 0 {
            stats.f_frsize as u64
        } else {
            stats.f_bsize as u64
        };
        Some(Volume {
            key: device.to_string(),
            label: None,
            total: (stats.f_blocks as u64).checked_mul(unit)?,
            free: (stats.f_bfree as u64).checked_mul(unit)?,
        })
    }
}

pub type ProjectRoots = dyn Fn() -> Vec<JsString> + Send + Sync;

#[derive(Default)]
struct Snapshot {
    cpu: CpuAverage,
    cached: Option<(Instant, IvyJson)>,
}

/// One per AgentManager process; only the node a reader selects takes samples.
#[derive(Default)]
pub struct HostMetrics {
    roots: Mutex<Option<Arc<ProjectRoots>>>,
    snapshot: tokio::sync::Mutex<Snapshot>,
}

fn bytes(total: u64, used: u64) -> [(&'static str, IvyJson); 2] {
    [
        ("totalBytes", IvyJson::Number(total as f64)),
        ("usedBytes", IvyJson::Number(used.min(total) as f64)),
    ]
}

/// Each filesystem appears once, labelled by the first configured root on it.
pub fn project_volumes(
    roots: &[JsString],
    volume: impl Fn(&Path) -> Option<Volume>,
) -> Vec<IvyJson> {
    let mut seen = Vec::<String>::new();
    let mut result = Vec::new();
    for root in roots {
        let Ok(text) = root.to_utf8() else { continue };
        let Some(found) = existing(Path::new(&text)).and_then(&volume) else {
            continue;
        };
        if seen.contains(&found.key) {
            continue;
        }
        seen.push(found.key);
        let [total, used] = bytes(found.total, found.total.saturating_sub(found.free));
        result.push(IvyJson::object([
            ("path", IvyJson::String(root.clone())),
            ("volume", found.label.map_or(IvyJson::Null, IvyJson::from)),
            total,
            used,
        ]));
    }
    result
}

impl HostMetrics {
    pub fn set_project_roots(&self, roots: Arc<ProjectRoots>) {
        *self.roots.lock().unwrap_or_else(|e| e.into_inner()) = Some(roots);
    }
    /// Readers arriving during a sample wait for it instead of starting another.
    pub async fn read(&self) -> IvyResult<IvyJson> {
        let mut snapshot = self.snapshot.lock().await;
        if let Some((at, value)) = &snapshot.cached
            && at.elapsed() < SNAPSHOT_AGE
        {
            return Ok(value.clone());
        }
        let source = self.roots.lock().map_err(|_| IvyError::internal())?.clone();
        let roots = source.map(|roots| roots()).unwrap_or_default();
        let (cpu, memory, volumes) = tokio::task::spawn_blocking(move || {
            (
                platform::cpu_times(),
                platform::memory(),
                project_volumes(&roots, platform::volume),
            )
        })
        .await
        .map_err(|_| IvyError::internal())?;
        let now = Instant::now();
        let cpu = cpu.map(|times| {
            snapshot.cpu.observe(now, times);
            snapshot.cpu.json()
        });
        let value = IvyJson::object([
            (
                "observedAt",
                IvyJson::String(
                    chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                        .into(),
                ),
            ),
            (
                "sampleSeconds",
                IvyJson::Number(SNAPSHOT_AGE.as_secs() as f64),
            ),
            (
                "memory",
                memory.map_or(IvyJson::Null, |(total, used)| {
                    IvyJson::object(bytes(total, used))
                }),
            ),
            ("cpu", cpu.unwrap_or(IvyJson::Null)),
            ("volumes", IvyJson::Array(volumes)),
        ]);
        snapshot.cached = Some((now, value.clone()));
        Ok(value)
    }
    pub fn handler(self: &Arc<Self>) -> ToolHandler {
        let metrics = self.clone();
        Arc::new(move |args, context| {
            let metrics = metrics.clone();
            Box::pin(async move {
                ivy_contracts::agent_schema::validate_agent("BackendsInput", &args)?;
                context.throw_if_aborted()?;
                metrics.read().await
            })
        })
    }
}
