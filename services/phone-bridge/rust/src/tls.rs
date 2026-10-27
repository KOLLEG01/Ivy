//! SIP uses ring explicitly, while the SDK's WebSocket client uses the process
//! default. Both providers are linked, so feature-based selection is ambiguous.
pub(crate) fn initialize() {
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
}
