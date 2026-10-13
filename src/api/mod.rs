//! Router 组装与基础端点（health / variants）。

pub mod games;
pub mod mcts;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};

use banqi_core::core::env::Variant;

use crate::dto::{HealthResp, ModelInfo, VariantInfo, VariantsResp};
use crate::state::SharedState;
use crate::web;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/api/games", post(games::create_game).get(games::list_games))
        .route("/api/games/{id}", get(games::get_game).delete(games::delete_game))
        .route("/api/games/{id}/move", post(games::move_piece))
        .route("/api/games/{id}/ai-move", post(games::ai_move))
        .route("/api/games/{id}/undo", post(games::undo))
        .route("/api/games/{id}/mcts/root", get(mcts::root))
        .route("/api/games/{id}/mcts/search", post(mcts::search))
        .route("/api/games/{id}/mcts/node/{node}", get(mcts::detail))
        .route("/api/games/{id}/mcts/node/{node}/children", get(mcts::children))
        .route("/api/games/{id}/mcts/node/{node}/board", get(mcts::board))
        .route("/api/health", get(health))
        .route("/api/variants", get(variants))
        .with_state(state)
        // 内嵌前端：/api 之外的所有路径走静态资源 + SPA fallback
        .fallback(web::handler)
}

/// GET /api/health：健康检查（含已加载模型及其匹配变体）。
async fn health(State(app): State<SharedState>) -> Json<HealthResp> {
    let models = app
        .models
        .loaded_files()
        .into_iter()
        .map(|(file, stem, variants)| ModelInfo { file, stem, variants })
        .collect();
    Json(HealthResp {
        ok: true,
        models,
        sessions: app.config.sessions,
        device: app.config.device.clone(),
    })
}

/// GET /api/variants：动态列出全部变体元信息及 AI 可用性（零硬编码）。
async fn variants(State(app): State<SharedState>) -> Json<VariantsResp> {
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
                    ai_available: app.models.available(v),
                }
            })
            .collect(),
    })
}
