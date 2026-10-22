//! banqi-server — Banqi 暗棋对弈 HTTP 服务。
//!
//! - axum REST API：创建对局 / 走子 / 悔棋 / AI 对手 / 删除
//! - 推理：banqi-engine ONNX Runtime + Gumbel MCTS（默认 `onnx` feature，无 torch）
//! - 变体无关：变体分发复用 banqi-core `for_variant_env!`，模型按文件名（变体 ID）动态加载

/// gui / server 收敛出的共享应用层（原 banqi-app crate 并入，见 archive/banqi-app）
pub mod app;

mod variant;

mod ai;
mod api;
mod config;
mod dto;
mod error;
mod mcts;
mod nnue;
mod state;
mod web;

use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use state::AppState;

#[tokio::main]
async fn main() {
    // 日志：RUST_LOG 优先，默认 info
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = match config::ServerConfig::from_env_and_args() {
        Ok(c) => c,
        Err(e) => {
            // --help / 参数错误均经此输出
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let addr = config.addr.clone();
    let app_state = Arc::new(AppState::new(config));

    let files = app_state.models.files();
    tracing::info!(
        "模型目录扫描完成: {} 个模型文件（随用随加载：对局绑定的文件首次使用时才加载；会话池 {}，device {}）",
        files.len(),
        app_state.config.sessions,
        app_state.config.device
    );

    let router = api::router(app_state);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("绑定 {addr} 失败: {e}"));
    tracing::info!("banqi-server 监听 http://{addr}");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("HTTP 服务异常退出");
}

/// Ctrl-C / SIGTERM 优雅退出。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("注册 Ctrl-C 处理失败");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("注册 SIGTERM 处理失败")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("收到退出信号，正在关闭…");
}
