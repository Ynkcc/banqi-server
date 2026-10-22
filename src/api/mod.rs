//! Router 组装与基础端点（health / variants）。

pub mod engine;
pub mod games;
pub mod mcts;
pub mod nnue;

use axum::extract::State;
use axum::routing::{get, post, put};
use axum::{Json, Router};

use banqi_core::core::env::Variant;

use crate::dto::{
    CapabilitiesResp, HealthResp, ModelEntryDto, ModelInfo, ModelListResp, SettingsReq,
    SettingsResp, VariantInfo, VariantsResp,
};
use crate::error::ApiError;
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
        .route("/api/games/{id}/opponent", get(games::get_opponent))
        .route("/api/games/{id}/move-action", get(games::move_action))
        .route("/api/games/{id}/mcts/root", get(mcts::root))
        .route("/api/games/{id}/mcts/search", post(mcts::search))
        .route("/api/games/{id}/mcts/node/{node}", get(mcts::detail))
        .route("/api/games/{id}/mcts/node/{node}/children", get(mcts::children))
        .route("/api/games/{id}/mcts/node/{node}/board", get(mcts::board))
        .route("/api/games/{id}/nnue/root", get(nnue::root))
        .route("/api/games/{id}/nnue/search", post(nnue::search))
        .route("/api/games/{id}/nnue/node/{node}", get(nnue::detail))
        .route("/api/games/{id}/nnue/node/{node}/children", get(nnue::children))
        .route("/api/games/{id}/nnue/node/{node}/board", get(nnue::board))
        .route("/api/health", get(health))
        .route("/api/variants", get(variants))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/capabilities", get(capabilities))
        .route("/api/models", get(list_models))
        // 无状态引擎端点（供连线程序调用）：BFEN 局面 → MCTS+ONNX 最优动作
        .route("/api/engine/move", post(engine::engine_move))
        .with_state(state)
        // 内嵌前端：/api 之外的所有路径走静态资源 + SPA fallback
        .fallback(web::handler)
}

/// 解析请求中的变体字符串（非法时 400，错误消息列出全部可用变体；
/// BFEN 一致性校验与 variant 字段解析共用）。
pub fn parse_variant_or_err(s: &str) -> Result<Variant, ApiError> {
    crate::config::parse_variant(s).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "未知变体: {s}（可用: {}）",
            Variant::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
        ))
    })
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

/// GET /api/capabilities：编译期启用的推理后端（前端据此隐藏不可用的
/// 对手与模型类型；对应 gui `get_capabilities`）。
async fn capabilities() -> Json<CapabilitiesResp> {
    Json(CapabilitiesResp {
        torch: cfg!(feature = "torch"),
        onnx: cfg!(feature = "onnx"),
        nnue: true,
    })
}

/// GET /api/models：模型目录与扫描到的模型文件（含子目录，每次重扫保证最新；
/// 对应 gui `list_models`，`dir` 供前端展示）。
async fn list_models(State(app): State<SharedState>) -> Json<ModelListResp> {
    let models = app
        .models
        .files()
        .into_iter()
        .map(|path| {
            let name = std::path::Path::new(&path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            ModelEntryDto { name, path }
        })
        .collect();
    Json(ModelListResp {
        dir: app.config.models_dir.to_string_lossy().to_string(),
        models,
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

/// GET /api/settings：读取运行时可调的服务端设置（默认 MCTS 搜索预算 /
/// 强引擎节点预算 / NNUE 搜索预算：模式 + 节点/深度/时间 + 硬超时）。
async fn get_settings(State(app): State<SharedState>) -> Json<SettingsResp> {
    Json(app.params().into())
}

/// PUT /api/settings：运行时调整默认引擎参数（进行中的「跟随默认」对局实时同步；
/// 显式指定参数的对局不受影响）。缺省字段不修改；只覆盖显式给出的字段。
async fn put_settings(
    State(app): State<SharedState>,
    Json(req): Json<SettingsReq>,
) -> Result<Json<SettingsResp>, ApiError> {
    let nnue_budget = app.build_search_budget(
        req.nnue_budget_mode.as_deref(),
        req.nnue_budget,
        req.nnue_depth,
        req.nnue_time_ms,
        req.search_timeout_ms,
    )?;
    let p = app.update_params(req.mcts_sims, req.engine_budget, Some(nnue_budget));
    tracing::info!(
        "运行时调整默认引擎参数: mcts_sims={}, engine_budget={}, nnue_mode={}, nnue_depth={}, nnue_budget={}, nnue_time_ms={}, search_timeout_ms={}",
        p.mcts_sims,
        p.engine_budget,
        p.nnue_budget.mode.as_str(),
        p.nnue_budget.depth,
        p.nnue_budget.nodes,
        p.nnue_budget.time_ms,
        p.nnue_budget.timeout_ms
    );
    Ok(Json(p.into()))
}
