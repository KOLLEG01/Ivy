use super::*;

#[test]
fn closed_socket_fences_admission_but_retains_its_pending_send() {
    let budget = Sockets::default();
    let (mut socket, mut receiver) = Socket::new(&budget);
    let mut text = String::with_capacity(1024 * 1024);
    text.push_str("small");
    let capacity = text.capacity();
    socket.send_text(text).unwrap();
    let held = capacity + std::mem::size_of::<Pending>();
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
fn queue_overflow_terminates_only_the_affected_hive_socket() {
    let budget = Sockets::default();
    let (mut full, receiver) = Socket::new(&budget);
    let (mut other, mut other_receiver) = Socket::new(&budget);
    for _ in 0..2 * ivy_contracts::limits::CONNECTION_IN_FLIGHT_REQUESTS {
        full.ping().unwrap();
    }
    assert!(full.is_open());
    let retained = full.buffered_amount();
    full.ping().unwrap();
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
