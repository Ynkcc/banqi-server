// 编译钩子：每次 cargo build/check 前自动清理 src 下 Rust 源码的注释并 fmt。
// 钩子逻辑复用 banqi-core 包内的 build_hook.rs（零依赖，仅 std）。
// 注意：本文件路径耦合 banqi-core 的目录名，若其改名/移动需同步更新此处。
// 幂等：已清理的文件不重写（mtime 不变），至多触发一轮重编译后收敛，不会死循环。
#[path = "../banqi-core/build_hook.rs"]
mod comment_hook;

fn main() {
    // 一旦输出 rerun-if-changed，cargo 默认的"整包监听"即失效，
    // 因此必须显式声明：源码或钩子本体任一变化都要重跑本脚本。
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../banqi-core/build_hook.rs");
    comment_hook::clean(&std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
}
