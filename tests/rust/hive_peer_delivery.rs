use super::*;

struct MemorySocket(Arc<Mutex<Vec<IvyJson>>>);
impl Socket for MemorySocket {
    fn is_open(&self) -> bool { true }
    fn buffered_amount(&self) -> f64 { 0.0 }
    fn send_text(&mut self, text: String) -> IvyResult<()> {
        self.0.lock().unwrap().push(ivy_contracts::json::parse(&text, usize::MAX)?);
        Ok(())
    }
    fn terminate(&mut self) -> IvyResult<()> { Ok(()) }
}
impl PeerSocket for MemorySocket {
    fn ping(&mut self) -> IvyResult<()> { Ok(()) }
}

#[test]
fn db_completion_and_timeout_deliver_once_and_release_the_request_once() {
    for timeout_first in [false, true] {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let peer = Peer(Arc::new(Connection {
            socket: Mutex::new(Box::new(MemorySocket(sent.clone()))),
            context: Mutex::new(IvyJson::Null),
            session: Mutex::new(Session {
                active: 1, alive: true, diagnosed_at: 0.0,
                filters: Vec::new(), changes: Vec::new(), expiry: None,
            }),
        }));
        let requests = RequestBudget::new(crate::admission::RequestBudgetOptions {
            maximum: 1, per_consumer: 1, ..Default::default()
        });
        let mut delivery = PeerDelivery {
            peer: peer.clone(), id: "call".into(),
            lease: requests.acquire(&"credential".into(), Some(4096)).unwrap(),
            finished: false, error: None,
        };
        let timeout = || IvyError::new("deadline_exceeded", "Original call timed out.")
            .with_outcome(Outcome::Unknown);
        if timeout_first {
            delivery.reject(timeout());
            delivery.finish(Ok("committed".into()));
        } else {
            delivery.finish(Ok("committed".into()));
            delivery.reject(timeout());
        }
        assert_eq!(peer.0.session.lock().unwrap().active, 0);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].get("id"), Some(&"call".into()));
        assert_eq!(sent[0].get("error").is_some(), timeout_first);
        assert!(requests.acquire(&"credential".into(), Some(4096)).is_ok());
    }
}
