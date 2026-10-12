mod auth;
mod content;
mod core;
mod groups;
mod jobs;
mod storage;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use core::{env, ApiError, AppState, Reply, Request, Result};
use mongodb::bson::doc;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "moeflow_backend=info,tower_http=info".into()),
        )
        .init();
    let mongo = mongodb::Client::with_uri_str(env(
        "MONGODB_URI",
        "mongodb://127.0.0.1:27017/?replicaSet=rs0",
    ))
    .await?;
    let db = mongo.database(&env("MONGODB_DATABASE", "moeflow"));
    db.run_command(doc! {"ping":1}).await?;
    let hello = db.run_command(doc! {"hello":1}).await?;
    if !hello.contains_key("setName") && hello.get_str("msg").unwrap_or("") != "isdbgrid" {
        anyhow::bail!("MongoDB must be a replica set or sharded deployment: transactional resource binding requires it");
    }
    let state = Arc::new(AppState {
        mongo,
        db,
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(15))
            .build()?,
        cos: storage::Cos::new()?,
    });
    core::indexes(&state).await?;
    auth::bootstrap(&state).await?;
    if !state.cos.configured() {
        tracing::warn!("COS is not configured: upload/download operations will be unavailable");
    }
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(jobs::run(state.clone(), shutdown_rx));
    let origins = env(
        "CORS_ORIGINS",
        "http://localhost:8080,http://127.0.0.1:8080",
    )
    .split(',')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(|s| s.parse())
    .collect::<std::result::Result<Vec<axum::http::HeaderValue>, _>>()?;
    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::header::ACCEPT_LANGUAGE,
        ])
        .expose_headers([axum::http::HeaderName::from_static("x-pagination-count")]);
    let router = Router::new()
        .route("/health", get(health))
        .route("/ping", get(health))
        .fallback(dispatch)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let address = env("LISTEN_ADDR", "0.0.0.0:8000");
    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(%address,"HTTP API and persistent worker started in one process");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = shutdown_tx.send(true);
        })
        .await?;
    match tokio::time::timeout(Duration::from_secs(30), worker).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(%error,"worker stopped abnormally"),
        Err(_) => tracing::warn!("Worker drain timed out; unfinished leases recover after restart"),
    }
    Ok(())
}
async fn health(State(state): State<Arc<AppState>>) -> Result<Json<Value>> {
    state.db.run_command(doc! {"ping":1}).await?;
    Ok(Json(
        json!({"status":"ok","database":"connected","cos_configured":state.cos.configured(),"worker":"same-process","gc_enabled":env("GC_ENABLED","false")=="true","resource_backup_confirmed":env("RESOURCE_BACKUP_CONFIRMED","false")=="true"}),
    ))
}
async fn dispatch(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    match dispatch_inner(state, method, uri, headers, bytes).await {
        Ok(reply) => reply.into_response(),
        Err(error) => error.into_response(),
    }
}
async fn dispatch_inner(
    state: Arc<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Reply> {
    let path = uri
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|segment| {
            percent_encoding::percent_decode_str(segment)
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(|_| ApiError::bad("Invalid UTF-8 path segment"))
        })
        .collect::<Result<Vec<_>>>()?;
    if path.first().map(String::as_str) != Some("v1") {
        return Err(ApiError::not_found("Route not found"));
    }
    let user = match headers.get(axum::http::header::AUTHORIZATION) {
        Some(value) => {
            let bearer = value
                .to_str()
                .map_err(|_| ApiError::unauthorized("Invalid Authorization header"))?
                .strip_prefix("Bearer ")
                .ok_or_else(|| ApiError::unauthorized("Expected Bearer session"))?;
            Some(auth::authenticate(&state, bearer).await?)
        }
        None => None,
    };
    let body = if bytes.is_empty() {
        json!({})
    } else {
        if !headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|s| s.starts_with("application/json"))
        {
            return Err(ApiError::bad(
                "Only JSON is accepted; upload file bytes directly to COS",
            ));
        }
        serde_json::from_slice(&bytes).map_err(|_| ApiError::bad("Invalid JSON request"))?
    };
    let query = serde_urlencoded::from_str(uri.query().unwrap_or(""))
        .map_err(|_| ApiError::bad("Invalid query parameters"))?;
    let request = Request {
        state,
        user,
        method,
        path: path.into_iter().skip(1).collect(),
        query,
        body,
    };
    if let Some(reply) = auth::handle(&request).await? {
        return Ok(reply);
    }
    if let Some(reply) = storage::handle(&request).await? {
        return Ok(reply);
    }
    if let Some(reply) = jobs::handle(&request).await? {
        return Ok(reply);
    }
    if let Some(reply) = content::handle(&request).await? {
        return Ok(reply);
    }
    if let Some(reply) = groups::handle(&request).await? {
        return Ok(reply);
    }
    Err(ApiError::not_found("Route not found"))
}
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! { _=tokio::signal::ctrl_c()=>{},_=term.recv()=>{} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
