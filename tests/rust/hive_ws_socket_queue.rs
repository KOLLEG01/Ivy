use super::*;

#[test]
fn closed_socket_fences_admission_but_retains_its_pending_send() {
    let budget = Sockets::default();
    let (mut socket, mut receiver) = Socket::new(&budget);
    let mut text = String::with_capacity(1024 * 1024);
    text.push_str("small");
    let capacity = text.capacity();
    socket.send_text(text).unwrap();
    let held = socket.buffered_amount() as usize;
    assert!(held >= capacity + std::mem::size_of::<Pending>());
    assert_eq!(socket.buffered_amount(), held as f64);
    let mut in_flight = receiver.try_recv().unwrap();
    drop(in_flight.take_message());
    socket.terminate().unwrap();
    socket.send_text("ignored".into()).unwrap();
    socket.ping().unwrap();
    assert!(receiver.try_recv().is_err());
    assert_eq!(socket.buffered_amount(), held as f64);
    drop(in_flight);
    assert_eq!(socket.buffered_amount(), 0.0);
}

#[test]
fn byte_overflow_terminates_only_the_affected_hive_socket() {
    let budget = Sockets::default();
    let (mut full, receiver) = Socket::new(&budget);
    let (mut other, mut other_receiver) = Socket::new(&budget);
    let mut text = String::with_capacity(ivy_contracts::limits::MANAGEMENT_FRAME_BYTES);
    text.push_str("small");
    full.send_text(text).unwrap();
    assert!(full.is_open());
    let retained = full.buffered_amount();
    let mut text = String::with_capacity(ivy_contracts::limits::MANAGEMENT_FRAME_BYTES);
    text.push_str("small");
    full.send_text(text).unwrap();
    assert!(!full.is_open());
    for _ in 0..100 {
        full.ping().unwrap();
    }
    assert_eq!(full.buffered_amount(), retained);
    other.ping().unwrap();
    assert!(other.is_open());
    assert_eq!(
        other_receiver.try_recv().unwrap().take_message(),
        Message::Ping(Vec::new().into())
    );
    drop(receiver);
    assert_eq!(full.buffered_amount(), 0.0);
    drop(other_receiver);
    other.ping().unwrap();
    assert!(!other.is_open());
    assert_eq!(other.buffered_amount(), 0.0);
}

#[test]
fn native_notification_burst_does_not_close_a_small_send_queue() {
    let budget = Sockets::default();
    let (mut socket, mut receiver) = Socket::new(&budget);
    // A synchronous publication burst can precede the I/O owner's next poll.
    // Its retained bytes are small even when it exceeds 128 individual frames.
    for index in 0..1000 {
        socket.send_text(format!("notification-{index}")).unwrap();
    }
    assert!(socket.is_open());
    assert!(socket.buffered_amount() < 1024.0 * 1024.0);
    for index in 0..1000 {
        assert_eq!(
            receiver.try_recv().unwrap().take_message(),
            Message::Text(format!("notification-{index}").into())
        );
    }
    assert_eq!(socket.buffered_amount(), 0.0);
    assert!(receiver.try_recv().is_err());
}
