#[cfg(windows)]
fn main() {
    ivy_host_runtime::platform::windows_job::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("{{\"code\":\"unsupported_runtime\"}}");
    std::process::exit(1);
}
