//! AI 决策：`spawn_blocking` 包装 MCTS+ONNX / Expectimax+NNUE 推理，含版本号校验。
//!
//! 锁纪律：handler 在锁内完成人类走子并克隆快照后立即释放锁；搜索在
//! blocking 线程池用快照重建的泛型环境执行（`with_env!`，变体无关）；
//! 完成后重取锁校验版本号，不一致则丢弃 AI 结果。
//!
//! 推理后端由对局绑定模型文件的扩展名决定（`EngineKind`）：
//! - `.onnx` → Gumbel MCTS + ONNX 策略/价值网络（需 onnx feature）；
//! - `.nnue` → 迭代加深 Expectimax + NNUE 叶评估（量化前向，无外部依赖）。

use std::sync::Arc;

use uuid::Uuid;

#[cfg(feature = "onnx")]
use banqi_app::with_env;
use banqi_app::sync::{read_lock, write_lock};
use banqi_core::core::env::DarkChessEnv;
#[cfg(feature = "onnx")]
use banqi_core::core::env::{PositionSnapshot, Variant};
#[cfg(feature = "onnx")]
use banqi_core::core::mcts::GumbelConfig;
#[cfg(feature = "onnx")]
use banqi_engine::inference::onnx::{OnnxEvaluator, OnnxModel};
use banqi_engine::nnue::NnueEvaluator;

use crate::dto::{player_str, AppliedAction, GameStateResp, NnueStatsDto};
#[cfg(feature = "onnx")]
use crate::mcts::MctsTreeBox;
use crate::error::ApiError;
use crate::state::{AppState, EngineKind};

/// 在 blocking 线程池内执行一次 MCTS+ONNX 决策，并保留整棵搜索树。
///
/// 变体无关：由 `(变体, 快照)` 重建泛型环境后运行 Gumbel MCTS（机会节点由
/// core 内部处理）。搜索核心在 `banqi_app::search::run_gumbel`（单一真源）。
/// 返回 `(选择的动作, 变体擦除后的树)`。
#[cfg(feature = "onnx")]
pub(crate) async fn choose_blocking(
    variant: Variant,
    snap: PositionSnapshot,
    model: Arc<OnnxModel>,
    sims: usize,
) -> Result<Option<(usize, MctsTreeBox)>, String> {
    let joined = tokio::task::spawn_blocking(move || {
        with_env!(variant, &snap, env => {
            (|| -> Result<Option<(usize, MctsTreeBox)>, String> {
                let evaluator = OnnxEvaluator::<E>::new(model.clone());
                let tree = banqi_app::search::run_gumbel(
                    &env,
                    &evaluator,
                    GumbelConfig::for_play(sims),
                    None,
                )?;
                Ok(Some((tree.chosen_action, Box::new(tree) as MctsTreeBox)))
            })()
        })
    })
    .await
    .map_err(|e| format!("blocking 任务 join 失败: {e}"))?;
    joined
        .map_err(|e| format!("重建环境失败: {e}"))
        .and_then(std::convert::identity)
}

/// 在 blocking 线程池内执行一次 Expectimax+NNUE 决策。
///
/// `DarkChessEnv::from_snapshot` 按快照自带变体配置重建环境（所有变体通用），
/// 搜索核心在 `banqi_app::search::nnue_search`（含 `.nnue` 头部布局校验，
/// 跨变体权重硬失败）。返回 `(选择的动作, 搜索统计)`。
pub(crate) async fn choose_nnue_blocking(
    snap: banqi_core::core::env::PositionSnapshot,
    evaluator: Arc<NnueEvaluator>,
    depth: i32,
    budget: u64,
) -> Result<(usize, NnueStatsDto), String> {
    let joined = tokio::task::spawn_blocking(move || {
        use banqi_core::core::env::SnapshotEnv;

        let env = DarkChessEnv::from_snapshot(&snap)
            .map_err(|e| format!("重建环境失败: {e}"))?;
        banqi_app::search::nnue_search(&env, evaluator, depth, budget)
    })
    .await
    .map_err(|e| format!("blocking 任务 join 失败: {e}"))?;
    let (action, stats) = joined?;
    Ok((
        action,
        NnueStatsDto {
            engine: "nnue".to_string(),
            depth: stats.depth,
            budget: stats.budget,
            nodes: stats.nodes,
            reached_depth: stats.reached_depth,
            elapsed_ms: stats.elapsed_ms,
        },
    ))
}

/// AI 决策所需数据（锁 1 内取出，不跨 await）。
struct AiTurnPlan {
    #[cfg(feature = "onnx")]
    variant: banqi_core::core::env::Variant,
    snap: banqi_core::core::env::PositionSnapshot,
    engine: EngineKind,
    model_file: String,
    #[cfg(feature = "onnx")]
    mcts_sims: usize,
    nnue_depth: i32,
    nnue_budget: u64,
}

/// 锁 1：校验状态并取出决策所需数据，立即释放（不跨 await）。
fn plan_ai_turn(
    app: &AppState,
    game_id: Uuid,
    expect_version: u64,
) -> Result<AiTurnPlan, ApiError> {
    let games = read_lock(&app.games);
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
    if s.core.version != expect_version {
        return Err(ApiError::BadRequest(format!(
            "版本号已变化: 期望 {expect_version}，当前 {}",
            s.core.version
        )));
    }
    let model_file = s
        .model_file
        .clone()
        .ok_or_else(|| ApiError::BadRequest("对局未绑定模型文件".into()))?;
    Ok(AiTurnPlan {
        #[cfg(feature = "onnx")]
        variant: s.core.variant,
        snap: s.core.current.clone(),
        engine: ai.engine,
        model_file,
        #[cfg(feature = "onnx")]
        mcts_sims: ai.mcts_sims,
        nnue_depth: ai.nnue_depth,
        nnue_budget: ai.nnue_budget,
    })
}

/// 执行一轮 AI 走子（供「人类走子后自动 AI 回合」与「强制 AI 走子」共用）。
///
/// - `expect_version`：调用方在锁内看到并准备给 AI 用的版本号；推理完成后
///   重取锁校验，若期间发生过任何状态变更（走子/悔棋），AI 结果作废。
///
/// 返回 `Ok(Some(...))` = AI 已落子；`Ok(None)` = 版本号变化，AI 结果被丢弃
/// （正常竞态，不视为错误）。
pub async fn run_ai_turn(
    app: &Arc<AppState>,
    game_id: Uuid,
    expect_version: u64,
) -> Result<Option<(GameStateResp, AppliedAction, Option<crate::dto::MctsStatsDto>, Option<NnueStatsDto>)>, ApiError> {
    let plan = plan_ai_turn(app, game_id, expect_version)?;

    // 锁外搜索（blocking 线程池，按后端分派）
    let (action, mcts_tree, nnue_stats) = match plan.engine {
        EngineKind::Nnue => {
            let evaluator = app
                .acquire_nnue_model(plan.model_file)
                .await
                .map_err(ApiError::ModelUnavailable)?;
            let (action, stats) = choose_nnue_blocking(plan.snap, evaluator, plan.nnue_depth, plan.nnue_budget)
                .await
                .map_err(ApiError::Internal)?;
            (action, None, Some(stats))
        }
        EngineKind::MctsOnnx => {
            #[cfg(not(feature = "onnx"))]
            {
                return Err(ApiError::ModelUnavailable(
                    "服务编译未启用 onnx 推理，MCTS+ONNX 对手不可用（.nnue 对手仍可用）".into(),
                ));
            }
            #[cfg(feature = "onnx")]
            {
                let model = app
                    .acquire_model(plan.model_file)
                    .await
                    .map_err(ApiError::ModelUnavailable)?;
                let Some((action, tree)) = choose_blocking(plan.variant, plan.snap, model, plan.mcts_sims)
                    .await
                    .map_err(ApiError::Internal)?
                else {
                    return Err(ApiError::Internal(
                        "MCTS 未返回动作（终局判定与状态不一致）".to_string(),
                    ));
                };
                (action, Some(tree), None)
            }
        }
    };

    // 锁 2：校验版本号后落子
    let mut games = write_lock(&app.games);
    let s = games
        .get_mut(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(game_id.to_string()))?;
    if s.core.version != expect_version {
        tracing::info!(
            game = %game_id,
            "AI 结果作废: 版本号 {expect_version} != {}（期间对局已被修改）",
            s.core.version
        );
        return Ok(None);
    }
    if !s.ai_to_move() {
        // 版本号一致但行棋方已不是 AI（理论上不该发生，防御性处理）
        return Ok(None);
    }
    let view = s.action_view(action);
    let player = s.core.current.current_player;
    let outcome = s.apply_action(action)?;
    // 保留本次搜索树供前端查询（下一步对局状态变更后仍可浏览，直到下次搜索替换；
    // NNUE 对手无搜索树，落子后清空旧树避免展示过期决策）
    s.mcts_tree = mcts_tree;
    let mcts_stats = crate::dto::MctsStatsDto::from_session(s.mcts_tree.as_ref());
    tracing::debug!(game = %game_id, engine = ?plan.engine, "AI 落子: action={action}，终局={}", outcome.is_over());
    Ok(Some((
        s.view(game_id),
        AppliedAction {
            player: player_str(player),
            action: view,
        },
        mcts_stats,
        nnue_stats,
    )))
}
