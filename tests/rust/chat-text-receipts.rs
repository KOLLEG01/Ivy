use super::*;
use crate::whatsapp::journal_io::JournalIo;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn successful_socket_write_waits_for_receipt_across_restart_and_errors_keep_the_original_attempt()
 {
    let root = std::env::temp_dir().join(format!("ivy-text-receipts-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let fixture = Fixture(root);
    let path = fixture.0.join("journal.sqlite");
    let journal = Journal::new(JournalIo::open(&path).unwrap());
    let at = 1_800_000_000_000.0;
    let id = journal
        .access(|j| {
            let id = j.enqueue(
                &"original".into(),
                "15555550101@s.whatsapp.net",
                &"Original 😀".into(),
                false,
                at as i64,
            )?[0]
                .clone();
            j.mark(&id, OutputState::Sending)?;
            j.set(
                &format!("send-attempt:{id}"),
                &Json::object([("count", 1.0.into()), ("at", at.into())]),
            )?;
            Ok(id)
        })
        .unwrap();
    record_text_submission(&journal, &id, None).unwrap();
    assert!(
        !journal.access(|j| j.sent(&[id.clone()])).unwrap(),
        "The socket write does not prove a server acknowledgement"
    );
    journal.close().await.unwrap();
    let journal = Journal::new(JournalIo::open(&path).unwrap());
    assert_eq!(
        journal
            .access(|j| Ok(j.outgoing()?[0].state.clone()))
            .unwrap(),
        "sending"
    );
    let cause = "Bounded native failure 😀".repeat(40);
    record_text_submission(&journal, &id, Some(&cause)).unwrap();
    assert_eq!(
        journal
            .access(|j| Ok(j.outgoing()?[0].state.clone()))
            .unwrap(),
        "unknown"
    );
    let attempt = journal.get(&format!("send-attempt:{id}")).unwrap();
    assert_eq!(attempt.get("count"), Some(&1.0.into()));
    assert_eq!(attempt.get("at"), Some(&at.into()));
    assert_eq!(
        attempt.required("nativeError").unwrap().as_str().unwrap(),
        cause.chars().take(512).collect::<String>()
    );
    // A receipt can race the send's return or a later error. The original
    // acknowledgement remains authoritative; submission never downgrades it.
    journal.access(|j| j.mark(&id, OutputState::Sent)).unwrap();
    record_text_submission(&journal, &id, Some("Late failure")).unwrap();
    assert!(journal.access(|j| j.sent(&[id])).unwrap());
    journal.close().await.unwrap();
}
