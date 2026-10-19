//! Router 组装与基础端点（health / variants）。

pub mod engine;
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
        // 无状态引擎端点（供连线程序调用）：BFEN 局面 → MCTS+ONNX 最优动作
        .route("/api/engine/move", post(engine::engine_move))
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
        nn_cache_entries: app.config.nn_cache_entries,
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

/// GET /api/settings：读取运行时可调的服务端设置（默认 MCTS 搜索预算 / NNUE 深度与预算）。
async fn get_settings(State(app): State<SharedState>) -> Json<SettingsResp> {
    Json(SettingsResp {
        mcts_sims: app.default_sims(),
        nnue_depth: app.default_nnue_depth(),
        nnue_budget: app.default_nnue_budget(),
    })
}

/// PUT /api/settings：运行时调整默认引擎参数（进行中的「跟随默认」对局实时同步；
/// 显式指定参数的对局不受影响）。缺省字段不修改。
async fn put_settings(
    State(app): State<SharedState>,
    Json(req): Json<SettingsReq>,
) -> Json<SettingsResp> {
    let mcts_sims = match req.mcts_sims {
        Some(v) => app.set_default_sims(v),
        None => app.default_sims(),
    };
    let nnue_depth = match req.nnue_depth {
        Some(v) => app.set_default_nnue_depth(v),
        None => app.default_nnue_depth(),
    };
    let nnue_budget = match req.nnue_budget {
        Some(v) => app.set_default_nnue_budget(v),
        None => app.default_nnue_budget(),
    };
    tracing::info!(
        "运行时调整默认引擎参数: mcts_sims={mcts_sims}, nnue_depth={nnue_depth}, nnue_budget={nnue_budget}"
    );
    Json(SettingsResp {
        mcts_sims,
        nnue_depth,
        nnue_budget,
    })
}
