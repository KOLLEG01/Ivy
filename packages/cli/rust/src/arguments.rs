use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};

/// Admitted CLI syntax. Host configuration, paths and command execution remain
/// host-runtime responsibilities; admission performs no filesystem operations.
#[derive(Debug, Clone)]
pub struct Arguments {
    pub command: String,
    pub values: IvyJson,
    pub wait_ms: f64,
}

impl Arguments {
    pub fn required(&self, key: &str) -> IvyResult<&JsString> {
        self.values
            .get(key)
            .and_then(IvyJson::as_js_string)
            .filter(|s| !s.0.is_empty())
            .ok_or_else(|| invalid(&format!("Missing --{key}.")))
    }

    /// Resolve the original precedence lazily: the host's default path resolver
    /// must not run when either an explicit option or environment value exists.
    pub fn config_path(
        &self,
        environment: Option<&JsString>,
        default: impl FnOnce() -> IvyResult<JsString>,
    ) -> IvyResult<JsString> {
        let path = match self
            .values
            .get("config")
            .and_then(IvyJson::as_js_string)
            .or(environment)
        {
            Some(path) => path.clone(),
            None => default()?,
        };
        if path.0.is_empty() {
            return Err(invalid(
                "Use --config PATH or IVY_HOST_CONFIG to select this host installation.",
            ));
        }
        Ok(path)
    }
}

pub fn parse(args: &[JsString]) -> IvyResult<Arguments> {
    let mut values = IvyJson::object(Vec::<(String, IvyJson)>::new());
    let mut positionals = Vec::new();
    let mut terminated = false;
    let mut i = 0;
    while i < args.len() {
        let argument = &args[i];
        i += 1;
        let units = &argument.0;
        if terminated || units.len() < 2 || units[0] != 45 {
            positionals.push(argument);
            continue;
        }
        if units == &[45, 45] {
            terminated = true;
            continue;
        }
        if units[1] != 45 {
            return Err(options_error());
        }
        let split = units.iter().position(|u| *u == 61);
        let end = split.unwrap_or(units.len());
        let name = String::from_utf16(&units[2..end]).map_err(|_| options_error())?;
        let boolean = match name.as_str() {
            "json" | "apply" => true,
            "config" | "component" | "source" | "instance" | "candidate" | "operation-id"
            | "deployment" | "to" | "wait-ms" | "destination" | "backup" | "backup-hash"
            | "request" | "confirm" | "phase" | "reset-id" | "runtime-epoch" | "scope-roots"
            | "publisher-digest" => false,
            _ => return Err(options_error()),
        };
        let value = if boolean {
            if split.is_some() {
                return Err(options_error());
            }
            IvyJson::Bool(true)
        } else if let Some(split) = split {
            IvyJson::String(JsString(units[split + 1..].to_vec()))
        } else {
            let value = args.get(i).ok_or_else(options_error)?;
            // Node's strict string-option admission treats a dash-prefixed value
            // as ambiguous (except the single dash). The '=' spelling is explicit.
            if value.0.len() > 1 && value.0[0] == 45 {
                return Err(options_error());
            }
            i += 1;
            IvyJson::String(value.clone())
        };
        values.insert(name, value);
    }
    if positionals.len() != 1 {
        return Err(invalid("Select exactly one Ivy command."));
    }
    let command = positionals[0]
        .to_utf8()
        .map_err(|_| invalid("Unknown Ivy command."))?;
    let allowed: &[&str] = match command.as_str() {
        "prepare" => &["component", "source"],
        "deploy" => &["instance", "candidate", "source", "operation-id", "wait-ms"],
        "rollback" => &["instance", "to", "operation-id", "wait-ms"],
        "restart" | "enable" | "disable" => &["instance", "operation-id", "wait-ms"],
        "status" => &["deployment"],
        "preparations" | "compact-preparations" | "collect-storage" | "inspect-bootstrap" => &[],
        "backup" => &["destination"],
        "restore" => &["backup", "backup-hash"],
        "runtime-reset" => &["apply", "confirm"],
        "runtime-reset-host" => &[
            "phase",
            "reset-id",
            "runtime-epoch",
            "scope-roots",
            "publisher-digest",
        ],
        "update-bootstrap" => &["request"],
        "bootstrap-status" => &["operation-id"],
        "install-bootstrap" => &["candidate"],
        _ => return Err(invalid("Unknown Ivy command.")),
    };
    for (key, _) in values.as_object().unwrap() {
        let key = key.to_utf8()?;
        if key != "config" && key != "json" && !allowed.contains(&key.as_str()) {
            return Err(invalid(
                "This option does not belong to the selected command.",
            ));
        }
    }
    let wait_ms = parse_wait_ms(values.get("wait-ms").and_then(IvyJson::as_js_string))?;
    Ok(Arguments {
        command,
        values,
        wait_ms,
    })
}

fn parse_wait_ms(value: Option<&JsString>) -> IvyResult<f64> {
    let invalid_wait = || invalid("--wait-ms must be a finite integer from 0 to 600000.");
    let Some(value) = value else { return Ok(0.0) };
    let text = value.to_utf8().map_err(|_| invalid_wait())?;
    // CLI option syntax permits a surrounding BOM with whitespace. NEL is
    // ordinary input and must reach numeric validation.
    let text =
        text.trim_matches(|c: char| c == '\u{feff}' || (c.is_whitespace() && c != '\u{0085}'));
    // The CLI accepts decimal/exponent and unsigned radix spellings. Its small
    // bound needs only native integer/float parsing, not generic JSON coercion.
    let radix = match text.as_bytes().get(..2) {
        Some(b"0x" | b"0X") => Some(16),
        Some(b"0o" | b"0O") => Some(8),
        Some(b"0b" | b"0B") => Some(2),
        _ => None,
    };
    let number = if text.is_empty() {
        0.0
    } else if let Some(radix) = radix {
        let digits = &text[2..];
        if digits.starts_with('+') {
            return Err(invalid_wait());
        }
        u32::from_str_radix(digits, radix).map_err(|_| invalid_wait())? as f64
    } else {
        text.parse::<f64>().map_err(|_| invalid_wait())?
    };
    if number.is_finite() && number.fract() == 0.0 && (0.0..=600_000.0).contains(&number) {
        Ok(number)
    } else {
        Err(invalid_wait())
    }
}

fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}
fn options_error() -> IvyError {
    invalid("Invalid command options. See the Ivy local CLI contract.")
}
