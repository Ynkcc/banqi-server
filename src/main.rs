use bqsrv_app::state::AppState;
use bqsrv_config::ServerConfig;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = match ServerConfig::from_env_and_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let addr = config.addr.clone();
    let app_state = Arc::new(AppState::new(config));
    app_state.load_archives();
    let files = app_state.models.files();
    tracing::info!(
        "模型目录扫描完成: {} 个模型文件（随用随加载：对局绑定的文件首次使用时才加载；会话池 {}，device {}）",
        files.len(),
        app_state.config.sessions,
        app_state.config.device
    );
    let router = bqsrv_api::router(app_state);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("绑定 {addr} 失败: {e}"));
    tracing::info!("banqi-server 监听 http://{addr}");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("HTTP 服务异常退出");
}
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("注册 Ctrl-C 处理失败");
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
