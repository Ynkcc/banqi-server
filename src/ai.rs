//! AI 决策：`spawn_blocking` 包装 MCTS+ONNX 推理，含版本号校验。
//!
//! 锁纪律：handler 在锁内完成人类走子并克隆快照后立即释放锁；MCTS 在
//! blocking 线程池用快照重建的泛型环境执行（`with_env!`，变体无关）；
//! 完成后重取锁校验版本号，不一致则丢弃 AI 结果。

use std::sync::Arc;

use uuid::Uuid;

#[cfg(feature = "onnx")]
use banqi_core::core::env::{PositionSnapshot, Variant};
#[cfg(feature = "onnx")]
use banqi_engine::inference::onnx::{OnnxModel, onnx_choose_action_once};

#[cfg(feature = "onnx")]
use crate::dto::player_str;
use crate::dto::{AppliedAction, GameStateResp};
use crate::error::ApiError;
use crate::state::AppState;

/// 在 blocking 线程池内执行一次 MCTS+ONNX 决策。
///
/// 变体无关：由 `(变体, 快照)` 重建泛型环境后调用
/// `onnx_choose_action_once::<E>`（GumbelMCTS 已在 banqi-engine 内处理翻棋机会节点）。
#[cfg(feature = "onnx")]
async fn choose_blocking(
    variant: Variant,
    snap: PositionSnapshot,
    model: Arc<OnnxModel>,
    sims: usize,
) -> Result<Option<usize>, String> {
    tokio::task::spawn_blocking(move || {
        with_env!(variant, &snap, env => onnx_choose_action_once(&model, &env, sims))
            .map_err(|e| format!("重建环境失败: {e}"))
            .and_then(|r| r.map_err(|e| format!("MCTS+ONNX 决策失败: {e}")))
    })
    .await
    .map_err(|e| format!("blocking 任务 join 失败: {e}"))?
}

/// 执行一轮 AI 走子（供「人类走子后自动 AI 回合」与「强制 AI 走子」共用）。
///
/// - `expect_version`：调用方在锁内看到并准备给 AI 用的版本号；推理完成后
///   重取锁校验，若期间发生过任何状态变更（走子/悔棋），AI 结果作废。
///
/// 返回 `Ok(Some(...))` = AI 已落子；`Ok(None)` = 版本号变化，AI 结果被丢弃
/// （正常竞态，不视为错误）。
#[cfg(feature = "onnx")]
pub async fn run_ai_turn(
    app: &Arc<AppState>,
    game_id: Uuid,
    expect_version: u64,
) -> Result<Option<(GameStateResp, AppliedAction)>, ApiError> {
    // 锁 1：校验状态并取出决策所需数据，立即释放（不跨 await）
    let (variant, snap, model, sims) = {
        let games = app.games.read().expect("games 锁中毒");
        let s = games
            .get(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(game_id.to_string()))?;
        let ai = s.ai.ok_or(ApiError::AiDisabled)?;
        if s.over_state().0 || s.over_state().1 {
            return Err(ApiError::GameOver);
        }
        if !s.ai_to_move() {
            return Err(ApiError::NotAiTurn);
        }
        if s.version != expect_version {
            return Err(ApiError::BadRequest(format!(
                "版本号已变化: 期望 {expect_version}，当前 {}",
                s.version
            )));
        }
        let model = app
            .models
            .get(s.variant)
            .cloned()
            .ok_or_else(|| ApiError::ModelUnavailable(format!("变体 {} 无已加载模型", s.variant.as_str())))?;
        (s.variant, s.current.clone(), model, ai.mcts_sims)
    };

    // 锁外推理（blocking 线程池）
    let Some(action) = choose_blocking(variant, snap, model, sims)
        .await
        .map_err(ApiError::Internal)?
    else {
        return Err(ApiError::Internal("MCTS 未返回动作（终局判定与状态不一致）".to_string()));
    };

    // 锁 2：校验版本号后落子
    let mut games = app.games.write().expect("games 锁中毒");
    let s = games
        .get_mut(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(game_id.to_string()))?;
    if s.version != expect_version {
        tracing::info!(
            game = %game_id,
            "AI 结果作废: 版本号 {expect_version} != {}（期间对局已被修改）",
            s.version
        );
        return Ok(None);
    }
    if !s.ai_to_move() {
        // 版本号一致但行棋方已不是 AI（理论上不该发生，防御性处理）
        return Ok(None);
    }
    let view = s.action_view(action);
    let player = s.current.current_player;
    let outcome = s.apply_action(action)?;
    tracing::debug!(game = %game_id, "AI 落子: action={action}，终局={}", outcome.is_over());
    Ok(Some((
        s.view(game_id),
        AppliedAction {
            player: player_str(player),
            action: view,
        },
    )))
}

/// 无 onnx feature 时的回退：AI 功能不可用。
#[cfg(not(feature = "onnx"))]
pub async fn run_ai_turn(
    _app: &Arc<AppState>,
    _game_id: Uuid,
    _expect_version: u64,
) -> Result<Option<(GameStateResp, AppliedAction)>, ApiError> {
    Err(ApiError::ModelUnavailable(
        "服务编译未启用 onnx 推理，AI 对手不可用".to_string(),
    ))
}
