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
//! MCTS 推理后端调度收敛于 [`choose_mcts_by_engine`]（单一真源）：按模型文件
//! 扩展名分派，feature 门控与「未启用即报错」在此单点维护；blocking 执行骨架
//! （快照重建环境 + spawn_blocking）由 [`run_mcts_blocking`] 统一，onnx / torch
//! 仅在搜索核心注入处不同。
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
use banqi_engine::nnue::NnueEvaluator;

use crate::dto::{player_str, AppliedAction, GameStateResp, MctsStatsDto, NnueStatsDto};
use crate::error::ApiError;
use crate::mcts::{MctsTree, MctsTreeBox};
use crate::state::{get_mut_session, AppState, EngineKind, OpponentConfig};

// ============================================================================
// MCTS 决策（onnx / torch 后端共用骨架 + 单点调度）
// ============================================================================

/// 在 blocking 线程池内执行一次 MCTS 决策并保留整棵搜索树。
///
/// 变体无关：`DarkChessEnv::from_snapshot` 按快照自带变体配置重建环境
/// （所有变体通用）；搜索核心差异由 `search` 闭包注入（onnx / torch 薄分发）。
/// `prev_tree` 非空时尝试跨回合树复用（对不上自动回退全新建树）。
/// 返回 `(选择的动作, 保留的搜索树)`。
#[cfg(any(feature = "onnx", feature = "torch"))]
async fn run_mcts_blocking<F>(
    snap: PositionSnapshot,
    sims: usize,
    prev_tree: Option<MctsTree<DarkChessEnv>>,
    search: F,
) -> Result<(usize, MctsTreeBox), ApiError>
where
    F: FnOnce(&DarkChessEnv, usize, Option<MctsTree<DarkChessEnv>>) -> Result<MctsTree<DarkChessEnv>, String>
        + Send
        + 'static,
{
    let joined = tokio::task::spawn_blocking(move || {
        use banqi_core::core::env::SnapshotEnv;

        let env = DarkChessEnv::from_snapshot(&snap).map_err(|e| format!("重建环境失败: {e}"))?;
        search(&env, sims, prev_tree)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("blocking 任务 join 失败: {e}")))?;
    let tree = joined.map_err(ApiError::Internal)?;
    Ok((tree.chosen_action, Box::new(tree) as MctsTreeBox))
}

/// 按绑定模型文件的扩展名分派 MCTS 推理后端，在 blocking 线程池执行一次
/// Gumbel MCTS 决策并保留整棵搜索树（对手回合 / 手动搜索 / engine 查询共用）。
///
/// 单一真源：原 `build_opponent` / `bind_model` / `mcts::search` /
/// `engine::engine_move` / `run_opponent_turn` 五处 feature 门控调度收敛于此；
/// feature 未启用时在此报 `ModelUnavailable`，`.nnue` 文件（无搜索树）报 400。
pub(crate) async fn choose_mcts_by_engine(
    app: &Arc<AppState>,
    model_file: String,
    snap: PositionSnapshot,
    sims: usize,
    prev_tree: Option<MctsTree<DarkChessEnv>>,
) -> Result<(usize, MctsTreeBox), ApiError> {
    match EngineKind::from_model_file(&model_file) {
        EngineKind::Nnue => Err(ApiError::BadRequest(
            "MCTS 搜索需要 .onnx / .pt 策略价值模型（.nnue 为 NNUE 叶评估后端，无搜索树）".into(),
        )),
        EngineKind::MctsOnnx => {
            #[cfg(feature = "onnx")]
            {
                let model = app
                    .acquire_model(model_file)
                    .await
                    .map_err(ApiError::ModelUnavailable)?;
                run_mcts_blocking(snap, sims, prev_tree, move |env, sims, prev| {
                    crate::app::search::mcts_search_onnx(env, sims, Some(model), prev)
                })
                .await
            }
            #[cfg(not(feature = "onnx"))]
            {
                let _ = (snap, sims, prev_tree);
                Err(ApiError::ModelUnavailable(
                    "服务编译未启用 onnx 推理，MCTS+ONNX 路径不可用（可改用 .nnue / .pt 模型）"
                        .into(),
                ))
            }
        }
        EngineKind::MctsDL => {
            #[cfg(feature = "torch")]
            {
                let model = app
                    .acquire_torch_model(model_file)
                    .await
                    .map_err(ApiError::ModelUnavailable)?;
                run_mcts_blocking(snap, sims, prev_tree, move |env, sims, prev| {
                    crate::app::search::mcts_search_torch(env, sims, Some(model), prev)
                })
                .await
            }
            #[cfg(not(feature = "torch"))]
            {
                let _ = (snap, sims, prev_tree);
                Err(ApiError::ModelUnavailable(
                    "服务编译未启用 torch 推理，MCTS+DL 路径不可用（可改用 .onnx / .nnue 模型）"
                        .into(),
                ))
            }
        }
    }
}

/// 在 blocking 线程池内执行一次 Expectimax+NNUE 决策，并保留搜索轨迹树。
///
/// `DarkChessEnv::from_snapshot` 按快照自带变体配置重建环境（所有变体通用），
/// 搜索核心在 `crate::app::search::nnue_search_tree`（含 `.nnue` 头部布局校验，
/// 跨变体权重硬失败；可视化约束 threads=1 + 关闭 ASP + 树捕获）。
/// 返回 `(选择的动作, 搜索统计, 搜索轨迹树)`。
pub(crate) async fn choose_nnue_blocking(
    snap: PositionSnapshot,
    evaluator: Arc<NnueEvaluator>,
    budget: SearchBudget,
) -> Result<(usize, NnueStatsDto, Option<crate::app::expectimax_view::ExpectimaxTree>), String> {
    let joined = tokio::task::spawn_blocking(move || {
        use banqi_core::core::env::SnapshotEnv;

        let env = DarkChessEnv::from_snapshot(&snap)
            .map_err(|e| format!("重建环境失败: {e}"))?;
        crate::app::search::nnue_search_tree(&env, evaluator, budget)
    })
    .await
    .map_err(|e| format!("blocking 任务 join 失败: {e}"))?;
    let (action, kept) = joined?;
    let stats = &kept.stats;
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
        Some(kept),
    ))
}

/// 对手回合决策所需数据（锁 1 内取出，不跨 await）。
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
    let s = get_mut_session(&mut games, &game_id.to_string(), game_id)?;
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
/// [`apply_opponent_action`] 返回值：`(对局响应, 已执行动作, MCTS 统计, NNUE 统计)`。
type OpponentApplyResult =
    Result<Option<(GameStateResp, AppliedAction, Option<MctsStatsDto>, Option<NnueStatsDto>)>, ApiError>;

fn apply_opponent_action(
    app: &Arc<AppState>,
    game_id: Uuid,
    expect_version: u64,
    action: usize,
    mcts_tree: Option<crate::mcts::MctsTreeBox>,
    ex_tree: Option<crate::app::expectimax_view::ExpectimaxTree>,
    nnue_stats: Option<NnueStatsDto>,
) -> OpponentApplyResult {
    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &game_id.to_string(), game_id)?;
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
    s.expectimax_tree = ex_tree;
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
        return apply_opponent_action(app, game_id, expect_version, action, None, None, None);
    }

    // 锁外搜索（blocking 线程池，按对手种类分派）
    let (action, mcts_tree, ex_tree, nnue_stats) = match plan.kind {
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
            (action, None, None, None)
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
            let (action, stats, ex_tree) =
                choose_nnue_blocking(plan.snap, evaluator, plan.search_budget)
                    .await
                    .map_err(ApiError::Internal)?;
            (action, None, ex_tree, Some(stats))
        }
        // MCTS 类对手：推理后端按模型文件扩展名分派（choose_mcts_by_engine 单一真源）
        OpponentKind::MctsOnnx | OpponentKind::MctsDL => {
            let model_file = plan
                .model_file
                .clone()
                .ok_or_else(|| ApiError::BadRequest("对局未绑定模型文件".into()))?;
            let (action, tree) = choose_mcts_by_engine(
                app,
                model_file,
                plan.snap,
                plan.mcts_sims,
                plan.prev_tree,
            )
            .await?;
            (action, Some(tree), None, None)
        }
    };

    apply_opponent_action(app, game_id, expect_version, action, mcts_tree, ex_tree, nnue_stats)
}
