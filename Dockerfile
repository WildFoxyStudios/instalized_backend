# Multi-stage build for Fly.io free tier (256 MB shared VM).
FROM rust:1.91-slim AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/backend-rust /usr/local/bin/backend-rust
ENV PORT=8080 DATABASE_PATH=/data/app.db
EXPOSE 8080
CMD ["backend-rust"]
