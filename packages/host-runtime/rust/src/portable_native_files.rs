/// Windows payloads required in the universal package on Windows hosts.
pub fn portable_native_files(component_id: &str) -> &'static [&'static str] {
    match component_id {
        "host-executor" => &["dist/native/ivy-job.exe", "dist/native/ivy-host-job.exe"],
        "service-manager" | "agent-manager" => &["dist/native/ivy-job.exe"],
        "phone-bridge" => &["dist/native/ivy-job.exe", "dist/native/ivy-phone-input.dll"],
        _ => &[],
    }
}
