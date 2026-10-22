// Deliberately a sibling of native_http, not one of its descendant modules.
use crate::native_http::{ConfigFuture, TlsConfigSource, TransportError, TransportErrorKind};
use hyper::Uri;
use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
struct OwnedCause {
    message: String,
    dropped: Arc<AtomicUsize>,
}
impl fmt::Display for OwnedCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl Error for OwnedCause {}
impl Drop for OwnedCause {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct ConfigFailure {
    profile: String,
    origin: String,
    cause: OwnedCause,
}
impl fmt::Display for ConfigFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TLS profile {} failed for {}", self.profile, self.origin)
    }
}
impl Error for ConfigFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.cause)
    }
}

struct FailingSource {
    profile: String,
    calls: AtomicUsize,
    dropped: Arc<AtomicUsize>,
}
impl TlsConfigSource for FailingSource {
    fn for_origin<'a>(&'a self, origin: &'a Uri) -> ConfigFuture<'a> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            // Both arguments remain borrowed across the suspension point.
            self.calls.fetch_add(1, Ordering::SeqCst);
            let cause: Box<dyn Error + Send + Sync + 'static> = Box::new(ConfigFailure {
                profile: self.profile.clone(),
                origin: origin.to_string(),
                cause: OwnedCause {
                    message: format!("configuration unavailable for {}", self.profile),
                    dropped: self.dropped.clone(),
                },
            });
            Err(TransportError::caused(TransportErrorKind::Tls, cause))
        })
    }
}

fn send<T: Send>(_: &T) {}
fn owned_error<T: Error + Send + Sync + 'static>(error: T) -> T {
    error
}

#[tokio::test(flavor = "current_thread")]
async fn sibling_tls_source_retains_owned_failure_after_its_borrows_end() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let error = {
        let source = FailingSource {
            profile: ["request", "profile"].join("-"),
            calls: AtomicUsize::new(0),
            dropped: dropped.clone(),
        };
        let origin: Uri = "https://example.invalid:8443/".parse().unwrap();
        let provider: &dyn TlsConfigSource = &source;
        let future = provider.for_origin(&origin);
        send(&future);
        let error = future.await.err().expect("injected configuration failure");
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        error
    };
    // Source and URI have now been destroyed; only the returned failure owns data.
    let error = owned_error(error);
    assert_eq!(error.kind, TransportErrorKind::Tls);
    assert_eq!(error.to_string(), "HTTP transport Tls");
    let config = error
        .source()
        .unwrap()
        .downcast_ref::<ConfigFailure>()
        .unwrap();
    assert_eq!(config.profile, "request-profile");
    assert_eq!(config.origin, "https://example.invalid:8443/");
    let cause = config
        .source()
        .unwrap()
        .downcast_ref::<OwnedCause>()
        .unwrap();
    assert_eq!(
        cause.to_string(),
        "configuration unavailable for request-profile"
    );
    assert!(cause.source().is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(error);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}
