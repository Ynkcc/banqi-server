//! 无状态引擎端点：BFEN 局面 → 引擎最优动作（MCTS+ONNX 或 Expectimax+NNUE）。
//!
//! 与对局 API 的差异：无会话、无版本号、不落子——连线程序自行持有局面，
//! 每次把 BFEN 发来询问着法即可，服务端不保存任何对局状态（重启无影响）。
//! 推理后端由模型文件扩展名决定：`.onnx` → MCTS+ONNX（走 blocking 线程池，
//! NN 推理结果经模型实例上的 nncache 复用）；`.nnue` → Expectimax+NNUE。

use axum::extract::State;
use axum::Json;

use banqi_core::core::env::PositionSnapshot;

use crate::ai::{choose_mcts_by_engine, choose_nnue_blocking};
use crate::dto::{EngineMoveReq, EngineMoveResp, MctsStatsDto};
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

    let model_file = app.resolve_model_file(req.model_file.as_deref(), variant)?;
    let engine = EngineKind::from_model_file(&model_file);
    let view_of = |action: usize| session.action_view(action);
    let is_flip = |action: usize| action < variant.config().reveal_actions_count;

    // 搜索执行：Nnue 走 Expectimax（本地），MCTS 类后端经
    // choose_mcts_by_engine 单点调度（feature 门控与报错收敛于此）
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
            let (action, stats, _tree) = choose_nnue_blocking(snap, evaluator, budget)
                .await
                .map_err(ApiError::Internal)?;
            (action, None, Some(stats))
        }
        EngineKind::MctsOnnx | EngineKind::MctsDL => {
            let sims = req.mcts_sims.unwrap_or_else(|| app.default_sims()).max(1);
            // 锁外推理（无会话状态，无版本号校验需求；无上一棵树可复用）
            let (action, tree) =
                choose_mcts_by_engine(&app, model_file, snap, sims, None).await?;
            let mcts_stats = MctsStatsDto::from_session(Some(&tree))
                .ok_or_else(|| ApiError::Internal("搜索树缺少统计信息".into()))?;
            (action, Some(mcts_stats), None)
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
