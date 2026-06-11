# Instalized — backend (Rust)

Central coordination server for the Instalized hybrid Web2/Web3 social network.

## Stack
axum · tokio · sqlx (SQLite/WAL) · jsonwebtoken · tower · firebase-http-v1 · hmac · base32

## Responsibilities
- REST API (axum): auth (email/password + Google OAuth), social graph, feed, profiles,
  likes, comments, DMs (text/image/voice/video), stories TTL, 2FA TOTP, blocks, mutes,
  reports, privacy, notifications prefs, archive, recently deleted, highlights,
  hashtag grid, comment threads + likes, post tags/mentions, hard delete.
- WebSocket server: realtime DMs, typing indicators, message reactions fan-out,
  live-stream CID signaling, push-notification fan-out.
- Push delivery: Firebase Cloud Messaging HTTP v1 worker, dead-token soft-delete,
  per-user notification preferences.
- SEO interceptor: serves SSR-friendly og: meta tags to crawler user-agents via
  public IPFS gateways.
- Pinning orchestration: registers CIDs with Pinata on first-chunk upload.
- Storage: SQLite (WAL mode, in-memory write batching). Only metadata + CIDs;
  media bytes live on IPFS.

## Layout
```
src/
  api/        — REST handlers (auth, social, posts, users, discover, mod, notifications, totp, dm)
  auth/       — JWT, password hashing, Google OAuth
  db/         — connection pool, migrations 001..007
  pinning/    — Pinata registration
  push/       — FCM worker (HTTP v1, OAuth2 service account)
  seo/        — crawler interceptor
  ws/         — WebSocket hub (DMs, live, typing, reactions)
migrations/   — SQL schema, applied at startup
tests/        — integration tests (api, social_v2, totp, push, dm_media, dm_reactions)
```

## Build & test
```bash
cargo build --release
cargo test
```

Required env vars (see `.env.example`): DATABASE_URL, JWT_SECRET, PINATA_JWT,
FCM_SERVICE_ACCOUNT_JSON, FCM_PROJECT_ID, GOOGLE_OAUTH_CLIENT_ID, GOOGLE_OAUTH_CLIENT_SECRET,
APP_BASE_URL, RUST_LOG.

## Deployment
See `../DEPLOYMENT.md` and `RUNBOOK_FCM.md`.
