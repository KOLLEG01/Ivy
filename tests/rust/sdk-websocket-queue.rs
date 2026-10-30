use super::*;

#[test]
fn retained_string_capacity_stays_charged_until_send_guard_drops() {
    let budget = SendBudget::default();
    let (sender, mut receiver) = budget.channel();
    let mut text = String::with_capacity(1024 * 1024);
    text.push_str("small");
    let bytes = text.capacity() + std::mem::size_of::<Pending>();
    sender.text(text).unwrap();
    assert_eq!(sender.buffered_bytes(), bytes);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES - bytes);
    let mut pending = receiver.try_recv().unwrap();
    drop(pending.take_message());
    assert_eq!(sender.buffered_bytes(), bytes);
    drop(receiver);
    assert_eq!(sender.text("closed".into()), Err(SendError::Closed));
    assert_eq!(sender.buffered_bytes(), bytes);
    drop(pending);
    assert_eq!(sender.buffered_bytes(), 0);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES);
}

#[test]
fn count_bound_rejects_without_closing_and_recovers_after_drain() {
    let budget = SendBudget::default();
    let (sender, mut receiver) = budget.channel();
    for _ in 0..QUEUED_MESSAGES {
        sender.ping().unwrap();
    }
    let bytes = QUEUED_MESSAGES * std::mem::size_of::<Pending>();
    assert_eq!(sender.buffered_bytes(), bytes);
    assert_eq!(sender.ping(), Err(SendError::Full));
    assert_eq!(sender.buffered_bytes(), bytes);
    drop(receiver.try_recv().unwrap());
    sender.text("after drain".into()).unwrap();
    drop(receiver);
    assert_eq!(sender.buffered_bytes(), 0);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES);
    assert_eq!(sender.ping(), Err(SendError::Closed));
}

#[test]
fn local_and_shared_limits_return_all_rejected_reservations() {
    let budget = SendBudget(Arc::new(Semaphore::new(4096)));
    let (first, first_queue) = budget.channel();
    let (second, second_queue) = budget.channel();
    first.text("a".repeat(3000)).unwrap();
    let available = budget.0.available_permits();
    assert_eq!(second.text("b".repeat(3000)), Err(SendError::Full));
    assert_eq!(budget.0.available_permits(), available);
    assert_eq!(second.buffered_bytes(), 0);
    drop((first_queue, second_queue));
    assert_eq!(budget.0.available_permits(), 4096);

    let budget = SendBudget::default();
    let (sender, receiver) = budget.channel();
    let chunk = MANAGEMENT_FRAME_BYTES - std::mem::size_of::<Pending>() - 16;
    sender.text("x".repeat(chunk)).unwrap();
    sender.text("y".repeat(chunk)).unwrap();
    let retained = sender.buffered_bytes();
    let available = budget.0.available_permits();
    assert_eq!(sender.text("z".repeat(4096)), Err(SendError::Full));
    assert_eq!(sender.buffered_bytes(), retained);
    assert_eq!(budget.0.available_permits(), available);
    drop(receiver);
    assert_eq!(sender.buffered_bytes(), 0);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES);
}

#[test]
fn concurrent_admission_keeps_count_and_byte_reservations_consistent() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let budget = SendBudget::default();
    let (sender, receiver) = budget.channel();
    let accepted = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..QUEUED_MESSAGES {
                    match sender.ping() {
                        Ok(()) => {
                            accepted.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(SendError::Full) => {}
                        Err(SendError::Closed) => panic!("Queue closed during admission"),
                    }
                }
            });
        }
    });
    assert_eq!(accepted.load(Ordering::SeqCst), QUEUED_MESSAGES);
    let retained = QUEUED_MESSAGES * std::mem::size_of::<Pending>();
    assert_eq!(sender.buffered_bytes(), retained);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES - retained);
    drop(receiver);
    assert_eq!(sender.buffered_bytes(), 0);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES);
}

#[tokio::test]
async fn sdk_close_and_incoming_failure_drop_blocked_io_and_its_reservations() {
    use crate::service_connection::{ConnectionOptions, ServiceConnection};
    use crate::{IvyJson, JsString};
    use futures_util::SinkExt;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

    for remote_failure in [false, true] {
        let available = service_budget().0.available_permits();
        let (client, server) = tokio::io::duplex(1024);
        let client = WebSocketStream::from_raw_socket(client, Role::Client, Some(config())).await;
        let mut peer = WebSocketStream::from_raw_socket(server, Role::Server, Some(config())).await;
        let connection =
            ServiceConnection::connected(client, ConnectionOptions::new("fixture".into()));
        connection
            .notification(
                "fixture".into(),
                "large".into(),
                "1.0.0".into(),
                IvyJson::String(JsString(vec![97; 8 * 1024 * 1024])),
            )
            .unwrap();
        let mut first = [0];
        tokio::time::timeout(
            Duration::from_secs(5),
            peer.get_mut().read_exact(&mut first),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(first[0], 0x01);
        assert!(service_budget().0.available_permits() < available);
        if remote_failure {
            // Ingress must remain live while the real output pipe is full.
            peer.send(Message::Binary(vec![1].into())).await.unwrap();
        } else {
            connection.close();
        }
        tokio::time::timeout(Duration::from_secs(5), connection.closed())
            .await
            .unwrap();
        assert_eq!(
            connection.close_reason().unwrap().code,
            if remote_failure {
                "invalid_frame"
            } else {
                "service_unavailable"
            }
        );
        // EOF proves the actual I/O owner closed. Only its small stalled pipe
        // remains to drain; no accepted handler or external action is replayed.
        let mut rest = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            peer.get_mut().read_to_end(&mut rest),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(rest.len() <= 1024);
        assert_eq!(service_budget().0.available_permits(), available);
    }
}
