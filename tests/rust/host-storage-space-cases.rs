use super::{ServiceFixture, compare_config_oracle};
use ivy_contracts::{IvyJson, IvyResult};
use ivy_host_runtime::storage_space::{self, RetentionSchedule};
use std::{fs, path::Path};

fn path(value: &Path) -> IvyJson { value.to_string_lossy().to_string().into() }
fn compare(operation: &str, args: Vec<IvyJson>, result: IvyResult<IvyJson>) {
    compare_config_oracle(IvyJson::object([("kind","storage".into()),("operation",operation.into()),("args",IvyJson::Array(args))]),result);
}

#[test]
fn storage_pressure_and_retry_boundaries_match_node() {
    let gib=1024.0_f64.powi(3);
    for (available,total) in [(2.0*gib-1.0,0.0),(2.0*gib,0.0),(3.0*gib,30.0*gib),(3.0*gib,30.0*gib+1.0),(10.0*gib,20.0*gib)] {
        compare("storageIsLow",vec![IvyJson::Number(available),IvyJson::Number(total)],Ok(storage_space::storage_is_low(available,total).into()));
    }
    let mut schedule=RetentionSchedule::new(1000.0);let mut steps=Vec::new();let mut values=Vec::new();
    for (method,at,pressure) in [("due",60_999.0,false),("due",61_000.0,false),("due",1000.0,true),("started",2000.0,false),("due",3_601_999.0,true),("due",3_602_000.0,true),("due",86_401_999.0,false),("due",86_402_000.0,false),("failed",3000.0,false),("due",3_602_999.0,false),("due",3_603_000.0,false)] {
        let mut args=vec![IvyJson::Number(at)];
        let value=match method {"due"=>{args.push(pressure.into());schedule.due(at,pressure).into()},"started"=>{schedule.started(at);IvyJson::Null},_=>{schedule.failed(at);IvyJson::Null}};
        steps.push(IvyJson::object([("method",method.into()),("args",IvyJson::Array(args))]));values.push(value);
    }
    compare_config_oracle(IvyJson::object([("kind","storage".into()),("operation","schedule".into()),("now",IvyJson::Number(1000.0)),("steps",IvyJson::Array(steps))]),Ok(IvyJson::Array(values)));
}

#[tokio::test]
async fn storage_inventory_counts_hardlinks_without_following_symlinks() {
    let fixture=ServiceFixture::new();let root=fixture.0.join("tree");fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("file"),b"abc").unwrap();fs::hard_link(root.join("file"),root.join("nested/hardlink")).unwrap();
    let outside=fixture.0.join("outside");fs::write(&outside,b"must not count").unwrap();
    #[cfg(windows)] std::os::windows::fs::symlink_file(&outside,root.join("link")).unwrap();
    #[cfg(unix)] std::os::unix::fs::symlink(&outside,root.join("link")).unwrap();
    assert_eq!(storage_space::tree_bytes(root.clone(),None).await.unwrap(),6.0);
    compare("treeBytes",vec![path(&root)],storage_space::tree_bytes(root.clone(),None).await.map(IvyJson::Number));
    compare("treeBytes",vec![path(&root.join("link")),path(&root)],storage_space::tree_bytes(root.join("link"),Some(root.clone())).await.map(IvyJson::Number));
    fs::remove_file(&outside).unwrap();
    // An explicit boundary skips dangling symlinks; the implicit boundary must resolve first.
    compare("treeBytes",vec![path(&root.join("link")),path(&root)],storage_space::tree_bytes(root.join("link"),Some(root.clone())).await.map(IvyJson::Number));
    compare("treeBytes",vec![path(&root.join("link"))],storage_space::tree_bytes(root.join("link"),None).await.map(IvyJson::Number));
    compare("treeBytes",vec![path(&root),path(&root.join("nested"))],storage_space::tree_bytes(root.clone(),Some(root.join("nested"))).await.map(IvyJson::Number));
}

#[tokio::test]
async fn storage_inventory_enforces_depth_and_handles_files() {
    let fixture=ServiceFixture::new();let root=fixture.0.join("tree");fs::create_dir(&root).unwrap();
    let mut nested=root.clone();for _ in 0..64 {nested=nested.join("d");fs::create_dir(&nested).unwrap();}
    compare("treeBytes",vec![path(&root)],storage_space::tree_bytes(root.clone(),None).await.map(IvyJson::Number));
    fs::create_dir(nested.join("d")).unwrap();
    compare("treeBytes",vec![path(&root)],storage_space::tree_bytes(root,None).await.map(IvyJson::Number));
    let file=fixture.0.join("bytes");fs::write(&file,b"12345").unwrap();
    compare("treeBytes",vec![path(&file)],storage_space::tree_bytes(file,None).await.map(IvyJson::Number));
}

#[tokio::test]
async fn measured_storage_supports_files_missing_host_roots_and_pressure_gate() {
    let fixture=ServiceFixture::new();let file=fixture.0.join("file");fs::write(&file,b"fixture").unwrap();
    let directory_bytes=storage_space::available_storage_bytes(fixture.0.clone()).await.unwrap();
    let file_bytes=storage_space::available_storage_bytes(file).await.unwrap();
    // Free space can change between independent system calls; assert the contract
    // without requiring the disk to remain idle during the comparison.
    assert!(directory_bytes>=0.0 && file_bytes>=0.0 && directory_bytes<=9_007_199_254_740_991.0);
    assert_eq!(directory_bytes.fract(),0.0);assert_eq!(file_bytes.fract(),0.0);
    compare("availableStorageBytes",vec![path(&fixture.0.join("missing"))],storage_space::available_storage_bytes(fixture.0.join("missing")).await.map(IvyJson::Number));
    let config=IvyJson::object([("runtimeRoot",path(&fixture.0.join("missing/runtime"))),("artifactRoot",path(&fixture.0.join("missing/artifacts"))),("stagingRoot",path(&fixture.0.join("missing/staging")))]);
    let value=storage_space::host_storage_space(config).await.unwrap();
    for area in ["runtime","artifacts","staging"] {assert!(value.required("availableBytes").unwrap().get(area).is_some());}
    assert!(!fixture.0.join("missing").exists());
    compare("requireStorageSpace",vec![path(&fixture.0),IvyJson::Number(0.0)],storage_space::require_storage_space(fixture.0.clone(),0.0).await.map(|_|IvyJson::Null));
    let failure=storage_space::require_storage_space(fixture.0.clone(),9_007_199_254_740_992.0).await.unwrap_err();
    assert_eq!(failure.code,"storage_pressure");assert_eq!(failure.outcome,ivy_contracts::Outcome::NotExecuted);
    let details=failure.details.unwrap();assert!(matches!(details.get("availableBytes"),Some(IvyJson::Number(v)) if *v>=0.0));
    assert_eq!(details.required("requiredBytes").unwrap(),&IvyJson::Number(9_007_199_254_740_992.0));
}
