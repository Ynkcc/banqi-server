//! Router 组装与基础端点（health / variants）。

pub mod games;
pub mod mcts;

use axum::extract::State;
use axum::routing::{get, post, put};
use axum::{Json, Router};

use banqi_core::core::env::Variant;

use crate::dto::{HealthResp, ModelInfo, SettingsReq, SettingsResp, VariantInfo, VariantsResp};
use crate::state::SharedState;
use crate::web;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/api/games", post(games::create_game).get(games::list_games))
        .route("/api/games/{id}", get(games::get_game).delete(games::delete_game))
        .route("/api/games/{id}/move", post(games::move_piece))
        .route("/api/games/{id}/ai-move", post(games::ai_move))
        .route("/api/games/{id}/undo", post(games::undo))
        .route("/api/games/{id}/model", put(games::bind_model))
        .route("/api/games/{id}/mcts/root", get(mcts::root))
        .route("/api/games/{id}/mcts/search", post(mcts::search))
        .route("/api/games/{id}/mcts/node/{node}", get(mcts::detail))
        .route("/api/games/{id}/mcts/node/{node}/children", get(mcts::children))
        .route("/api/games/{id}/mcts/node/{node}/board", get(mcts::board))
        .route("/api/health", get(health))
        .route("/api/variants", get(variants))
        .route("/api/settings", get(get_settings).put(put_settings))
        .with_state(state)
        // 内嵌前端：/api 之外的所有路径走静态资源 + SPA fallback
        .fallback(web::handler)
}

/// GET /api/health：健康检查（含扫描到的模型文件与加载状态）。
async fn health(State(app): State<SharedState>) -> Json<HealthResp> {
    let models = app
        .models
        .loaded_files()
        .into_iter()
        .map(|(file, loaded)| ModelInfo { file, loaded })
        .collect();
    Json(HealthResp {
        ok: true,
        models,
        sessions: app.config.sessions,
        device: app.config.device.clone(),
    })
}

/// GET /api/variants：动态列出全部变体元信息（零硬编码）。
async fn variants() -> Json<VariantsResp> {
    Json(VariantsResp {
        variants: Variant::ALL
            .iter()
            .copied()
            .map(|v| {
                let cfg = v.config();
                VariantInfo {
                    id: v.as_str().to_string(),
                    label: v.label().to_string(),
                    rows: cfg.rows,
                    cols: cfg.cols,
                    initial_health: cfg.initial_health,
                    max_consecutive_moves_for_draw: cfg.max_consecutive_moves_for_draw,
                }
            })
            .collect(),
    })
}

/// GET /api/settings：读取运行时可调的服务端设置（当前仅默认 MCTS 搜索预算）。
async fn get_settings(State(app): State<SharedState>) -> Json<SettingsResp> {
    Json(SettingsResp {
        mcts_sims: app.default_sims(),
    })
}

/// PUT /api/settings：运行时调整默认 MCTS 搜索预算（进行中的「跟随默认」
/// 对局实时同步；显式指定 `mctsSims` 的对局不受影响）。
async fn put_settings(
    State(app): State<SharedState>,
    Json(req): Json<SettingsReq>,
) -> Json<SettingsResp> {
    let applied = app.set_default_sims(req.mcts_sims);
    tracing::info!("运行时调整默认 MCTS 搜索预算: {applied}");
    Json(SettingsResp { mcts_sims: applied })
}
