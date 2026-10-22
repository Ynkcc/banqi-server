//! 对手回合执行：规则对手 / Engine / NNUE / MCTS+ONNX / MCTS+DL 的决策与落子。
//!
//! 锁纪律：handler 在锁内完成人类走子并校验版本号后立即释放锁；搜索类对手
//! 在 blocking 线程池用快照重建的泛型环境执行（`with_env!`，变体无关）；
//! 规则对手决策廉价，直接在锁内同步完成；完成后重取锁校验版本号，
//! 不一致则丢弃对手结果。
//!
//! 对手种类由 `crate::app::opponent::OpponentKind` 给出（单一真源）：
//! - Rule（Random / RevealFirst / CaptureFirst）：`choose_simple` 锁内同步决策，无模型；
//! - Engine：Expectimax 强引擎（`choose_simple`），blocking 线程池执行，无模型；
//! - Nnue：迭代加深 Expectimax + NNUE 叶评估（量化前向，无外部依赖）；
//! - MctsOnnx：Gumbel MCTS + ONNX 策略/价值网络（需 onnx feature）；
//! - MctsDL：Gumbel MCTS + TorchScript 策略/价值网络（需 torch feature）。
//!
//! 跨回合树复用：MCTS 类对手在锁 1 取出会话保留的上一棵树，沿「AI 上手 →
//! 对手上手」推进根节点续用（`tree_reuse_plan`，对不上自动回退全新建树）；
//! 复用尝试会消费旧树——若最终版本号校验失败结果作废，旧树不恢复（可重新搜索）。

use std::sync::Arc;

use uuid::Uuid;

use crate::app::opponent::{OpponentKind, SimpleKind};
use crate::app::search::{BudgetMode, SearchBudget};
use crate::app::sync::write_lock;
use banqi_core::core::env::{DarkChessEnv, PositionSnapshot};
#[cfg(feature = "torch")]
use banqi_engine::engine::ModelWrapper;
#[cfg(feature = "onnx")]
use banqi_engine::inference::onnx::OnnxModel;
use banqi_engine::nnue::NnueEvaluator;

use crate::dto::{player_str, AppliedAction, GameStateResp, MctsStatsDto, NnueStatsDto};
use crate::mcts::MctsTree;
#[cfg(feature = "onnx")]
use crate::mcts::MctsTreeBox;
use crate::error::ApiError;
use crate::state::{AppState, OpponentConfig};

/// 在 blocking 线程池内执行一次 MCTS+ONNX 决策，并保留整棵搜索树。
///
/// 变体无关：`DarkChessEnv::from_snapshot` 按快照自带变体配置重建环境
/// （所有变体通用），运行 Gumbel MCTS（机会节点由 core 内部处理）。
/// 搜索核心在 `crate::app::search::mcts_search_onnx`（单一真源）；
/// `prev_tree` 为上一棵搜索树（`DarkChessEnv` 实例化，与返回类型一致），
/// 非空时尝试跨回合树复用（对不上自动回退全新建树）。
/// 返回 `(选择的动作, 保留的搜索树)`。
#[cfg(feature = "onnx")]
pub(crate) async fn choose_blocking(
    snap: PositionSnapshot,
    model: Arc<OnnxModel>,
    sims: usize,
    prev_tree: Option<MctsTree<DarkChessEnv>>,
) -> Result<Option<(usize, MctsTreeBox)>, String> {
    let joined = tokio::task::spawn_blocking(move || {
        use banqi_core::core::env::SnapshotEnv;

        let env = DarkChessEnv::from_snapshot(&snap).map_err(|e| format!("重建环境失败: {e}"))?;
        let tree = crate::app::search::mcts_search_onnx(&env, sims, Some(model), prev_tree)?;
        Ok(Some((tree.chosen_action, Box::new(tree) as MctsTreeBox)))
    })
    .await
    .map_err(|e| format!("blocking 任务 join 失败: {e}"))?;
    joined
}

/// 在 blocking 线程池内执行一次 MCTS+DL（TorchScript）决策，并保留整棵搜索树。
///
/// 搜索核心在 `crate::app::search::mcts_search_torch`（单一真源）；
/// 树复用语义同 [`choose_blocking`]。返回 `(选择的动作, 保留的搜索树)`。
#[cfg(feature = "torch")]
pub(crate) async fn choose_torch_blocking(
    snap: PositionSnapshot,
    model: Arc<ModelWrapper>,
    sims: usize,
    prev_tree: Option<MctsTree<DarkChessEnv>>,
) -> Result<Option<(usize, MctsTreeBox)>, String> {
    let joined = tokio::task::spawn_blocking(move || {
        use banqi_core::core::env::SnapshotEnv;

        let env = DarkChessEnv::from_snapshot(&snap).map_err(|e| format!("重建环境失败: {e}"))?;
        let tree = crate::app::search::mcts_search_torch(&env, sims, Some(model), prev_tree)?;
        Ok(Some((tree.chosen_action, Box::new(tree) as MctsTreeBox)))
    })
    .await
    .map_err(|e| format!("blocking 任务 join 失败: {e}"))?;
    joined
}

/// 在 blocking 线程池内执行一次 Expectimax+NNUE 决策。
///
/// `DarkChessEnv::from_snapshot` 按快照自带变体配置重建环境（所有变体通用），
/// 搜索核心在 `crate::app::search::nnue_search`（含 `.nnue` 头部布局校验，
/// 跨变体权重硬失败）。返回 `(选择的动作, 搜索统计)`。
pub(crate) async fn choose_nnue_blocking(
    snap: PositionSnapshot,
    evaluator: Arc<NnueEvaluator>,
    budget: SearchBudget,
) -> Result<(usize, NnueStatsDto), String> {
    let joined = tokio::task::spawn_blocking(move || {
        use banqi_core::core::env::SnapshotEnv;

        let env = DarkChessEnv::from_snapshot(&snap)
            .map_err(|e| format!("重建环境失败: {e}"))?;
        crate::app::search::nnue_search(&env, evaluator, budget)
    })
    .await
    .map_err(|e| format!("blocking 任务 join 失败: {e}"))?;
    let (action, stats) = joined?;
    Ok((
        action,
        NnueStatsDto {
            engine: "nnue".to_string(),
            mode: stats.mode.to_string(),
            depth: stats.depth,
            budget: stats.budget,
            time_limit_ms: stats.time_limit_ms,
            nodes: stats.nodes,
            reached_depth: stats.reached_depth,
            elapsed_ms: stats.elapsed_ms,
        },
    ))
}

/// 对手回合决策所需数据（锁 1 内取出，不跨 await）。
#[cfg_attr(not(any(feature = "onnx", feature = "torch")), allow(dead_code))]
struct TurnPlan {
    snap: banqi_core::core::env::PositionSnapshot,
    kind: OpponentKind,
    model_file: Option<String>,
    mcts_sims: usize,
    engine_budget: u64,
    /// Expectimax 搜索预算（Nnue 全量；Engine 取节点预算 + 硬超时）
    search_budget: SearchBudget,
    /// MCTS 类对手：锁 1 取出的上一棵搜索树（树复用用；其余对手为 None）
    prev_tree: Option<MctsTree<DarkChessEnv>>,
    /// 规则对手：锁内已完成的同步决策结果（其余对手为 None）
    ready_action: Option<usize>,
}

/// 锁 1：校验状态并取出决策所需数据，立即释放（不跨 await）。
///
/// 规则对手决策廉价，直接在此锁内同步完成（`ready_action`）；
/// 其余对手仅取出参数，搜索在锁外执行。
fn plan_opponent_turn(
    app: &AppState,
    game_id: Uuid,
    expect_version: u64,
) -> Result<TurnPlan, ApiError> {
    // 写锁：MCTS 类对手需取出上一棵树供复用（其余对手树保持原位）
    let mut games = write_lock(&app.games);
    let s = games
        .get_mut(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(game_id.to_string()))?;
    let opp: OpponentConfig = s.opponent.ok_or(ApiError::AiDisabled)?;
    if s.over_state().0 || s.over_state().1 {
        return Err(ApiError::GameOver);
    }
    if !s.opponent_to_move() {
        return Err(ApiError::NotAiTurn);
    }
    if s.core.version != expect_version {
        return Err(ApiError::BadRequest(format!(
            "版本号已变化: 期望 {expect_version}，当前 {}",
            s.core.version
        )));
    }
    // MCTS 类对手：锁 1 取出上一棵树供复用（非 MCTS 对手保留原树不动，
    // 前端仍可浏览上次决策）
    let prev_tree = if matches!(opp.kind, OpponentKind::MctsOnnx | OpponentKind::MctsDL) {
        s.take_reusable_mcts_tree()
    } else {
        None
    };
    let plan = TurnPlan {
        snap: s.core.current.clone(),
        kind: opp.kind,
        model_file: s.model_file.clone(),
        mcts_sims: opp.mcts_sims,
        engine_budget: opp.engine_budget,
        search_budget: opp.search_budget,
        prev_tree,
        ready_action: None,
    };
    if let OpponentKind::Rule(rule) = opp.kind {
        let action = crate::app::opponent::choose_simple(
            SimpleKind::Rule(rule),
            &s.core.current,
            &opp.search_budget,
        )
        .map_err(ApiError::Internal)?;
        return Ok(TurnPlan {
            ready_action: Some(action),
            ..plan
        });
    }
    Ok(plan)
}

/// 锁 2：校验版本号后落子（供全部对手路径共用）。
///
/// 返回 `Ok(Some(...))` = 已落子；`Ok(None)` = 版本号变化，结果被丢弃
/// （正常竞态，不视为错误）。
fn apply_opponent_action(
    app: &Arc<AppState>,
    game_id: Uuid,
    expect_version: u64,
    action: usize,
    mcts_tree: Option<crate::mcts::MctsTreeBox>,
    nnue_stats: Option<NnueStatsDto>,
) -> Result<Option<(GameStateResp, AppliedAction, Option<MctsStatsDto>, Option<NnueStatsDto>)>, ApiError> {
    let mut games = write_lock(&app.games);
    let s = games
        .get_mut(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(game_id.to_string()))?;
    if s.core.version != expect_version {
        tracing::info!(
            game = %game_id,
            "对手结果作废: 版本号 {expect_version} != {}（期间对局已被修改）",
            s.core.version
        );
        return Ok(None);
    }
    if !s.opponent_to_move() {
        // 版本号一致但行棋方已不是对手（理论上不该发生，防御性处理）
        return Ok(None);
    }
    let view = s.action_view(action);
    let player = s.core.current.current_player;
    let outcome = s.apply_action(action)?;
    // 保留本次搜索树供前端查询（下一步对局状态变更后仍可浏览，直到下次搜索替换；
    // 非搜索类对手落子后清空旧树避免展示过期决策）
    s.mcts_tree = mcts_tree;
    let mcts_stats = MctsStatsDto::from_session(s.mcts_tree.as_ref());
    tracing::debug!(game = %game_id, "对手落子: action={action}，终局={}", outcome.is_over());
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

/// 执行一轮对手走子（供「人类走子后自动对手回合」与「强制对手走子」共用）。
///
/// - `expect_version`：调用方在锁内看到并准备给对手用的版本号；决策完成后
///   重取锁校验，若期间发生过任何状态变更（走子/悔棋），结果作废。
///
/// 返回 `Ok(Some(...))` = 对手已落子；`Ok(None)` = 版本号变化，结果被丢弃。
pub async fn run_opponent_turn(
    app: &Arc<AppState>,
    game_id: Uuid,
    expect_version: u64,
) -> Result<Option<(GameStateResp, AppliedAction, Option<MctsStatsDto>, Option<NnueStatsDto>)>, ApiError> {
    let plan = plan_opponent_turn(app, game_id, expect_version)?;

    // 规则对手：锁内已完成决策，直接落子
    if let Some(action) = plan.ready_action {
        return apply_opponent_action(app, game_id, expect_version, action, None, None);
    }

    // 锁外搜索（blocking 线程池，按对手种类分派）
    let (action, mcts_tree, nnue_stats) = match plan.kind {
        // 已在锁 1 处理，防御性兜底
        OpponentKind::Rule(_) => return Ok(None),
        OpponentKind::Engine => {
            let snap = plan.snap;
            // Engine 对手：节点预算 + 硬超时（节点模式）
            let budget = SearchBudget {
                mode: BudgetMode::Nodes,
                nodes: plan.engine_budget,
                timeout_ms: plan.search_budget.timeout_ms,
                ..Default::default()
            };
            let action = tokio::task::spawn_blocking(move || {
                crate::app::opponent::choose_simple(SimpleKind::Engine, &snap, &budget)
            })
            .await
            .map_err(|e| ApiError::Internal(format!("blocking 任务 join 失败: {e}")))?
            .map_err(ApiError::Internal)?;
            (action, None, None)
        }
        OpponentKind::Nnue => {
            let model_file = plan
                .model_file
                .clone()
                .ok_or_else(|| ApiError::BadRequest("对局未绑定模型文件".into()))?;
            let evaluator = app
                .acquire_nnue_model(model_file)
                .await
                .map_err(ApiError::ModelUnavailable)?;
            let (action, stats) =
                choose_nnue_blocking(plan.snap, evaluator, plan.search_budget)
                    .await
                    .map_err(ApiError::Internal)?;
            (action, None, Some(stats))
        }
        OpponentKind::MctsOnnx => {
            #[cfg(not(feature = "onnx"))]
            {
                return Err(ApiError::ModelUnavailable(
                    "服务编译未启用 onnx 推理，MCTS+ONNX 对手不可用（其余对手仍可用）".into(),
                ));
            }
            #[cfg(feature = "onnx")]
            {
                let model_file = plan
                    .model_file
                    .clone()
                    .ok_or_else(|| ApiError::BadRequest("对局未绑定模型文件".into()))?;
                let model = app
                    .acquire_model(model_file)
                    .await
                    .map_err(ApiError::ModelUnavailable)?;
                let Some((action, tree)) = choose_blocking(
                    plan.snap,
                    model,
                    plan.mcts_sims,
                    plan.prev_tree,
                )
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
        OpponentKind::MctsDL => {
            #[cfg(not(feature = "torch"))]
            {
                return Err(ApiError::ModelUnavailable(
                    "服务编译未启用 torch 推理，MCTS+DL 对手不可用（其余对手仍可用）".into(),
                ));
            }
            #[cfg(feature = "torch")]
            {
                let model_file = plan
                    .model_file
                    .clone()
                    .ok_or_else(|| ApiError::BadRequest("对局未绑定模型文件".into()))?;
                let model = app
                    .acquire_torch_model(model_file)
                    .await
                    .map_err(ApiError::ModelUnavailable)?;
                let Some((action, tree)) = choose_torch_blocking(
                    plan.snap,
                    model,
                    plan.mcts_sims,
                    plan.prev_tree,
                )
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

    apply_opponent_action(app, game_id, expect_version, action, mcts_tree, nnue_stats)
}
