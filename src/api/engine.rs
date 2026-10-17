//! 无状态引擎端点：BFEN 局面 → MCTS+ONNX 最优动作。
//!
//! 与对局 API 的差异：无会话、无版本号、不落子——连线程序自行持有局面，
//! 每次把 BFEN 发来询问着法即可，服务端不保存任何对局状态（重启无影响）。
//! 搜索全程走 blocking 线程池（`ai::choose_blocking`），NN 推理结果经
//! 模型实例上的 nncache 复用（同观测跨请求命中）。

use axum::extract::State;
use axum::Json;

#[cfg(feature = "onnx")]
use banqi_core::core::env::PositionSnapshot;

use crate::dto::{EngineMoveReq, EngineMoveResp, MctsStatsDto};
use crate::error::ApiError;
#[cfg(feature = "onnx")]
use crate::state::GameSession;
use crate::state::SharedState;

/// POST /api/engine/move
///
/// 请求：`{ fen, mctsSims?, modelFile? }`；
/// 响应：`{ variant, currentPlayer, action, from, to, isChance, mctsStats }`。
/// 输入局面已终局时返回 409（GAME_OVER）。
#[cfg(feature = "onnx")]
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

    let sims = req.mcts_sims.unwrap_or_else(|| app.default_sims()).max(1);
    let model_file = match req.model_file.as_deref().map(str::trim) {
        Some(f) if !f.is_empty() => f.to_string(),
        _ => auto_match_model(&app, variant)?,
    };
    let model = app
        .acquire_model(model_file)
        .await
        .map_err(ApiError::ModelUnavailable)?;

    // 锁外推理（无会话状态，无版本号校验需求）
    let Some((action, tree)) = crate::ai::choose_blocking(variant, snap, model, sims)
        .await
        .map_err(ApiError::Internal)?
    else {
        return Err(ApiError::Internal("MCTS 未返回动作（无合法动作）".into()));
    };
    let mcts_stats = MctsStatsDto::from_session(Some(&tree))
        .ok_or_else(|| ApiError::Internal("搜索树缺少统计信息".into()))?;
    let view = session.action_view(action);
    // 纯翻棋判定与 MoveRecord::from_step 同口径：翻棋动作段位于动作空间前部
    let is_flip = action < variant.config().reveal_actions_count;

    tracing::info!(
        variant = variant.as_str(),
        sims,
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
        is_flip,
        mcts_stats,
    }))
}

/// 未显式指定模型时的自动匹配：文件名主干 == 变体 ID 的唯一候选
/// （模型目录约定 `<variant_id>.onnx`，见 config.rs）。
#[cfg(feature = "onnx")]
fn auto_match_model(
    app: &SharedState,
    variant: banqi_core::core::env::Variant,
) -> Result<String, ApiError> {
    let id = variant.as_str();
    let candidates: Vec<String> = app
        .models
        .files()
        .into_iter()
        .filter(|f| {
            std::path::Path::new(f)
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(id))
        })
        .collect();
    match candidates.len() {
        1 => Ok(candidates.into_iter().next().expect("候选数已判为 1")),
        0 => Err(ApiError::BadRequest(format!(
            "未指定 modelFile 且未找到文件名为 {id}.onnx 的模型（可用清单见 GET /api/health）"
        ))),
        _ => Err(ApiError::BadRequest(format!(
            "变体 {id} 匹配到多个候选，须显式指定 modelFile: {}",
            candidates.join(", ")
        ))),
    }
}

/// 无 onnx feature 时的回退：引擎端点不可用。
#[cfg(not(feature = "onnx"))]
pub async fn engine_move(
    State(_app): State<SharedState>,
    Json(_req): Json<EngineMoveReq>,
) -> Result<Json<EngineMoveResp>, ApiError> {
    Err(ApiError::ModelUnavailable(
        "服务编译未启用 onnx 推理，引擎端点不可用".into(),
    ))
}
