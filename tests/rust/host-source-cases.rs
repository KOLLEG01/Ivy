use super::{ServiceFixture, compare_config_oracle, assert_json_eq, assert_optional_json_eq};
use ivy_contracts::{IvyJson, IvyResult, JsString};
use ivy_host_runtime::{candidate_identity::candidate_build_id, config, preparations, source};
use std::{fs, path::Path};

fn hash(c: &str) -> String { format!("sha256:{}", c.repeat(64)) }
fn path(p: &Path) -> IvyJson { p.to_string_lossy().to_string().into() }
fn compare(kind: &str, operation: &str, args: Vec<IvyJson>, result: IvyResult<IvyJson>) {
    compare_config_oracle(IvyJson::object([("kind",kind.into()),("operation",operation.into()),("args",IvyJson::Array(args))]),result);
}
fn snapshot(fixture: &ServiceFixture) -> IvyJson {
    IvyJson::object([("snapshotId","fixed-snapshot".into()),("sourceRoot",path(&fixture.0.join("staging/source"))),("originalRoot",path(&fixture.0.join("checkout"))),
        ("manifestPath",path(&fixture.0.join("staging/snapshot.json"))),("capturedAt","2026-01-01T00:00:00.000Z".into())])
}
fn host(fixture: &ServiceFixture) -> IvyJson { IvyJson::object([("stagingRoot",path(&fixture.0.join("staging")))]) }
fn build_plan() -> IvyJson {
    // Use the real versioned source contract, then change only its identity.
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let bytes=fs::read_to_string(root.join("services/host-executor/deploy.json")).unwrap();
    let mut plan=ivy_contracts::json::parse(&bytes,usize::MAX).unwrap(); plan.insert("componentId","fixture".into()); plan
}

#[tokio::test]
async fn source_plan_requires_exactly_one_valid_matching_component() {
    let fixture=ServiceFixture::new(); let plan=build_plan();
    for id in ["fixture","../fixture","Fixture","fixture/child",""] {
        compare("source","sourceBuildPlan",vec![path(&fixture.0),id.into()],source::source_build_plan(fixture.0.clone(),id).await);
    }
    config::atomic_json(fixture.0.join("services/fixture/deploy.json"),plan.clone()).await.unwrap();
    assert_json_eq(&source::source_build_plan(fixture.0.clone(),"fixture").await.unwrap(),&plan);
    compare("source","sourceBuildPlan",vec![path(&fixture.0),"fixture".into()],source::source_build_plan(fixture.0.clone(),"fixture").await);
    config::atomic_json(fixture.0.join("ui/fixture/deploy.json"),plan.clone()).await.unwrap();
    compare("source","sourceBuildPlan",vec![path(&fixture.0),"fixture".into()],source::source_build_plan(fixture.0.clone(),"fixture").await);
    let mut wrong=plan.clone();wrong.insert("componentId","other".into());config::atomic_json(fixture.0.join("services/fixture/deploy.json"),wrong).await.unwrap();
    compare("source","sourceBuildPlan",vec![path(&fixture.0),"fixture".into()],source::source_build_plan(fixture.0.clone(),"fixture").await);
    fs::write(fixture.0.join("services/fixture/deploy.json"),b"{").unwrap();
    compare("source","sourceBuildPlan",vec![path(&fixture.0),"fixture".into()],source::source_build_plan(fixture.0.clone(),"fixture").await);
    fs::remove_file(fixture.0.join("services/fixture/deploy.json")).unwrap();
    assert_json_eq(&source::source_build_plan(fixture.0.clone(),"fixture").await.unwrap(),&plan);
}

#[tokio::test]
async fn snapshot_metadata_binds_identity_root_schema_and_size_without_walking_source() {
    let fixture=ServiceFixture::new();let snapshot=snapshot(&fixture);let staging=fixture.0.join("staging");
    let manifest=IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("snapshot",snapshot.clone())]);
    config::atomic_json(staging.join("snapshot.json"),manifest.clone()).await.unwrap();
    assert!(!staging.join("source").exists());
    compare("source","verifySnapshotMetadata",vec![snapshot.clone(),host(&fixture)],source::verify_snapshot_metadata(snapshot.clone(),staging.clone()).await);
    compare("source","verifySnapshot",vec![snapshot.clone(),host(&fixture)],source::verify_snapshot(snapshot.clone(),staging.clone()).await.map(|_|IvyJson::Null));
    for key in ["snapshotId","sourceRoot"] {
        let mut changed=snapshot.clone();changed.insert(key,if key=="sourceRoot" {path(&staging.join("other"))}else{"changed".into()});
        compare("source","verifySnapshotMetadata",vec![changed.clone(),host(&fixture)],source::verify_snapshot_metadata(changed,staging.clone()).await);
    }
    let mut outside=snapshot.clone();outside.insert("sourceRoot",path(&fixture.0.join("outside")));
    compare("source","verifySnapshotMetadata",vec![outside.clone(),host(&fixture)],source::verify_snapshot_metadata(outside,staging.clone()).await);
    let mut wrong=manifest.clone();wrong.insert("schemaVersion",IvyJson::Number(2.0));config::atomic_json(staging.join("snapshot.json"),wrong).await.unwrap();
    compare("source","verifySnapshotMetadata",vec![snapshot.clone(),host(&fixture)],source::verify_snapshot_metadata(snapshot.clone(),staging.clone()).await);
    fs::write(staging.join("snapshot.json"),vec![b' ';8*1024*1024+1]).unwrap();
    compare("source","verifySnapshotMetadata",vec![snapshot.clone(),host(&fixture)],source::verify_snapshot_metadata(snapshot,staging).await);
}

#[test]
fn build_and_preparation_identities_preserve_recipe_and_utf16() {
    for identity in [JsString::from("source-hash"),JsString(vec![0xd800,0x61])] {
        let component=JsString::from("fixture");let version=JsString::from("1.0.0");
        compare("candidateIdentity","candidateBuildId",vec![IvyJson::String(component.clone()),IvyJson::String(version.clone()),IvyJson::String(identity.clone())],candidate_build_id(&component,&version,&identity).map(IvyJson::from));
    }
    for digit in ['0','1','2','3','4','7','8','b','f'] {
        let build=hash(&digit.to_string());
        compare("preparations","preparationIdForBuild",vec![build.clone().into()],preparations::preparation_id_for_build(&build).map(IvyJson::from));
    }
    for build in ["", "sha256:bad", &format!("sha256:{}","A".repeat(64))] {
        compare("preparations","preparationIdForBuild",vec![build.into()],preparations::preparation_id_for_build(build).map(IvyJson::from));
    }
}

#[tokio::test]
async fn preparation_records_are_atomic_owned_and_bound_to_release_identity() {
    let fixture=ServiceFixture::new();let staging=fixture.0.join("staging");let id=preparations::preparation_id_for_build(&hash("a")).unwrap();let root=staging.join(format!("build-{id}"));fs::create_dir_all(&root).unwrap();
    let record=IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("preparationId",id.clone().into()),("componentId","fixture".into()),("buildId",hash("a").into()),("snapshot",snapshot(&fixture)),("phase","building".into()),("candidate",IvyJson::Null),("startedAt","2026-01-01T00:00:00.000Z".into()),("updatedAt","2026-01-01T00:00:00.000Z".into()),("errorCode",IvyJson::Null)]);
    assert_eq!(preparations::preparation_record(staging.clone(),&id).await.unwrap(),None);
    compare("preparations","savePreparation",vec![host(&fixture),record.clone()],preparations::save_preparation(staging.clone(),record.clone()).await.map(|_|IvyJson::Null));
    assert_optional_json_eq(preparations::preparation_record(staging.clone(),&id).await.unwrap(),Some(record.clone()));
    let mut invalid=record.clone();invalid.insert("phase","invalid".into());
    compare("preparations","savePreparation",vec![host(&fixture),invalid.clone()],preparations::save_preparation(staging.clone(),invalid).await.map(|_|IvyJson::Null));
    assert_optional_json_eq(preparations::preparation_record(staging.clone(),&id).await.unwrap(),Some(record.clone()));
    let mut wrong=record.clone();wrong.insert("preparationId",preparations::preparation_id_for_build(&hash("b")).unwrap().into());config::atomic_json(root.join("preparation.json"),wrong).await.unwrap();
    assert_eq!(preparations::preparation_record(staging.clone(),&id).await.unwrap_err().code,"build_mismatch");
    let outside=fixture.0.join("outside");fs::create_dir(&outside).unwrap();fs::remove_file(root.join("preparation.json")).unwrap();fs::remove_dir(&root).unwrap();
    #[cfg(windows)] std::os::windows::fs::symlink_dir(&outside,&root).unwrap();
    #[cfg(unix)] std::os::unix::fs::symlink(&outside,&root).unwrap();
    compare("preparations","savePreparation",vec![host(&fixture),record.clone()],preparations::save_preparation(staging,record).await.map(|_|IvyJson::Null));
    assert!(!outside.join("preparation.json").exists());
}

#[tokio::test]
async fn preparation_inventory_preserves_errors_and_never_follows_linked_build_roots() {
    let fixture=ServiceFixture::new();let staging=fixture.0.join("staging");
    let mut host=host(&fixture);host.insert("hostId","fixture-host".into());
    let names:Vec<_>=["a","b","c","d"].iter().map(|digit| preparations::preparation_id_for_build(&hash(digit)).unwrap()).collect();
    let roots:Vec<_>=names.iter().map(|id| staging.join(format!("build-{id}"))).collect();
    for root in &roots[..3] {fs::create_dir_all(root).unwrap();}
    fs::create_dir_all(staging.join("build-not-an-id")).unwrap();
    fs::write(staging.join("build-not-an-id/ignored"),b"not inventoried").unwrap();
    let record=IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("preparationId",names[0].clone().into()),("componentId","fixture".into()),("buildId",hash("a").into()),("snapshot",snapshot(&fixture)),("phase","building".into()),("candidate",IvyJson::Null),("startedAt","2026-01-01T00:00:00.000Z".into()),("updatedAt","2026-01-01T00:00:00.000Z".into()),("errorCode",IvyJson::Null)]);
    preparations::save_preparation(staging.clone(),record).await.unwrap();
    fs::write(roots[0].join("payload"),b"retained").unwrap();
    fs::write(roots[1].join("payload"),b"unrecognized").unwrap();
    fs::write(roots[2].join("preparation.json"),b"{").unwrap();
    let outside=fixture.0.join("outside");fs::create_dir(&outside).unwrap();fs::write(outside.join("payload"),b"private").unwrap();
    #[cfg(windows)] std::os::windows::fs::symlink_dir(&outside,&roots[3]).unwrap();
    #[cfg(unix)] std::os::unix::fs::symlink(&outside,&roots[3]).unwrap();
    let inventory=preparations::preparation_inventory(host.clone()).await.unwrap();
    let entries=inventory.required("entries").unwrap().as_array().unwrap();assert_eq!(entries.len(),4);
    assert_eq!(entries[0].required("phase").unwrap(),&IvyJson::from("building"));
    assert_eq!(entries[1].required("bytes").unwrap(),&IvyJson::Number(12.0));
    assert_ne!(entries[2].required("code").unwrap(),&IvyJson::Null);
    assert_eq!(entries[3].required("code").unwrap(),&IvyJson::from("target_conflict"));
    assert_eq!(entries[3].required("bytes").unwrap(),&IvyJson::Number(0.0));
    // Clock and live volume availability are independently measured; compare the
    // stable inventory against the original Node implementation on these files.
    let keys=["schemaVersion","hostId","entries"];
    let stable=IvyJson::object(keys.map(|key| (key,inventory.required(key).unwrap().clone())));
    compare_config_oracle(IvyJson::object([("kind","preparations".into()),("operation","preparationInventory".into()),("args",IvyJson::Array(vec![host])),("pick",IvyJson::Array(keys.map(IvyJson::from).to_vec()))]),Ok(stable));
    assert_eq!(fs::read(outside.join("payload")).unwrap(),b"private");
    assert_eq!(fs::read(roots[0].join("payload")).unwrap(),b"retained");
}
