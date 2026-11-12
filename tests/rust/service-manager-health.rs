use super::*;
use tokio::sync::Notify;

#[tokio::test]
async fn slow_writer_coalesces_pulses_and_confirms_one_final_unready() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let reporter = HealthReporter::with_writer({
        let seen = seen.clone(); let entered = entered.clone(); let release = release.clone();
        move |ready, message, generation| {
            let seen = seen.clone(); let entered = entered.clone(); let release = release.clone();
            async move {
                let first = { let mut seen = seen.lock().unwrap(); seen.push((ready, message, generation)); seen.len() == 1 };
                if first { entered.notify_one(); release.notified().await; }
                Ok(())
            }
        }
    });
    let first = reporter.write(true, Some(1), "first".into());
    entered.notified().await;
    for _ in 0..10_000 { drop(reporter.write(true, Some(1), "pulse".into())); }
    drop(reporter.write(true, Some(2), "new generation".into()));
    assert!(reporter.write(false, Some(1), "late old generation".into()).await.is_err());
    let flush = reporter.flush();
    let final_one = reporter.finalize();
    let final_two = reporter.finalize();
    assert!(reporter.write(true, Some(3), "late ready".into()).await.is_err());
    release.notify_one();
    let (a,b,c,d) = tokio::join!(first, flush, final_one, final_two);
    a.unwrap(); b.unwrap(); c.unwrap(); d.unwrap();
    reporter.flush().await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1], (false, "ServiceManager stopped.".into(), None));
}

#[tokio::test]
async fn failed_write_does_not_poison_a_newer_generation() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reporter = HealthReporter::with_writer({
        let count = count.clone();
        move |_, _, _| {
            let first = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            async move { if first { Err(ivy_contracts::IvyError::new("fixture", "write failed")) } else { Ok(()) } }
        }
    });
    assert!(reporter.write(true, Some(1), "failed".into()).await.is_err());
    reporter.write(true, Some(2), "recovered".into()).await.unwrap();
    assert!(reporter.write(false, Some(1), "late".into()).await.is_err());
    reporter.finalize().await.unwrap();
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 3);
}
