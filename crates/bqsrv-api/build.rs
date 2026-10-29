#[path = "../../../banqi-core/build_hook.rs"]
mod comment_hook;

use std::path::{Path, PathBuf};

const FRONTEND_SRC: &str = "../../../banqi-frontend/src";
const FRONTEND_DIST_INDEX: &str = "../../../banqi-frontend/dist/index.html";

/// 前端产物新鲜度校验。
///
/// `web.rs` 用 rust-embed 在**编译期**把 `banqi-frontend/dist` 打进二进制；
/// 若 dist 是旧产物（早于 `src/**` 的最近改动），服务端会静默嵌入过期页面。
/// 此处在编译期拦截：dist/index.html 不存在、或早于 src 下任一源文件时，
/// 编译直接报错，提示先执行前端构建。
///
/// 逃生口：设置 `BQ_SKIP_FRONTEND_CHECK=1` 可跳过（如 CI 只做 Rust 语法检查）。
fn check_frontend_freshness() {
    if std::env::var("BQ_SKIP_FRONTEND_CHECK").is_ok() {
        println!("cargo:warning=bqsrv-api: BQ_SKIP_FRONTEND_CHECK=1，跳过前端产物新鲜度校验");
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let src_dir = manifest.join(FRONTEND_SRC);
    let dist_index = manifest.join(FRONTEND_DIST_INDEX);

    if !dist_index.is_file() {
        panic!(
            "banqi-frontend/dist/index.html 不存在：rust-embed 将嵌入空的前端产物。\n\
             请先构建前端：  cd banqi-frontend && npm run build\n\
             （仅检查 Rust 语法时可用 BQ_SKIP_FRONTEND_CHECK=1 跳过）"
        );
    }

    let dist_mtime = file_mtime(&dist_index).unwrap_or_else(|| {
        panic!("无法读取 dist/index.html 的 mtime：{}", dist_index.display());
    });

    if let Some(stale_src) = newest_source_newer_than(&src_dir, dist_mtime) {
        panic!(
            "前端产物过期：dist/index.html 早于源文件 {}（rust-embed 会静默嵌入旧页面）。\n\
             请先构建前端：  cd banqi-frontend && npm run build\n\
             （仅检查 Rust 语法时可用 BQ_SKIP_FRONTEND_CHECK=1 跳过）",
            stale_src.display()
        );
    }

    // src 或 dist 有变动时重跑本 build script，保证校验与 release 期嵌入及时刷新
    println!("cargo:rerun-if-changed={}", src_dir.display());
    println!("cargo:rerun-if-changed={}", dist_index.parent().unwrap().display());
}

fn file_mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// 递归找出 src 下 mtime 晚于 `cutoff` 的任一文件（找到即返回）。
fn newest_source_newer_than(dir: &Path, cutoff: std::time::SystemTime) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.modified().ok().is_some_and(|t| t > cutoff) {
                return Some(path);
            }
        }
    }
    None
}

fn main() {
    check_frontend_freshness();
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../../../banqi-core/build_hook.rs");
    comment_hook::clean(&std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
}
