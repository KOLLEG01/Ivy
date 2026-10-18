//! Test-only parser adapter; never included in component packaging.
use ivy_secretary::document::{Format, SOURCE_LIMIT, extract};
use std::io::{Read, Write};
fn main() {
    let format = match std::env::args().nth(1).as_deref() {
        Some("pdf") => Format::Pdf,
        Some("docx") => Format::Docx,
        Some("html") => Format::Html,
        _ => std::process::exit(2),
    };
    let mut input = Vec::new();
    std::io::stdin()
        .take(SOURCE_LIMIT as u64 + 1)
        .read_to_end(&mut input)
        .unwrap();
    let result = match extract(format, &input) {
        Ok(value) => serde_json::json!({ "value": value }),
        Err(code) => serde_json::json!({ "code": code }),
    };
    std::io::stdout()
        .write_all(serde_json::to_string(&result).unwrap().as_bytes())
        .unwrap();
}
