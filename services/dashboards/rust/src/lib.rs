pub mod browser_path;
pub mod document;
pub mod png;
pub mod raster;

pub fn registry() -> ivy_contracts::IvyJson {
    ivy_contracts::json::parse(
        include_str!("../assets/registry.json"),
        ivy_contracts::limits::MANAGEMENT_FRAME_BYTES,
    )
    .expect("embedded dashboard registry is valid JSON")
}
