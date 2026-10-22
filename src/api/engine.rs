//! 无状态引擎端点：BFEN 局面 → 引擎最优动作（MCTS+ONNX / MCTS+DL / Expectimax+NNUE）。
//!
//! 与对局 API 的差异：无会话、无版本号、不落子——连线程序自行持有局面，
//! 每次把 BFEN 发来询问着法即可，服务端不保存任何对局状态（重启无影响）。
//! 推理后端由模型文件扩展名决定：`.onnx` → MCTS+ONNX、`.pt` → MCTS+DL、
//! `.nnue` → Expectimax+NNUE；决策分派收敛于 `app::player::decide`（单一真源）。

use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use banqi_core::core::env::PositionSnapshot;

use crate::app::models::EngineKind;
use crate::app::player::{self, EngineParams, PlayerSlot, StrategyKind};
use crate::dto::{EngineMoveReq, EngineMoveResp};
use crate::error::ApiError;
use crate::state::{GameSession, SharedState};

/// POST /api/engine/move
///
/// 请求：`{ fen, mctsSims?, nnueDepth?, nnueBudget?, modelFile? }`；
/// 响应：`{ variant, currentPlayer, action, from, to, isChance, isFlip, mctsStats?, nnueStats? }`。
/// 输入局面已终局时返回 409（GAME_OVER）。
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

    // 终局预检：终局局面无着法可问（借用 GameSession 复用终局判定与动作视图逻辑）
    let session = GameSession::from_snapshot(variant, snap.clone(), crate::app::player::Slots::all_human());
    let (terminated, truncated, _) = session.over_state();
    if terminated || truncated {
        return Err(ApiError::GameOver);
    }

    let model_file = app.resolve_model_file(req.model_file.as_deref(), variant)?;
    // 推理后端按模型扩展名分派（单一真源：EngineKind）
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

    // 锁外搜索（blocking 线程池；无会话状态，无版本号校验需求，无上一棵树可复用）
    let app2 = Arc::clone(&app);
    let decision = tokio::task::spawn_blocking(move || player::decide(&snap, &slot, None, &app2.models))
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
