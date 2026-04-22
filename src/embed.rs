use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

#[cfg(not(debug_assertions))]
#[derive(rust_embed::Embed)]
#[folder = "app/dist/"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    serve_asset(path).await
}

#[cfg(not(debug_assertions))]
async fn serve_asset(path: &str) -> Response {
    if let Some(file) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return (
            [(header::CONTENT_TYPE, mime.as_ref())],
            file.data.into_owned(),
        )
            .into_response();
    }
    // SPA fallback
    if let Some(index) = Assets::get("index.html") {
        return (
            [(header::CONTENT_TYPE, "text/html")],
            index.data.into_owned(),
        )
            .into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

#[cfg(debug_assertions)]
async fn serve_asset(path: &str) -> Response {
    use std::path::PathBuf;

    let base = PathBuf::from("app/dist");
    let Ok(base_real) = tokio::fs::canonicalize(&base).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let candidate = base.join(path);
    if let Ok(candidate_real) = tokio::fs::canonicalize(&candidate).await {
        if candidate_real.starts_with(&base_real) {
            if let Ok(bytes) = tokio::fs::read(&candidate_real).await {
                let mime = mime_guess::from_path(&candidate_real).first_or_octet_stream();
                return ([(header::CONTENT_TYPE, mime.as_ref())], bytes).into_response();
            }
        }
    }
    if let Ok(bytes) = tokio::fs::read(base_real.join("index.html")).await {
        return ([(header::CONTENT_TYPE, "text/html")], bytes).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
