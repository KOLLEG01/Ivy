//! Fixed compatibility cases. Only the native implementation executes during tests.
pub fn read(bytes: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(bytes), &mut decoded).unwrap();
    decoded
}

// Catalog cases also check that adding a retained catalog cannot leave it uncovered.
#[allow(dead_code)]
pub fn codex_catalog_count(root: &std::path::Path) -> usize {
    std::fs::read_dir(root.join("specs/native"))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry.file_type().unwrap().is_dir()
                && entry.file_name().to_string_lossy().starts_with("codex-")
        })
        .flat_map(|entry| std::fs::read_dir(entry.path()).unwrap().map(Result::unwrap))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name == "catalog.json" || name.starts_with("catalog.") && name.ends_with(".json")
        })
        .count()
}
