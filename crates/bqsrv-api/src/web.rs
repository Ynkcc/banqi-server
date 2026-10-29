use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::RustEmbed;
#[derive(RustEmbed)]
#[folder = "../../../banqi-frontend/dist"]
struct WebAssets;
pub async fn handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.split('/').any(|seg| seg == "..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let candidates: &[&str] = if path.is_empty() {
        &["index.html"]
    } else {
        &[path]
    };
    for candidate in candidates {
        if let Some(file) = WebAssets::get(candidate) {
            let mime = mime_guess::from_path(candidate).first_or_octet_stream();
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref())],
                file.data,
            )
                .into_response();
        }
    }
    match WebAssets::get("index.html") {
        Some(index) => Html(index.data).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            "前端资源缺失（banqi-frontend/dist，先 npm run build）",
        )
            .into_response(),
    }
}
