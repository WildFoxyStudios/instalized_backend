//! Firebase Cloud Messaging HTTP v1 transport.
//!
//!   * `client::FcmClient` — owns the service-account key, mints and caches
//!     OAuth2 access tokens (RS256-signed self-JWT exchanged at
//!     `https://oauth2.googleapis.com/token`), and POSTs notification
//!     payloads to the FCM REST endpoint.
//!   * `worker` — poll loop that drains `notifications` whose
//!     `push_sent_at IS NULL AND read_at IS NULL`, fans them out to all
//!     registered `push_tokens` for the recipient, and writes back
//!     success / failure per row.
//!
//! Tokens returning `UNREGISTERED` / `INVALID_ARGUMENT` / `SENDER_ID_MISMATCH`
//! are soft-deleted (their row stays for re-install recovery, but they
//! are excluded from the next fan-out).

pub mod fcm;
pub mod worker;
