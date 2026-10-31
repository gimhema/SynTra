use axum::{Router, response::Html, routing::get};

/// 승인 시 워크스페이스에 쓰기가 일어나므로 기본값은 로컬 접속만 허용한다.
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

const INDEX_HTML: &str = include_str!("../static/index.html");

fn app() -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/health", get(|| async { "ok" }))
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::var("SYNTRA_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("SynTra: http://{}", listener.local_addr()?);

    axum::serve(listener, app())
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
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_ok() {
        let request = Request::get("/api/health").body(Body::empty()).unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
