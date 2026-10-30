use futures_util::{SinkExt, StreamExt};
use ivy_sdk::websocket::{FRAGMENT_BYTES, config, send};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    io,
    pin::Pin,
    sync::atomic::{AtomicIsize, Ordering},
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Error, Message, protocol::Role},
};

struct Counted;
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            LIVE_BYTES.fetch_add(size as isize - layout.size() as isize, Ordering::Relaxed);
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

#[derive(Default)]
struct Discard {
    writes: usize,
    fail_after: Option<usize>,
}
impl AsyncRead for Discard {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}
impl AsyncWrite for Discard {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.writes += 1;
        if self.fail_after.is_some_and(|maximum| self.writes > maximum) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "fixture write failed",
            )));
        }
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn fragmented_messages_preserve_unicode_binary_control_and_following_messages() {
    for role in [Role::Server, Role::Client] {
        let opposite = match role {
            Role::Server => Role::Client,
            Role::Client => Role::Server,
        };
        let (a, b) = tokio::io::duplex(2048);
        let mut sender = WebSocketStream::from_raw_socket(a, role, Some(config())).await;
        // The independent protocol decoder rejects any oversized wire fragment,
        // while still accepting the original complete management message size.
        let mut peer = WebSocketStream::from_raw_socket(
            b,
            opposite,
            Some(config().max_frame_size(Some(FRAGMENT_BYTES))),
        )
        .await;
        let text = format!(
            "{}🦀{}",
            "x".repeat(FRAGMENT_BYTES - 1),
            "y".repeat(2 * FRAGMENT_BYTES)
        );
        let expected = text.clone();
        let binary = (0..2 * FRAGMENT_BYTES + 1)
            .map(|i| i as u8)
            .collect::<Vec<_>>();
        let expected_binary = binary.clone();
        let write = async {
            for message in [
                Message::Text(text.into()),
                Message::Binary(binary.into()),
                Message::Text("".into()),
                Message::Text("following".into()),
                Message::Ping(vec![1, 2, 3].into()),
            ] {
                send(&mut sender, message).await.unwrap();
            }
            assert_eq!(
                sender.next().await.unwrap().unwrap(),
                Message::Pong(vec![1, 2, 3].into())
            );
            send(&mut sender, Message::Close(None)).await.unwrap();
            assert!(matches!(sender.next().await, Some(Ok(Message::Close(_)))));
        };
        let read = async {
            assert_eq!(
                peer.next().await.unwrap().unwrap(),
                Message::Text(expected.into())
            );
            assert_eq!(
                peer.next().await.unwrap().unwrap(),
                Message::Binary(expected_binary.into())
            );
            assert_eq!(
                peer.next().await.unwrap().unwrap(),
                Message::Text("".into())
            );
            assert_eq!(
                peer.next().await.unwrap().unwrap(),
                Message::Text("following".into())
            );
            assert_eq!(
                peer.next().await.unwrap().unwrap(),
                Message::Ping(vec![1, 2, 3].into())
            );
            peer.flush().await.unwrap();
            assert!(matches!(peer.next().await, Some(Ok(Message::Close(_)))));
            peer.flush().await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(write, read);
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn failed_fragment_is_not_replayed_and_keeps_the_transport_error() {
    let transport = Discard {
        fail_after: Some(1),
        ..Default::default()
    };
    let mut socket =
        WebSocketStream::from_raw_socket(transport, Role::Server, Some(config())).await;
    let result = send(
        &mut socket,
        Message::Text("x".repeat(3 * FRAGMENT_BYTES).into()),
    )
    .await;
    assert!(matches!(result, Err(Error::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe));
    assert_eq!(socket.get_ref().writes, 2);
}

#[tokio::test]
async fn large_messages_do_not_leave_large_idle_output_buffers() {
    // No payload copy is retained by this transport. Keep each actual protocol
    // owner open after sending, and count live Rust allocations rather than RSS.
    let mut original =
        WebSocketStream::from_raw_socket(Discard::default(), Role::Server, Some(config())).await;
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    original
        .send(Message::Text("x".repeat(8 * 1024 * 1024).into()))
        .await
        .unwrap();
    let full = LIVE_BYTES.load(Ordering::Relaxed) - baseline;
    assert!(
        full >= 8 * 1024 * 1024,
        "Unfragmented retained bytes: {full}"
    );

    let mut bounded =
        WebSocketStream::from_raw_socket(Discard::default(), Role::Server, Some(config())).await;
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    for _ in 0..3 {
        send(
            &mut bounded,
            Message::Text("x".repeat(8 * 1024 * 1024).into()),
        )
        .await
        .unwrap();
    }
    let fragmented = LIVE_BYTES.load(Ordering::Relaxed) - baseline;
    assert!(
        fragmented >= 0 && fragmented < 256 * 1024,
        "Fragmented retained bytes: {fragmented}"
    );
    eprintln!("websocket_idle_output_bytes unfragmented={full} fragmented={fragmented}");
    // The owners are intentionally still alive at both measurements.
    assert!(original.get_ref().writes > 0);
    assert!(bounded.get_ref().writes >= 3 * 8 * 1024 * 1024 / FRAGMENT_BYTES);
}
