//! Disposable native peer for CLI, shell and connection tests. No account or network access.
use std::{
    env,
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{BufRead, Write},
};

fn main() {
    let argument = env::args_os().nth(1);
    if argument.as_deref() == Some(OsStr::new("-NoLogo")) {
        let arguments: Vec<_> = env::args_os().skip(1).collect();
        assert_eq!(arguments.len(), 5);
        for (actual, expected) in
            arguments
                .iter()
                .zip(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        {
            assert_eq!(actual, OsStr::new(expected));
        }
        assert!(
            arguments[4]
                .to_str()
                .unwrap()
                .contains("GetCurrentPackageFullName")
        );
        let executable = env::current_exe().unwrap();
        let root = executable.parent().unwrap();
        fs::write(root.join("shell-probe-called"), b"called").unwrap();
        std::io::stdout()
            .write_all(&fs::read(root.join("shell-probe.json")).unwrap())
            .unwrap();
        return;
    }
    if argument.as_deref() == Some(OsStr::new("--version")) {
        print!("{}", env::var("IVY_CODEX_STDOUT").unwrap());
        std::io::stdout().flush().unwrap();
        std::process::exit(env::var("IVY_CODEX_EXIT").unwrap().parse().unwrap());
    }
    let mut record = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(env::var_os("IVY_CODEX_RECORD").unwrap())
        .unwrap();
    let mut input = std::io::stdin().lock();
    let mut line = String::new();
    assert_ne!(input.read_line(&mut line).unwrap(), 0);
    record.write_all(line.as_bytes()).unwrap();
    record.flush().unwrap();
    if env::var("IVY_CODEX_MODE").unwrap() != "hold" {
        // initialize generates a UUID string ID; this fixture does not parse
        // arbitrary JSON or implement another protocol runtime.
        let id = line
            .split_once("\"id\":\"")
            .unwrap()
            .1
            .split_once('"')
            .unwrap()
            .0;
        assert!(
            id.bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        );
        println!(
            "{{\"id\":\"{id}\",\"result\":{}}}",
            env::var("IVY_CODEX_REPLY").unwrap()
        );
        std::io::stdout().flush().unwrap();
    }
    loop {
        line.clear();
        if input.read_line(&mut line).unwrap() == 0 {
            return;
        }
        record.write_all(line.as_bytes()).unwrap();
        record.flush().unwrap();
    }
}
