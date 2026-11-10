mod cli;
mod workspace;

use std::path::{Path as FsPath, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, RwLock};

use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use syntra_core::{Edited, Listing, Proposal, ReviewError, Store};
use workspace::{Folder, Recent};

/// 승인 시 워크스페이스에 쓰기가 일어나므로 로컬 접속만 허용한다.
const PREFERRED_ADDR: &str = "127.0.0.1:7878";
const FALLBACK_ADDR: &str = "127.0.0.1:0";

const INDEX_HTML: &str = include_str!("../static/index.html");

struct AppState {
    current: RwLock<Option<Arc<Store>>>,
    current_path: RwLock<Option<String>>,
    recent: Recent,
}

type SharedState = Arc<AppState>;

#[derive(Deserialize)]
struct RejectRequest {
    reason: String,
}

#[derive(Deserialize)]
struct ContentRequest {
    content: String,
}

#[derive(Deserialize)]
struct AfterRequest {
    after: String,
}

#[derive(Deserialize)]
struct OpenRequest {
    path: String,
}

#[derive(Deserialize)]
struct BrowseQuery {
    path: Option<String>,
}

#[derive(Serialize)]
struct WorkspaceInfo {
    current: Option<String>,
    recent: Vec<String>,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

struct ApiError(StatusCode, String);

impl ApiError {
    fn internal(error: impl ToString) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

impl From<ReviewError> for ApiError {
    fn from(error: ReviewError) -> Self {
        let status = match error {
            ReviewError::BlankReason | ReviewError::UnsafePath(_) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            ReviewError::NotFound(_) => StatusCode::NOT_FOUND,
            ReviewError::AlreadyReviewed(_) | ReviewError::Apply(_) => StatusCode::CONFLICT,
            ReviewError::Io(_) | ReviewError::Json(_) | ReviewError::Config(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        Self(status, error.to_string())
    }
}

fn current_store(state: &AppState) -> Result<Arc<Store>, ApiError> {
    state
        .current
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| {
            ApiError(
                StatusCode::CONFLICT,
                "워크스페이스를 먼저 선택해야 합니다".to_string(),
            )
        })
}

fn set_current(state: &AppState, path: Option<String>) {
    let store = path.as_ref().map(|path| Arc::new(Store::new(path)));
    *state.current.write().unwrap_or_else(|e| e.into_inner()) = store;
    *state
        .current_path
        .write()
        .unwrap_or_else(|e| e.into_inner()) = path;
}

fn workspace_info(state: &AppState) -> WorkspaceInfo {
    WorkspaceInfo {
        current: state
            .current_path
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
        recent: state.recent.list(),
    }
}

async fn get_workspace(State(state): State<SharedState>) -> Json<WorkspaceInfo> {
    Json(workspace_info(&state))
}

async fn open_workspace(
    State(state): State<SharedState>,
    Json(request): Json<OpenRequest>,
) -> Result<Json<WorkspaceInfo>, ApiError> {
    let path = std::path::absolute(request.path.trim()).map_err(|_| not_a_folder(&request.path))?;
    if !path.is_dir() {
        return Err(not_a_folder(&request.path));
    }
    let path = Store::workspace_root(&path).display().to_string();
    state.recent.add(&path);
    set_current(&state, Some(path));
    Ok(Json(workspace_info(&state)))
}

fn not_a_folder(path: &str) -> ApiError {
    ApiError(
        StatusCode::UNPROCESSABLE_ENTITY,
        format!("폴더를 찾을 수 없습니다: {path}"),
    )
}

async fn close_workspace(State(state): State<SharedState>) -> Json<WorkspaceInfo> {
    set_current(&state, None);
    Json(workspace_info(&state))
}

async fn browse(Query(query): Query<BrowseQuery>) -> Result<Json<Folder>, ApiError> {
    let path = query
        .path
        .map(PathBuf::from)
        .or_else(workspace::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    workspace::browse(&path)
        .map(Json)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}

async fn list_proposals(State(state): State<SharedState>) -> Result<Json<Listing>, ApiError> {
    current_store(&state)?
        .list()
        .map(Json)
        .map_err(ApiError::internal)
}

async fn approve_proposal(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Proposal>, ApiError> {
    Ok(Json(current_store(&state)?.approve(&id)?))
}

async fn reject_proposal(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(request): Json<RejectRequest>,
) -> Result<Json<Proposal>, ApiError> {
    Ok(Json(current_store(&state)?.reject(&id, &request.reason)?))
}

async fn approve_region(
    State(state): State<SharedState>,
    Path((id, index)): Path<(String, usize)>,
) -> Result<Json<Proposal>, ApiError> {
    Ok(Json(current_store(&state)?.approve_region(&id, index)?))
}

async fn reject_region(
    State(state): State<SharedState>,
    Path((id, index)): Path<(String, usize)>,
    Json(request): Json<RejectRequest>,
) -> Result<Json<Proposal>, ApiError> {
    Ok(Json(current_store(&state)?.reject_region(
        &id,
        index,
        &request.reason,
    )?))
}

async fn edit_content(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(request): Json<ContentRequest>,
) -> Result<Json<Edited>, ApiError> {
    Ok(Json(
        current_store(&state)?.edit_content(&id, &request.content)?,
    ))
}

async fn edit_region(
    State(state): State<SharedState>,
    Path((id, index)): Path<(String, usize)>,
    Json(request): Json<AfterRequest>,
) -> Result<Json<Edited>, ApiError> {
    Ok(Json(current_store(&state)?.edit_region(
        &id,
        index,
        &request.after,
    )?))
}

/// 브라우저에서 열린 다른 사이트가 이 서버로 승인 요청을 보내지 못하게 막는다.
fn is_local_request(headers: &HeaderMap) -> bool {
    let header_str = |name| headers.get(name).and_then(|value| value.to_str().ok());
    let Some(host) = header_str(header::HOST) else {
        return false;
    };
    let hostname = host.rsplit_once(':').map_or(host, |(name, _)| name);
    if hostname != "127.0.0.1" && hostname != "localhost" {
        return false;
    }
    match header_str(header::ORIGIN) {
        Some(origin) => origin.strip_prefix("http://") == Some(host),
        None => true,
    }
}

async fn local_only(request: Request, next: Next) -> Response {
    if is_local_request(request.headers()) {
        next.run(request).await
    } else {
        ApiError(
            StatusCode::FORBIDDEN,
            "허용되지 않은 접속입니다".to_string(),
        )
        .into_response()
    }
}

fn app(state: SharedState) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/health", get(|| async { "ok" }))
        .route(
            "/api/workspace",
            get(get_workspace)
                .post(open_workspace)
                .delete(close_workspace),
        )
        .route("/api/browse", get(browse))
        .route("/api/proposals", get(list_proposals))
        .route("/api/proposals/{id}/approve", post(approve_proposal))
        .route("/api/proposals/{id}/reject", post(reject_proposal))
        .route("/api/proposals/{id}/content", post(edit_content))
        .route(
            "/api/proposals/{id}/regions/{index}/approve",
            post(approve_region),
        )
        .route(
            "/api/proposals/{id}/regions/{index}/reject",
            post(reject_region),
        )
        .route(
            "/api/proposals/{id}/regions/{index}/after",
            post(edit_region),
        )
        .layer(middleware::from_fn(local_only))
        .with_state(state)
}

fn new_state(recent_file: Option<PathBuf>) -> SharedState {
    Arc::new(AppState {
        current: RwLock::new(None),
        current_path: RwLock::new(None),
        recent: Recent::new(recent_file),
    })
}

fn open_browser(url: String) {
    std::thread::spawn(move || {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/C", "start", "", &url]);
            command
        } else {
            let mut command = Command::new("xdg-open");
            command.arg(&url);
            command
        };
        let _ = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => cli::check(&args[1..]),
        Some("cat") => cli::cat(&args[1..]),
        Some("-h" | "--help" | "help") => {
            println!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        _ => {
            let open = !args.iter().any(|arg| arg == "--no-open");
            let result = tokio::runtime::Runtime::new().and_then(|rt| rt.block_on(serve(open)));
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

async fn serve(open: bool) -> std::io::Result<()> {
    // 7878 포트를 다른 프로그램이 쓰고 있으면 빈 포트로 실행한다.
    let listener = match tokio::net::TcpListener::bind(PREFERRED_ADDR).await {
        Ok(listener) => listener,
        Err(_) => tokio::net::TcpListener::bind(FALLBACK_ADDR).await?,
    };
    let url = format!("http://{}", listener.local_addr()?);
    println!("SynTra: {url}");
    if open {
        open_browser(url);
    }

    let recent_file =
        workspace::home_dir().map(|home| home.join(FsPath::new(".syntra/recent.json")));
    axum::serve(listener, app(new_state(recent_file)))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    const HOST: &str = "127.0.0.1:7878";

    fn test_app() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        let proposals = dir.path().join("work/.syntra/proposals");
        std::fs::create_dir_all(&proposals).unwrap();
        std::fs::write(
            proposals.join("one.json"),
            r#"{"kind":"create","file_name":"a.rs","content":"x","purpose":"p","location":""}"#,
        )
        .unwrap();
        std::fs::write(
            proposals.join("two.json"),
            r#"{"kind":"create","file_name":"b.rs","content":"x","purpose":"p","location":""}"#,
        )
        .unwrap();
        let app = app(new_state(Some(dir.path().join("recent.json"))));
        (dir, app)
    }

    async fn post_json(app: &Router, uri: &str, body: &str) -> StatusCode {
        let request = Request::post(uri)
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        app.clone().oneshot(request).await.unwrap().status()
    }

    async fn open(app: &Router, dir: &tempfile::TempDir) {
        let body = serde_json::json!({ "path": dir.path().join("work") }).to_string();
        assert_eq!(
            post_json(app, "/api/workspace", &body).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (_dir, app) = test_app();
        let request = Request::get("/api/health")
            .header(header::HOST, HOST)
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn foreign_host_and_origin_are_forbidden() {
        let (dir, app) = test_app();
        open(&app, &dir).await;
        for (host, origin) in [
            ("evil.example:7878", None),
            (HOST, Some("https://evil.example")),
            (HOST, Some("http://localhost:3000")),
        ] {
            let mut request =
                Request::post("/api/proposals/one/approve").header(header::HOST, host);
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        assert!(!dir.path().join("work/a.rs").exists());
    }

    #[tokio::test]
    async fn proposals_require_a_workspace() {
        let (_dir, app) = test_app();
        assert_eq!(
            post_json(&app, "/api/proposals/one/approve", "").await,
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn opening_a_missing_folder_fails() {
        let (dir, app) = test_app();
        let body = serde_json::json!({ "path": dir.path().join("none") }).to_string();
        assert_eq!(
            post_json(&app, "/api/workspace", &body).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn opened_workspace_is_remembered() {
        let (dir, app) = test_app();
        open(&app, &dir).await;
        let recent = Recent::new(Some(dir.path().join("recent.json"))).list();
        assert_eq!(recent, [dir.path().join("work").display().to_string()]);
    }

    #[tokio::test]
    async fn opening_a_proposals_folder_opens_its_workspace() {
        let (dir, app) = test_app();
        let plain = dir.path().join("plain/proposals");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(
            plain.join("one.json"),
            r#"{"kind":"create","file_name":"a.rs","content":"x","purpose":"p","location":""}"#,
        )
        .unwrap();

        let body = serde_json::json!({ "path": plain }).to_string();
        assert_eq!(
            post_json(&app, "/api/workspace", &body).await,
            StatusCode::OK
        );
        assert_eq!(
            post_json(&app, "/api/proposals/one/approve", "").await,
            StatusCode::OK
        );
        assert!(dir.path().join("plain/a.rs").exists());
    }

    #[tokio::test]
    async fn region_routes_review_one_region() {
        let (dir, app) = test_app();
        let work = dir.path().join("work");
        std::fs::write(work.join("c.rs"), "one\ntwo\n").unwrap();
        std::fs::write(
            work.join(".syntra/proposals/edit.json"),
            r#"{"kind":"edit","file_name":"c.rs","regions":[
                {"before":"one","after":"ONE","purpose":"a"},
                {"before":"two","after":"TWO","purpose":"b"}]}"#,
        )
        .unwrap();
        open(&app, &dir).await;

        let base = "/api/proposals/edit/regions";
        assert_eq!(
            post_json(&app, &format!("{base}/1/approve"), "").await,
            StatusCode::OK
        );
        assert_eq!(
            std::fs::read_to_string(work.join("c.rs")).unwrap(),
            "one\nTWO\n"
        );
        assert_eq!(
            post_json(&app, &format!("{base}/1/approve"), "").await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            post_json(&app, &format!("{base}/5/approve"), "").await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            post_json(&app, &format!("{base}/0/reject"), r#"{"reason":" "}"#).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            post_json(&app, &format!("{base}/0/reject"), r#"{"reason":"사유"}"#).await,
            StatusCode::OK
        );
        assert_eq!(
            std::fs::read_to_string(work.join("c.rs")).unwrap(),
            "one\nTWO\n"
        );
    }

    #[tokio::test]
    async fn edit_routes_save_without_applying() {
        let (dir, app) = test_app();
        let work = dir.path().join("work");
        std::fs::write(work.join("c.rs"), "one\n").unwrap();
        std::fs::write(
            work.join(".syntra/proposals/edit.json"),
            r#"{"kind":"edit","file_name":"c.rs","regions":[{"before":"one","after":"ONE","purpose":"a"}]}"#,
        )
        .unwrap();
        open(&app, &dir).await;

        let after = "/api/proposals/edit/regions/0/after";
        assert_eq!(
            post_json(&app, after, r#"{"after":"Uno"}"#).await,
            StatusCode::OK
        );
        let content = "/api/proposals/one/content";
        assert_eq!(
            post_json(&app, content, r#"{"content":"y"}"#).await,
            StatusCode::OK
        );
        assert_eq!(
            post_json(&app, content, "{}").await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            post_json(&app, "/api/proposals/edit/content", r#"{"content":"y"}"#).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(std::fs::read_to_string(work.join("c.rs")).unwrap(), "one\n");
        assert!(!work.join("a.rs").exists());

        for id in ["edit", "one"] {
            assert_eq!(
                post_json(&app, &format!("/api/proposals/{id}/approve"), "").await,
                StatusCode::OK
            );
        }
        assert_eq!(std::fs::read_to_string(work.join("c.rs")).unwrap(), "Uno\n");
        assert_eq!(std::fs::read_to_string(work.join("a.rs")).unwrap(), "y");
    }

    #[tokio::test]
    async fn approve_and_reject_status_codes() {
        let (dir, app) = test_app();
        open(&app, &dir).await;
        let reason = r#"{"reason":"사유"}"#;
        assert_eq!(
            post_json(&app, "/api/proposals/two/reject", r#"{"reason":"  "}"#).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            post_json(&app, "/api/proposals/two/reject", "{}").await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            post_json(&app, "/api/proposals/none/reject", reason).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            post_json(&app, "/api/proposals/two/reject", reason).await,
            StatusCode::OK
        );
        assert_eq!(
            post_json(&app, "/api/proposals/two/reject", reason).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            post_json(&app, "/api/proposals/one/approve", "").await,
            StatusCode::OK
        );
        assert!(dir.path().join("work/a.rs").exists());
        assert!(!dir.path().join("work/b.rs").exists());
    }
}
