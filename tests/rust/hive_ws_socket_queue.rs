use super::*;

#[test]
fn retained_string_capacity_is_charged_through_send_and_receiver_drop() {
    let budget = Sockets::default();
    let (mut socket, mut receiver) = Socket::new(&budget);
    let mut text = String::with_capacity(1024 * 1024);
    text.push_str("small");
    let capacity = text.capacity();
    socket.send_text(text).unwrap();
    let held = capacity + std::mem::size_of::<Queued>();
    assert_eq!(socket.buffered_amount(), held as f64);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES - held);
    let mut in_flight = receiver.try_recv().unwrap();
    drop(in_flight.message.take());
    assert_eq!(socket.buffered_amount(), held as f64);
    socket.terminate().unwrap();
    socket.send_text("ignored".into()).unwrap();
    socket.ping().unwrap();
    assert!(receiver.try_recv().is_err());
    drop(in_flight);
    assert_eq!(socket.buffered_amount(), 0.0);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES);

    let (mut socket, receiver) = Socket::new(&budget);
    socket.send_text("queued".into()).unwrap();
    assert!(socket.buffered_amount() > 0.0);
    drop(receiver);
    assert_eq!(socket.buffered_amount(), 0.0);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES);
    socket.ping().unwrap();
    assert!(!socket.is_open());
    assert_eq!(socket.buffered_amount(), 0.0);
}

#[test]
fn control_frames_have_a_count_bound_and_cannot_bypass_closed_admission() {
    let budget = Sockets::default();
    let (mut socket, receiver) = Socket::new(&budget);
    for _ in 0..QUEUED_MESSAGES {
        socket.ping().unwrap();
    }
    assert!(socket.is_open());
    let retained = socket.buffered_amount();
    assert_eq!(
        retained,
        (QUEUED_MESSAGES * std::mem::size_of::<Queued>()) as f64
    );
    socket.ping().unwrap();
    assert!(!socket.is_open());
    for _ in 0..100 {
        socket.ping().unwrap();
    }
    assert_eq!(socket.buffered_amount(), retained);
    drop(receiver);
    assert_eq!(socket.buffered_amount(), 0.0);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES);
}

#[test]
fn local_and_shared_byte_limits_release_every_rejected_reservation() {
    let budget = Sockets(Arc::new(Semaphore::new(4096)));
    let (mut first, first_queue) = Socket::new(&budget);
    let (mut second, second_queue) = Socket::new(&budget);
    first.send_text("a".repeat(3000)).unwrap();
    let available = budget.0.available_permits();
    second.send_text("b".repeat(3000)).unwrap();
    assert!(!second.is_open());
    assert!(first.is_open());
    assert_eq!(budget.0.available_permits(), available);
    assert_eq!(second.buffered_amount(), 0.0);
    drop((first_queue, second_queue));
    assert_eq!(budget.0.available_permits(), 4096);

    let budget = Sockets::default();
    let (mut socket, receiver) = Socket::new(&budget);
    let chunk = MANAGEMENT_FRAME_BYTES - std::mem::size_of::<Queued>() - 16;
    socket.send_text("x".repeat(chunk)).unwrap();
    socket.send_text("y".repeat(chunk)).unwrap();
    assert!(socket.is_open());
    let available = budget.0.available_permits();
    socket.send_text("z".repeat(4096)).unwrap();
    assert!(!socket.is_open());
    assert_eq!(budget.0.available_permits(), available);
    drop(receiver);
    assert_eq!(socket.buffered_amount(), 0.0);
    assert_eq!(budget.0.available_permits(), OUTBOUND_BYTES);
}
