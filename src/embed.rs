use axum::http::{StatusCode, Uri};
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
    if path == "sw.js" {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        StatusCode::NOT_FOUND,
        "Frontend assets are not served by the Rust server in debug builds. Use http://127.0.0.1:5173 for local UI development.",
    )
        .into_response()
}
