#[path = "../../../banqi-core/build_hook.rs"]
mod comment_hook;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FRONTEND_SRC: &str = "../../../banqi-frontend/src";
const FRONTEND_DIST_INDEX: &str = "../../../banqi-frontend/dist/index.html";
/// OUT_DIR 下的状态文件：记录最近一次通过校验时的前端源内容哈希。
const HASH_STATE_FILE: &str = "frontend-src-content-hash";

/// 前端产物新鲜度校验（内容哈希版）。
///
/// `web.rs` 用 rust-embed 在**编译期**把 `banqi-frontend/dist` 打进二进制；
/// 若 dist 是旧产物，服务端会静默嵌入过期页面。此处在编译期拦截。
///
/// 校验分两层：
/// 1. **内容哈希**（忽略 mtime）：对 `frontend/src` 全部文件按相对路径排序后
///    计算 FNV-1a 64 稳定哈希，与 OUT_DIR 状态文件中上次通过校验的哈希比较。
///    一致（源内容零变化，即使 mtime 被 touch 或 ts-rs 重写刷新）→ 直接通过。
///    这根治了「前端 build → server 测试重写 generated → dist 过期」的循环。
/// 2. **mtime 回退**：源内容有实质变化（或首次构建/状态丢失）时，回退为
///    「dist/index.html 必须晚于 src 下任一源文件」的判断——generated 里的
///    TS 类型声明编译进 dist 后会被擦除，无法从 dist 内容反推源状态，
///    因此内容等价性只能靠 dist 构建晚于源变更来近似。
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

    let hash = content_hash(&src_dir);
    let state_path = std::env::var("OUT_DIR")
        .ok()
        .map(|d| Path::new(&d).join(HASH_STATE_FILE));
    let stored = state_path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if stored.as_deref() == Some(hash.as_str()) {
        // 源内容自上次成功校验后零变化：无论 mtime 如何被刷新（touch、
        // ts-rs 重写 generated 等），嵌入的 dist 仍是最新内容的产物。
        println!("cargo:rerun-if-changed={}", src_dir.display());
        println!("cargo:rerun-if-changed={}", dist_index.parent().unwrap().display());
        return;
    }

    let dist_mtime = file_mtime(&dist_index).unwrap_or_else(|| {
        panic!("无法读取 dist/index.html 的 mtime：{}", dist_index.display());
    });

    if let Some(stale_src) = newest_source_newer_than(&src_dir, dist_mtime) {
        panic!(
            "前端产物过期：dist/index.html 早于源文件 {}（源内容有实质变化，rust-embed 会静默嵌入旧页面）。\n\
             请先构建前端：  cd banqi-frontend && npm run build\n\
             （仅检查 Rust 语法时可用 BQ_SKIP_FRONTEND_CHECK=1 跳过）",
            stale_src.display()
        );
    }

    // 校验通过：记录当前源内容哈希，供后续忽略 mtime 快速通道使用。
    if let Some(p) = &state_path {
        let _ = std::fs::write(p, &hash);
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

// ---------------------------------------------------------------------------
// 内容哈希：FNV-1a 64（手写实现，避免新增构建期重依赖）。
// 输入 = src 下全部文件，按相对路径排序；每个文件依次混入
// 「相对路径 + 0xFF 分隔 + 内容长度(LE) + 文件内容」，保证跨平台稳定。
// ---------------------------------------------------------------------------

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv_mix(acc: u64, bytes: &[u8]) -> u64 {
    let mut h = acc;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    // 段分隔符：避免「路径 A + 内容 B」与「路径 B + 内容 A」等拼接歧义
    h ^= 0xff;
    h.wrapping_mul(FNV_PRIME)
}

fn collect_files(root: &Path, dir: &Path, out: &mut BTreeMap<String, PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.metadata() {
            Ok(m) if m.is_dir() => collect_files(root, &path, out),
            Ok(m) if m.is_file() => {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, path);
            }
            _ => {}
        }
    }
}

fn content_hash(dir: &Path) -> String {
    let mut files = BTreeMap::new();
    collect_files(dir, dir, &mut files);
    let mut acc = FNV_OFFSET;
    for (rel, path) in &files {
        acc = fnv_mix(acc, rel.as_bytes());
        let data = std::fs::read(path).unwrap_or_default();
        acc = fnv_mix(acc, &(data.len() as u64).to_le_bytes());
        acc = fnv_mix(acc, &data);
    }
    format!("{acc:016x}")
}

fn main() {
    check_frontend_freshness();
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../../../banqi-core/build_hook.rs");
    comment_hook::clean(&std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
}
