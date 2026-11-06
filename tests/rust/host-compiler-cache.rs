use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Progress(Mutex<Vec<String>>);
impl RetentionProgress for Progress {
    fn removed(&self, area: &str) { self.0.lock().unwrap().push(format!("removed:{area}")); }
    fn skipped(&self, area: &str, reason: &str, _: bool) { self.0.lock().unwrap().push(format!("{area}:{reason}")); }
}
struct Temp(PathBuf);
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

#[tokio::test]
async fn parallel_cache_initialization_recovers_empty_admission_and_retains_live_builds() {
    let temp = Temp(std::env::temp_dir().join(format!("ivy-compiler-cache-{}", uuid::Uuid::new_v4())));
    let staging = temp.0.join("staging");
    // A stopped admission before publishing owner.json left no owned payload.
    fs::create_dir_all(staging.join("compiler-cache")).unwrap();
    let lease = crate::source::storage_lease(staging.clone()).await.unwrap();
    let (left, right) = tokio::join!(begin(staging.clone()), begin(staging.clone()));
    let cache = left.unwrap(); assert_eq!(cache, right.unwrap());
    fs::create_dir(cache.join("cargo")).unwrap(); fs::write(cache.join("cargo/payload"), b"compiler output").unwrap();
    let host = J::object([("stagingRoot", crate::layout::path_json(&staging))]);
    let progress = Arc::new(Progress::default());
    let later = SystemClock.utc_millis() + 8 * 86_400_000;
    collect(&host, later, false, progress.clone()).await.unwrap();
    assert!(cache.join("cargo/payload").exists());
    assert!(progress.0.lock().unwrap().contains(&"compiler-cache:active_build".to_owned()));
    lease.close().await.unwrap();
    collect(&host, later, false, progress.clone()).await.unwrap();
    assert!(!cache.exists()); assert!(progress.0.lock().unwrap().contains(&"removed:payloads".to_owned()));
}
