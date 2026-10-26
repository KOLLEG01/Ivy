//! Fixed Date/Intl observations from the original constructor, captured on both OS.
pub fn registry(worker: bool) -> serde_json::Value {
    static PROFILES: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    PROFILES.get_or_init(|| {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::GzDecoder::new(&include_bytes!("hive-startup-profiles.json.gz")[..]),
            &mut bytes,
        )
        .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    })[if worker { "worker" } else { "kernel" }]
    .clone()
}
