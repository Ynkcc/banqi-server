use crate::app::models::EngineKind;
use crate::app::player::{self, EngineParams, PlayerSlot, StrategyKind};
use crate::dto::{EngineMoveReq, EngineMoveResp};
use crate::error::ApiError;
use crate::state::{GameSession, SharedState};
use axum::Json;
use axum::extract::State;
use banqi_core::core::env::PositionSnapshot;
use std::sync::Arc;
pub async fn engine_move(
    State(app): State<SharedState>,
    Json(req): Json<EngineMoveReq>,
) -> Result<Json<EngineMoveResp>, ApiError> {
    let fen = req.fen.trim();
    if fen.is_empty() {
        return Err(ApiError::BadRequest("fen 不能为空".into()));
    }
    let snap = PositionSnapshot::from_fen(fen)
        .map_err(|e| ApiError::BadRequest(format!("BFEN 局面解析失败: {e}")))?;
    let variant = snap.variant;
    let current_player = crate::dto::player_str(snap.current_player);
    let session = GameSession::from_snapshot(
        variant,
        snap.clone(),
        crate::app::player::Slots::all_human(),
    );
    let (terminated, truncated, _) = session.over_state();
    if terminated || truncated {
        return Err(ApiError::GameOver);
    }
    let model_file = app.resolve_model_file(req.model_file.as_deref(), variant)?;
    let kind = StrategyKind::from_engine_kind(EngineKind::from_model_file(&model_file));
    let slot = match kind {
        StrategyKind::ExpectimaxNnue => {
            let budget = app.build_search_budget(
                req.nnue_budget_mode.as_deref(),
                req.nnue_budget,
                req.nnue_depth,
                req.nnue_time_ms,
                req.search_timeout_ms,
            )?;
            PlayerSlot::strategy(
                kind,
                Some(model_file),
                EngineParams {
                    nnue_budget: budget,
                    ..app.params()
                },
            )
        }
        _ => PlayerSlot::strategy(
            kind,
            Some(model_file),
            EngineParams {
                mcts_sims: req.mcts_sims.unwrap_or_else(|| app.default_sims()).max(1),
                ..app.params()
            },
        ),
    };
    let app2 = Arc::clone(&app);
    let decision =
        tokio::task::spawn_blocking(move || player::decide(&snap, &slot, None, &app2.models))
            .await
            .map_err(|e| ApiError::Internal(format!("blocking 任务 join 失败: {e}")))?
            .map_err(ApiError::Internal)?;
    let view = session.action_view(decision.action);
    let is_flip = decision.action < variant.config().reveal_actions_count;
    tracing::info!(
        variant = variant.as_str(),
        kind = kind.as_str(),
        "engine/move: action={}（from={} to={}，chance={}）",
        decision.action,
        view.from,
        view.to,
        view.is_chance
    );
    Ok(Json(EngineMoveResp {
        variant: variant.as_str().to_string(),
        current_player,
        action: decision.action,
        from: view.from,
        to: view.to,
        is_chance: view.is_chance,
        is_flip,
        mcts_stats: decision.mcts_stats,
        nnue_stats: decision.nnue_stats,
    }))
}
