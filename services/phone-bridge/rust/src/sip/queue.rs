//! Bounded request delivery with a separate lane for dialog termination.
use std::task::{Context, Poll};
use tokio::sync::mpsc;

#[derive(Clone, Copy)]
pub(super) enum Priority {
    Ordinary,
    Control,
}
pub(super) struct Sender<T> {
    ordinary: mpsc::Sender<T>,
    control: mpsc::Sender<T>,
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            ordinary: self.ordinary.clone(),
            control: self.control.clone(),
        }
    }
}
pub(super) struct Receiver<T> {
    ordinary: mpsc::Receiver<T>,
    control: mpsc::Receiver<T>,
}
pub(super) fn channel<T>(ordinary: usize, control: usize) -> (Sender<T>, Receiver<T>) {
    let (ordinary, ordinary_rx) = mpsc::channel(ordinary);
    let (control, control_rx) = mpsc::channel(control);
    (
        Sender { ordinary, control },
        Receiver {
            ordinary: ordinary_rx,
            control: control_rx,
        },
    )
}
impl<T> Sender<T> {
    pub async fn send(&self, value: T, priority: Priority) -> Result<(), T> {
        match priority {
            Priority::Ordinary => &self.ordinary,
            Priority::Control => &self.control,
        }
        .send(value)
        .await
        .map_err(|error| error.0)
    }
    pub fn is_closed(&self, priority: Priority) -> bool {
        match priority {
            Priority::Ordinary => &self.ordinary,
            Priority::Control => &self.control,
        }
        .is_closed()
    }
    pub fn try_send(&self, value: T, priority: Priority) -> Result<(), T> {
        match priority {
            Priority::Ordinary => &self.ordinary,
            Priority::Control => &self.control,
        }
        .try_send(value)
        .map_err(|error| error.into_inner())
    }
}
impl<T> Receiver<T> {
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let control_closed = match self.control.poll_recv(cx) {
            Poll::Ready(Some(value)) => return Poll::Ready(Some(value)),
            Poll::Ready(None) => true,
            Poll::Pending => false,
        };
        match self.ordinary.poll_recv(cx) {
            Poll::Ready(None) if !control_closed => Poll::Pending,
            result => result,
        }
    }
    pub async fn recv(&mut self) -> Option<T> {
        std::future::poll_fn(|cx| self.poll_recv(cx)).await
    }
    pub fn close(&mut self) {
        self.control.close();
        self.ordinary.close();
    }
}
