//! Local HTTP API (127.0.0.1 only) used by the macOS app, the menu bar helper and the
//! share extension. Every request needs the per-launch token from `daemon.json`.

mod routes;

use crate::Engine;
use crate::error::{Error, Result};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub const DEFAULT_PORT: u16 = 47921;

#[derive(Clone)]
pub(crate) struct AppState {
    pub engine: Engine,
    pub token: Arc<String>,
    pub port: u16,
}

/// Contents of `daemon.json`, read by the app to find the API.
#[derive(Serialize, serde::Deserialize, Debug, Clone)]
pub struct DaemonInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: String,
}

pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError(Error::Invalid(e.to_string()))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Invalid(_) => StatusCode::BAD_REQUEST,
            Error::Auth(_) | Error::OAuth(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Error::Net(_) | Error::Imap(_) | Error::Smtp(_) | Error::Pop3(_) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = serde_json::json!({ "error": self.0.to_string(), "code": self.0.code() });
        (status, axum::Json(body)).into_response()
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

fn random_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn host_allowed(headers: &HeaderMap, port: u16) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else { return true };
    let allowed = [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")];
    allowed.iter().any(|a| a == host)
}

async fn auth(State(st): State<AppState>, req: Request, next: Next) -> Response {
    if !host_allowed(req.headers(), st.port) {
        return (StatusCode::FORBIDDEN, "bad host").into_response();
    }
    let path = req.uri().path();
    if path == "/oauth/callback" || path == "/health" {
        return next.run(req).await;
    }
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let query_token = req.uri().query().and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "token").map(|(_, v)| v.to_string())
    });
    let ok = bearer.or(query_token).is_some_and(|t| constant_eq(t.as_bytes(), st.token.as_bytes()));
    if !ok {
        return (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({"error": "unauthorized", "code": "token"}))).into_response();
    }
    next.run(req).await
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub struct ServerHandle {
    pub port: u16,
    pub token: String,
}

/// Binds the API (preferring `DEFAULT_PORT`), writes `daemon.json` and serves until the
/// engine shuts down.
pub async fn serve(engine: Engine, preferred_port: Option<u16>) -> Result<ServerHandle> {
    let want = preferred_port.unwrap_or(DEFAULT_PORT);
    let listener = match tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], want))).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?,
    };
    let port = listener.local_addr()?.port();
    // Browsers may resolve "localhost" (Microsoft OAuth redirect) to ::1 first.
    let v6 = tokio::net::TcpListener::bind(SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port))).await.ok();
    engine.api_port.store(port, Ordering::Relaxed);
    let token = random_token();
    let state = AppState { engine: engine.clone(), token: Arc::new(token.clone()), port };
    let app: Router = routes::router(state.clone())
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024));

    let info = DaemonInfo { port, token: token.clone(), pid: std::process::id(), version: env!("CARGO_PKG_VERSION").into() };
    crate::paths::write_private(&engine.paths.daemon_info(), &serde_json::to_vec_pretty(&info)?)?;

    let shutdown = engine.shutdown_token();
    let app6 = app.clone();
    let s6 = shutdown.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).with_graceful_shutdown(async move { shutdown.cancelled().await }).await;
    });
    if let Some(l6) = v6 {
        tokio::spawn(async move {
            let _ = axum::serve(l6, app6).with_graceful_shutdown(async move { s6.cancelled().await }).await;
        });
    }
    tracing::info!(port, "api listening");
    Ok(ServerHandle { port, token })
}
