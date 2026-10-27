pub mod agents;
pub mod engine;
pub mod games;
pub mod mcts;
pub mod nnue;
use crate::dto::{
    CapabilitiesResp, HealthResp, ModelEntryDto, ModelInfo, ModelListResp, SettingsReq,
    SettingsResp, VariantInfo, VariantsResp,
};
use crate::error::ApiError;
use crate::state::SharedState;
use crate::web;
use axum::extract::State;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use banqi_core::core::env::Variant;
pub fn router(state: SharedState) -> Router {
    Router::new()
        .route(
            "/api/games",
            post(games::create_game).get(games::list_games),
        )
        .route(
            "/api/agents",
            post(agents::create_agent).get(agents::list_agents),
        )
        .route(
            "/api/agents/{id}",
            get(agents::get_agent).delete(agents::delete_agent),
        )
        .route(
            "/api/agents/{id}/settings",
            put(agents::update_agent_settings),
        )
        .route(
            "/api/games/{id}",
            get(games::get_game).delete(games::delete_game),
        )
        .route("/api/games/recent", get(games::list_recent_games))
        .route("/api/games/recent/{id}", get(games::get_recent_game))
        .route(
            "/api/games/recent/{id}/replay",
            get(games::replay_recent_game).post(games::replay_as_new_game),
        )
        .route("/api/games/{id}/move", post(games::move_piece))
        .route("/api/games/{id}/run", post(games::run))
        .route("/api/games/{id}/dry-run", post(games::dry_run))
        .route("/api/games/{id}/undo", post(games::undo))
        .route("/api/games/{id}/slots", put(games::put_slots))
        .route("/api/games/{id}/move-action", get(games::move_action))
        .route("/api/games/{id}/mcts/root", get(mcts::root))
        .route("/api/games/{id}/mcts/search", post(mcts::search))
        .route("/api/games/{id}/mcts/node/{node}", get(mcts::detail))
        .route(
            "/api/games/{id}/mcts/node/{node}/children",
            get(mcts::children),
        )
        .route("/api/games/{id}/mcts/node/{node}/board", get(mcts::board))
        .route("/api/games/{id}/nnue/root", get(nnue::root))
        .route("/api/games/{id}/nnue/search", post(nnue::search))
        .route("/api/games/{id}/nnue/node/{node}", get(nnue::detail))
        .route(
            "/api/games/{id}/nnue/node/{node}/children",
            get(nnue::children),
        )
        .route("/api/games/{id}/nnue/node/{node}/board", get(nnue::board))
        .route("/api/health", get(health))
        .route("/api/variants", get(variants))
        .route("/api/scenarios", get(scenarios))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/capabilities", get(capabilities))
        .route("/api/models", get(list_models))
        .route("/api/engine/move", post(engine::engine_move))
        .with_state(state)
        .fallback(web::handler)
}
pub fn parse_variant_or_err(s: &str) -> Result<Variant, ApiError> {
    crate::config::parse_variant(s).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "未知变体: {s}（可用: {}）",
            Variant::ALL
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })
}
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
async fn capabilities() -> Json<CapabilitiesResp> {
    Json(CapabilitiesResp {
        torch: cfg!(feature = "torch"),
        onnx: cfg!(feature = "onnx"),
        nnue: true,
    })
}
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
async fn scenarios() -> Json<crate::dto::ScenariosResp> {
    Json(crate::dto::ScenariosResp {
        variants: Variant::ALL
            .iter()
            .copied()
            .map(|v| crate::dto::VariantScenarios {
                variant: v.as_str().to_string(),
                scenarios: banqi_core::core::env::supported_scenario_infos(v),
            })
            .collect(),
    })
}
async fn get_settings(State(app): State<SharedState>) -> Json<SettingsResp> {
    Json(app.params().into())
}
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
