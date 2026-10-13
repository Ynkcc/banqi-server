//! 内嵌 Web 前端服务：`banqi-web/public` 目录经 rust-embed 打包，
//! SPA fallback——未命中文件时返回 `index.html`（前端 history 路由）。
//!
//! debug 构建：rust-embed 直接读磁盘，改前端无需重编译；
//! release 构建：资源编译期嵌入，单二进制分发。

use axum::http::{header, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../banqi-web/public"]
struct WebAssets;

/// 静态资源处理 + SPA fallback。挂在 `/api` 之外的所有路径上。
pub async fn handler(uri: Uri) -> Response {
    // 去掉前导 '/'；拒绝路径穿越
    let path = uri.path().trim_start_matches('/');
    if path.split('/').any(|seg| seg == "..") {
        return StatusCode::BAD_REQUEST.into_response();
    }

    // 空路径或目录请求 → index.html；其余先按字面查找
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

    // SPA fallback：非资源路径回 index.html
    match WebAssets::get("index.html") {
        Some(index) => Html(index.data).into_response(),
        None => (StatusCode::NOT_FOUND, "前端资源缺失（banqi-web/public）").into_response(),
    }
}
