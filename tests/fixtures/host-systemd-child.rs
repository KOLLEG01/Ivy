use std::io::Write;
fn main() {
    let name = std::env::current_exe()
        .unwrap()
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    if name == "exit" {
        eprint!("bounded diagnostic");
        std::process::exit(7);
    }
    if name == "unicode" {
        eprint!("prefix😀{}", "x".repeat(1023));
        std::process::exit(9);
    }
    if name == "overflow" {
        let _ = std::io::stdout().write_all(&vec![b'x'; 70_000]);
        return;
    }
    println!("LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=42");
}
