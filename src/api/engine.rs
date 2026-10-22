//! 无状态引擎端点：BFEN 局面 → 引擎最优动作（MCTS+ONNX 或 Expectimax+NNUE）。
//!
//! 与对局 API 的差异：无会话、无版本号、不落子——连线程序自行持有局面，
//! 每次把 BFEN 发来询问着法即可，服务端不保存任何对局状态（重启无影响）。
//! 推理后端由模型文件扩展名决定：`.onnx` → MCTS+ONNX（走 blocking 线程池，
//! NN 推理结果经模型实例上的 nncache 复用）；`.nnue` → Expectimax+NNUE。

use axum::extract::State;
use axum::Json;

use banqi_core::core::env::PositionSnapshot;

use crate::dto::{EngineMoveReq, EngineMoveResp, NnueStatsDto};
use crate::error::ApiError;
use crate::state::{EngineKind, GameSession, SharedState};

/// POST /api/engine/move
///
/// 请求：`{ fen, mctsSims?, nnueDepth?, nnueBudget?, modelFile? }`；
/// 响应：`{ variant, currentPlayer, action, from, to, isChance, mctsStats?, nnueStats? }`。
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
    let session = GameSession::from_snapshot(variant, snap.clone(), None, None);
    let (terminated, truncated, _) = session.over_state();
    if terminated || truncated {
        return Err(ApiError::GameOver);
    }

    let model_file = match req.model_file.as_deref().map(str::trim) {
        Some(f) if !f.is_empty() => f.to_string(),
        _ => app.auto_match_model(variant)?,
    };
    let engine = EngineKind::from_model_file(&model_file);
    let view_of = |action: usize| session.action_view(action);
    let is_flip = |action: usize| action < variant.config().reveal_actions_count;

    let (action, mcts_stats, nnue_stats) = match engine {
        EngineKind::Nnue => {
            let budget = app.build_search_budget(
                req.nnue_budget_mode.as_deref(),
                req.nnue_budget,
                req.nnue_depth,
                req.nnue_time_ms,
                req.search_timeout_ms,
            )?;
            let evaluator = app
                .acquire_nnue_model(model_file)
                .await
                .map_err(ApiError::ModelUnavailable)?;
            let (action, stats) = crate::ai::choose_nnue_blocking(snap, evaluator, budget)
                .await
                .map_err(ApiError::Internal)?;
            (action, None, Some(stats))
        }
        EngineKind::MctsOnnx => {
            #[cfg(feature = "onnx")]
            {
                let sims = req.mcts_sims.unwrap_or_else(|| app.default_sims()).max(1);
                let model = app
                    .acquire_model(model_file)
                    .await
                    .map_err(ApiError::ModelUnavailable)?;
                // 锁外推理（无会话状态，无版本号校验需求；无上一棵树可复用）
                let Some((action, tree)) =
                    crate::ai::choose_blocking(snap, model, sims, None)
                        .await
                        .map_err(ApiError::Internal)?
                else {
                    return Err(ApiError::Internal("MCTS 未返回动作（无合法动作）".into()));
                };
                let mcts_stats = crate::dto::MctsStatsDto::from_session(Some(&tree))
                    .ok_or_else(|| ApiError::Internal("搜索树缺少统计信息".into()))?;
                (action, Some(mcts_stats), None)
            }
            #[cfg(not(feature = "onnx"))]
            {
                let _ = (&req.mcts_sims, &snap, &model_file);
                return Err(ApiError::ModelUnavailable(
                    "服务编译未启用 onnx 推理，MCTS+ONNX 路径不可用（请使用 .nnue / .pt 模型）".into(),
                ));
            }
        }
        EngineKind::MctsDL => {
            #[cfg(feature = "torch")]
            {
                let sims = req.mcts_sims.unwrap_or_else(|| app.default_sims()).max(1);
                let model = app
                    .acquire_torch_model(model_file)
                    .await
                    .map_err(ApiError::ModelUnavailable)?;
                // 锁外推理（无会话状态，无版本号校验需求；无上一棵树可复用）
                let Some((action, tree)) =
                    crate::ai::choose_torch_blocking(snap, model, sims, None)
                        .await
                        .map_err(ApiError::Internal)?
                else {
                    return Err(ApiError::Internal("MCTS 未返回动作（无合法动作）".into()));
                };
                let mcts_stats = crate::dto::MctsStatsDto::from_session(Some(&tree))
                    .ok_or_else(|| ApiError::Internal("搜索树缺少统计信息".into()))?;
                (action, Some(mcts_stats), None)
            }
            #[cfg(not(feature = "torch"))]
            {
                let _ = (&req.mcts_sims, &snap, &model_file);
                return Err(ApiError::ModelUnavailable(
                    "服务编译未启用 torch 推理，MCTS+DL 路径不可用（请使用 .onnx / .nnue 模型）".into(),
                ));
            }
        }
    };

    let view = view_of(action);
    tracing::info!(
        variant = variant.as_str(),
        engine = ?engine,
        "engine/move: action={action}（from={} to={}，chance={}）",
        view.from,
        view.to,
        view.is_chance
    );

    Ok(Json(EngineMoveResp {
        variant: variant.as_str().to_string(),
        current_player,
        action,
        from: view.from,
        to: view.to,
        is_chance: view.is_chance,
        is_flip: is_flip(action),
        mcts_stats,
        nnue_stats,
    }))
}

/// 保持 NnueStatsDto 导入被无 onnx 构建引用（抑制未使用告警的一致性占位）。
#[allow(dead_code)]
fn _touch(_: Option<NnueStatsDto>) {}
