//! WebSocket layer: hub (fan-out registry), protocol (envelope), session (per-socket loop).
//!
//! Lifecycle policy (manifesto #5): clients close the socket the moment they go to
//! background; the server keeps no offline queues — missed events are re-fetched over
//! REST on reconnect, realtime resumes via push notifications → foreground → WS.

pub mod hub;
pub mod protocol;
pub mod session;
