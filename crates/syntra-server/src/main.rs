use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use syntra_core::{Listing, Proposal, RejectError, Store};

/// 승인 시 워크스페이스에 쓰기가 일어나므로 기본값은 로컬 접속만 허용한다.
const DEFAULT_ADDR: &str = "127.0.0.1:7878";
const DEFAULT_PROPOSALS_DIR: &str = ".syntra/proposals";

const INDEX_HTML: &str = include_str!("../static/index.html");

#[derive(Deserialize)]
struct RejectRequest {
    reason: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

impl From<RejectError> for ApiError {
    fn from(error: RejectError) -> Self {
        let status = match error {
            RejectError::BlankReason => StatusCode::UNPROCESSABLE_ENTITY,
            RejectError::NotFound(_) => StatusCode::NOT_FOUND,
            RejectError::AlreadyReviewed(_) => StatusCode::CONFLICT,
            RejectError::Io(_) | RejectError::Json(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self(status, error.to_string())
    }
}

async fn list_proposals(State(store): State<Arc<Store>>) -> Result<Json<Listing>, ApiError> {
    store
        .list()
        .map(Json)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn reject_proposal(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(request): Json<RejectRequest>,
) -> Result<Json<Proposal>, ApiError> {
    Ok(Json(store.reject(&id, &request.reason)?))
}

fn app(store: Arc<Store>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/proposals", get(list_proposals))
        .route("/api/proposals/{id}/reject", post(reject_proposal))
        .with_state(store)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::var("SYNTRA_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let dir =
        std::env::var("SYNTRA_PROPOSALS").unwrap_or_else(|_| DEFAULT_PROPOSALS_DIR.to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("SynTra: http://{} (제안서: {dir})", listener.local_addr()?);

    axum::serve(listener, app(Arc::new(Store::new(dir))))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, header},
    };
    use tower::ServiceExt;

    fn app_with_proposal() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("one.json"),
            r#"{"kind":"edit","file_name":"a.rs","regions":[{"before":"a","after":"b","purpose":"p"}]}"#,
        )
        .unwrap();
        let app = app(Arc::new(Store::new(dir.path())));
        (dir, app)
    }

    async fn reject(app: Router, id: &str, body: &str) -> StatusCode {
        let request = Request::post(format!("/api/proposals/{id}/reject"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        app.oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (_dir, app) = app_with_proposal();
        let request = Request::get("/api/health").body(Body::empty()).unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn reject_status_codes() {
        let (_dir, app) = app_with_proposal();
        let body = r#"{"reason":"사유"}"#;
        assert_eq!(
            reject(app.clone(), "one", r#"{"reason":"  "}"#).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            reject(app.clone(), "one", "{}").await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            reject(app.clone(), "none", body).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(reject(app.clone(), "one", body).await, StatusCode::OK);
        assert_eq!(reject(app, "one", body).await, StatusCode::CONFLICT);
    }
}
