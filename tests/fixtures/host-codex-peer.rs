//! Disposable native peer for CLI/connection tests. No account or network access.
use std::{
    env,
    fs::OpenOptions,
    io::{BufRead, Write},
};

fn main() {
    if env::args().nth(1).as_deref() == Some("--version") {
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
