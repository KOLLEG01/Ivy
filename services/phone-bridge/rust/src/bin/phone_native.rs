fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    std::process::exit(ivy_phone_bridge::native_entry::run_owned_native_entry(
        &args,
        ivy_phone_bridge::native_runtime::system_runtime,
    ));
}
