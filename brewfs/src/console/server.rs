use super::{ConsoleConfig, ConsoleState, api};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::Html,
    routing::get,
};
use tower_http::services::ServeDir;

pub fn build_router(config: ConsoleConfig) -> Router {
    let state = ConsoleState {
        auth_mode: config.auth_mode,
        static_dir: config.static_dir.clone(),
        csi_dashboard: config.csi_dashboard,
    };
    Router::new()
        .route("/api/health", get(api::health))
        .nest_service("/assets", ServeDir::new(config.static_dir.join("assets")))
        .fallback(get(spa_index))
        .with_state(state)
}

pub async fn serve(config: ConsoleConfig) -> anyhow::Result<()> {
    let listen = config.listen;
    let app = build_router(config);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    println!("brewfs console listening on http://{listen}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn spa_index(State(state): State<ConsoleState>) -> Result<Html<String>, StatusCode> {
    let html = tokio::fs::read_to_string(state.static_dir.join("index.html"))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Html(html))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::{AuthMode, ConsoleConfig};
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use std::net::SocketAddr;
    use tempfile::tempdir;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_route_returns_json() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<div id=\"root\"></div>").unwrap();
        let app = build_router(ConsoleConfig {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            static_dir: dir.path().to_path_buf(),
            auth_mode: AuthMode::Disabled,
            csi_dashboard: false,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["service"], "brewfs-console");
        assert_eq!(value["auth_mode"], "disabled");
    }

    #[tokio::test]
    async fn static_fallback_serves_index_html() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<div id=\"root\"></div>").unwrap();
        let app = build_router(ConsoleConfig {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            static_dir: dir.path().to_path_buf(),
            auth_mode: AuthMode::Disabled,
            csi_dashboard: false,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/filesystems")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(body.as_ref(), b"<div id=\"root\"></div>");
    }
}
