# backend-rust

Central coordination server for the hybrid Web2/Web3 social network.
Deployed on Fly.io (free tier, $0 infra cost). **Stores metadata + CIDs only —
media bytes never touch this server.**

## Responsibilities
- REST API (axum): auth (Google / email+Argon2id, rotating refresh tokens), social
  graph, feed (keyset pagination), profiles, likes/comments (batched counters),
  stories (24 h TTL + sweeper), DMs, live stream control plane
- WebSocket hub (`/v1/ws`): realtime DMs, notifications, live chunk CID fan-out
- SEO interceptor (`/s/:postId`): og: meta via public IPFS gateways for crawlers,
  302 to the web app for humans
- Pinning orchestration: `pin_jobs` worker against any IETF Pinning Service API
  (Pinata by default); live streams pin from chun