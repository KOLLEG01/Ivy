//! A Linux lifetime lease and boot/start identity, independent of Windows FILETIME.
use super::{NativeOwner, RECORD_BYTES, guid_d, unavailable};
use ivy_contracts::{
    IvyJson,
    json::{ParseOptions, parse_with_options, stringify},
};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
    sync::Mutex,
};

static RETAINED: Mutex<Option<File>> = Mutex::new(None);

fn boot_id() -> io::Result<String> {
    let value = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let value = value.trim();
    if !guid_d(value) {
        return Err(unavailable());
    }
    Ok(value.into())
}

fn process_start(pid: u32) -> io::Result<Option<u64>> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // comm may contain spaces and parentheses. The last ')' ends field 2.
    let fields = stat.rsplit_once(')').ok_or_else(unavailable)?.1;
    let mut fields = fields.split_whitespace();
    if matches!(fields.next(), Some("Z" | "X")) {
        return Ok(None);
    }
    fields
        .nth(18)
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .map(Some)
        .ok_or_else(unavailable)
}

pub(crate) fn lock(file: &File) -> io::Result<bool> {
    // SAFETY: this borrowed descriptor remains owned and open throughout flock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

fn record(value: &IvyJson) -> io::Result<(NativeOwner, String)> {
    let fields = ["schemaVersion", "epoch", "pid", "startTicks", "bootId"];
    if !value
        .as_object()
        .is_some_and(|values| values.len() == fields.len())
        || fields.iter().any(|key| value.get(key).is_none())
        || value.get("schemaVersion") != Some(&1.0.into())
    {
        return Err(unavailable());
    }
    let string = |key| {
        value
            .get(key)
            .and_then(IvyJson::as_js_string)
            .ok_or_else(unavailable)?
            .to_utf8()
            .map_err(|_| unavailable())
    };
    let epoch = string("epoch")?;
    let boot = string("bootId")?;
    if !guid_d(&epoch) || !guid_d(&boot) {
        return Err(unavailable());
    }
    let pid = value
        .get("pid")
        .and_then(IvyJson::as_f64)
        .filter(|pid| *pid > 0.0 && *pid <= f64::from(i32::MAX) && pid.fract() == 0.0)
        .ok_or_else(unavailable)? as i32;
    let ticks = string("startTicks")?;
    let start = ticks
        .parse::<i64>()
        .ok()
        .filter(|start| *start > 0 && start.to_string() == ticks)
        .ok_or_else(unavailable)?;
    Ok((
        NativeOwner {
            epoch,
            pid,
            creation_filetime: start,
        },
        boot,
    ))
}

pub(crate) fn validate_record(value: &IvyJson) -> io::Result<()> {
    record(value).map(|_| ())
}

pub(super) fn acquire(root: &Path, epoch: &str) -> io::Result<NativeOwner> {
    let mut retained = RETAINED.lock().unwrap_or_else(|error| error.into_inner());
    if retained.is_some() {
        return Err(io::Error::other("Native process already owns a directory."));
    }
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join("phone-native.lock"))?;
    if !lock(&lease)? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Native directory is owned.",
        ));
    }
    let boot = boot_id()?;
    let path = root.join("phone-native-owner.json");
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(previous) => {
            if !previous.metadata()?.is_file() {
                return Err(unavailable());
            }
            let mut bytes = Vec::new();
            previous
                .take(RECORD_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if !(2..=RECORD_BYTES).contains(&bytes.len()) {
                return Err(unavailable());
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| unavailable())?;
            let value = parse_with_options(
                text,
                RECORD_BYTES,
                ParseOptions {
                    reject_duplicate_keys: true,
                    maximum_container_depth: Some(4),
                },
            )
            .map_err(|_| unavailable())?;
            let (owner, previous_boot) = record(&value)?;
            if previous_boot == boot
                && process_start(owner.pid())? == Some(owner.creation_filetime())
            {
                return Err(io::Error::other("Original native process has not exited."));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let pid = std::process::id();
    let start = process_start(pid)?.ok_or_else(unavailable)?;
    let current = NativeOwner {
        epoch: epoch.into(),
        pid: i32::try_from(pid).map_err(|_| unavailable())?,
        creation_filetime: i64::try_from(start).map_err(|_| unavailable())?,
    };
    let value = IvyJson::object([
        ("schemaVersion", 1.0.into()),
        ("epoch", epoch.into()),
        ("pid", f64::from(pid).into()),
        ("startTicks", start.to_string().into()),
        ("bootId", boot.into()),
    ]);
    let bytes = stringify(&value, RECORD_BYTES).map_err(|_| unavailable())?;
    let temporary = root.join(format!("phone-native-owner.json.{epoch}.tmp"));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    output.write_all(bytes.as_bytes())?;
    output.sync_all()?;
    drop(output);
    std::fs::rename(temporary, path)?;
    File::open(root)?.sync_all()?;
    *retained = Some(lease);
    Ok(current)
}
