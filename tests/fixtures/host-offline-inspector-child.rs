//! Controlled native subprocess for Host inspector deadline/pipe ownership tests.
use std::{
    fs,
    io::{self, Read},
    process::{self, Command},
    thread,
    time::Duration,
};
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--descendant") {
        fs::write("inspector-child.pid", process::id().to_string()).unwrap();
    } else {
        if args.iter().any(|arg| arg.contains("fixture.early-exit")) {
            process::exit(1);
        }
        let mut input = Vec::new();
        if !args.iter().any(|arg| arg.contains("fixture.blocked-input")) {
            io::stdin().read_to_end(&mut input).unwrap();
        }
        fs::write("inspector-input-length", input.len().to_string()).unwrap();
        fs::write("inspector-parent.pid", process::id().to_string()).unwrap();
        let _child = Command::new(std::env::current_exe().unwrap())
            .arg("--descendant")
            .spawn()
            .unwrap();
        loop {
            thread::sleep(Duration::from_millis(100));
        }
    }
    loop {
        thread::sleep(Duration::from_millis(100));
    }
}
