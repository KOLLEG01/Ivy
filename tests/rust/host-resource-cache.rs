use super::*;
use std::fs;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-resource-cache-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("bootstrap")).unwrap();
        Self(root)
    }
    fn config(&self) -> IvyJson {
        IvyJson::object([
            ("hostId", "resource-host".into()),
            ("runtimeRoot", IvyJson::String(node_path::from_native(&self.0))),
        ])
    }
    fn path(&self) -> PathBuf {
        self.0.join("bootstrap").join(format!("{}.json", linux_resources::installation_identity(&self.config()).unwrap()))
    }
    fn plan(&self) -> IvyJson {
        let config = self.config();
        let hash: IvyJson = format!("sha256:{}", "a".repeat(64)).into();
        IvyJson::object([
            ("schemaVersion", 1.0.into()),
            ("hostId", config.required("hostId").unwrap().clone()),
            ("os", "linux".into()),
            ("installationId", linux_resources::installation_identity(&config).unwrap().into()),
            ("candidateId", hash.clone()),
            ("artifactRoot", config.required("runtimeRoot").unwrap().clone()),
            ("configPath", IvyJson::String(node_path::from_native(&self.0.join("host.json")))),
            ("configHash", hash),
            ("runtimeRoot", config.required("runtimeRoot").unwrap().clone()),
            ("processes", IvyJson::Array(vec![])),
        ])
    }
    fn scope(&self) -> IvyJson {
        linux_resources::linux_resource_scope(
            &linux_resources::installation_identity(&self.config()).unwrap(),
            &IvyJson::object([
                ("memoryHighBytes", 67_108_864.0.into()),
                ("memoryMaxBytes", 134_217_728.0.into()),
                ("tasksMax", 16.0.into()),
            ]),
        ).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

#[tokio::test]
async fn errors_do_not_publish_a_missing_or_unvalidated_budget() {
    let fixture = Fixture::new();
    for case in ["read", "json", "schema", "installation", "scope"] {
        let mut cache = Resources::default();
        let mut plan = fixture.plan();
        match case {
            "read" => fs::create_dir(fixture.path()).unwrap(),
            "json" => fs::write(fixture.path(), b"{").unwrap(),
            "schema" => config::atomic_json(fixture.path(), IvyJson::Null).await.unwrap(),
            "installation" => {
                plan.insert("installationId", "ivy-next-000000000000".into());
                config::atomic_json(fixture.path(), plan).await.unwrap();
            }
            "scope" => {
                let mut scope = fixture.scope();
                scope.insert("slice", "ivynext000000000000000000000000.slice".into());
                plan.insert("linuxResources", scope);
                config::atomic_json(fixture.path(), plan).await.unwrap();
            }
            _ => unreachable!(),
        }
        for _ in 0..2 {
            assert!(cache.scope("linux", &fixture.config()).await.is_err(), "{case}");
        }
        if case == "read" { fs::remove_dir(fixture.path()).unwrap(); }
        let scope = fixture.scope();
        let mut plan = fixture.plan();
        plan.insert("linuxResources", scope.clone());
        config::atomic_json(fixture.path(), plan).await.unwrap();
        let loaded = cache.scope("linux", &fixture.config()).await.unwrap().unwrap();
        assert_eq!(ivy_contracts::hash::hash_json(&loaded).unwrap(), ivy_contracts::hash::hash_json(&scope).unwrap(), "{case}");
        fs::remove_file(fixture.path()).unwrap();
        assert_eq!(cache.scope("linux", &fixture.config()).await.unwrap(), Some(loaded), "{case}");
    }
}

#[tokio::test]
async fn successful_absence_is_cached_and_other_platforms_need_no_plan() {
    let fixture = Fixture::new();
    for present in [false, true] {
        if present { config::atomic_json(fixture.path(), fixture.plan()).await.unwrap(); }
        let mut cache = Resources::default();
        assert_eq!(cache.scope("linux", &fixture.config()).await.unwrap(), None);
        fs::write(fixture.path(), b"{").unwrap();
        assert_eq!(cache.scope("linux", &fixture.config()).await.unwrap(), None);
        fs::remove_file(fixture.path()).unwrap();
    }
    assert_eq!(Resources::default().scope("win32", &IvyJson::Null).await.unwrap(), None);
}
