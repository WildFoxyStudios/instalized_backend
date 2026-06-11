//! Entry point: config from env → build app → spawn workers → serve.

use backend_rust::config::Config;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();

    let cfg = Config::from_env();
    let port = cfg.port;
    let (app, state) = match backend_rust::build(cfg) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("startup failed: {e}");
            std::process::exit(1);
        }
    };
    backend_rust::spawn_workers(&state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind listener");
    tracing::info!("backend-rust listening on {addr}");

    let shutdown_state = state.clone();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down — flushing counter batches");
            shutdown_state.db.writer.flush_now().await;
        })
        .await
        .expect("server error");
}
