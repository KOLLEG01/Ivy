// Synthetic executable for launch-owner tests. It never loads a native provider.
use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    process::{Command, Stdio},
};

fn quote(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c < ' ' => output.push_str(&format!("\\u{:04x}", c as u32)),
            c => output.push(c),
        }
    }
    output.push('"');
    output
}

fn main() {
    let executable = env::current_exe().unwrap();
    let configuration =
        fs::read_to_string(executable.parent().unwrap().join("launch-fixture.txt")).unwrap();
    let fields: Vec<_> = configuration.lines().collect();
    let arguments: Vec<_> = env::args().skip(1).collect();
    let logged_arguments = if arguments.iter().any(|s| s == "-Command") {
        vec!["<shell-probe>".to_owned()]
    } else {
        arguments.clone()
    };
    let log = format!(
        "{{\"args\":[{}],\"cwd\":{},\"home\":{},\"credential\":{},\"shellPath\":{}}}\n",
        logged_arguments
            .iter()
            .map(|s| quote(s))
            .collect::<Vec<_>>()
            .join(","),
        quote(&env::current_dir().unwrap().to_string_lossy()),
        quote(&env::var("CODEX_HOME").unwrap_or_default()),
        env::var("IVY_HIVE_TOKEN").as_deref() == Ok("fixture-credential"),
        env::var("PATH").unwrap_or_default().split(';').next()
            == executable.parent().unwrap().to_str()
    );
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{}.launches", fields[4]))
        .unwrap()
        .write_all(log.as_bytes())
        .unwrap();
    if arguments == ["--version"] {
        println!("codex-cli {}", fields[6]);
    } else if arguments.iter().any(|s| s == "-Command") {
        println!(
            "{{\"packageStatus\":15700,\"version\":\"7.6.0\",\"executable\":{}}}",
            quote(&executable.to_string_lossy())
        );
    } else if arguments == ["app-server", "daemon", "start"] {
        println!("{}", fields[5]);
    } else {
        let proxy = arguments.get(1).is_some_and(|s| s == "proxy");
        let endpoint = if proxy {
            fs::read_to_string(&arguments[3]).unwrap()
        } else {
            fields[3].to_owned()
        };
        let status = Command::new(fields[0])
            .arg(fields[1])
            .arg(if proxy { "proxy" } else { fields[2] })
            .arg(endpoint.trim())
            .arg(fields[4])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .unwrap();
        std::process::exit(status.code().unwrap_or(1));
    }
}
