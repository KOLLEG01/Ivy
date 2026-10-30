# Tungstenite input buffer correction

This is the source of tungstenite 0.30.0 from crates.io (upstream commit
`7f4aeaf0944992c5664c8fb8e0d54577c7e18020`). Upstream licenses and runtime
source are retained; examples, benches and development dependencies are omitted.

The only runtime change is in `src/protocol/frame/mod.rs`: after completing a
frame, detach a large shared input allocation once the unread tail fits in the
configured read window. Only that tail is copied; the received payload keeps its
original allocation until its owner releases it. Unread frames, partial headers
and the WebSocket message/control state remain intact. Normal buffers stay shared.

The workspace Cargo patch applies this correction to tungstenite 0.30.0 users.
Remove this copy when a pinned upstream release provides equivalent reclamation.
Protocol and live-allocation cases are in `tests/rust/sdk-websocket.rs`.
