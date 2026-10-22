//! Focused native HTTP ingress and local MCP bridge checks.
use ivy_contracts::{IvyJson, json::parse};
use ivy_data_collector::{store::{CollectorStore, Limits}, store_owner::StoreOwner};
use std::path::PathBuf;

#[path = "data-collector-http.inc.rs"]
mod http;

fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-collector-http-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn open(&self) -> StoreOwner {
        StoreOwner::new(CollectorStore::open(&self.0.join("collector.sqlite"), Limits::default(), "2026-01-01T00:00:00.000Z").unwrap())
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
