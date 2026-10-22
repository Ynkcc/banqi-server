//! 策略回合执行：红/黑槽位中策略方的决策编排与落子。
//!
//! 锁纪律：handler 在锁内完成人类走子并校验版本号后立即释放锁；搜索类策略
//! 在 blocking 线程池执行（`app::player::decide` 统一决策入口，变体无关）；
//! 规则策略决策廉价，直接在锁内同步完成；完成后重取锁校验版本号，
//! 不一致则丢弃策略结果。
//!
//! 全部策略种类（Random / RevealFirst / CaptureFirst / Expectimax /
//! ExpectimaxNnue / MctsOnnx / MctsDL）的「策略 → 动作」分派收敛于
//! `crate::app::player::decide`（单一真源），本模块只负责编排：
//! - [`run_strategy_turn`]：当前行棋方按其槽位策略执行一步（人类走子后的
//!   自动应答回合与 `POST /run` 单步共用）；
//! - [`run_dry_run`]：非破坏性决策（不落子），供 dry-run 高亮建议走法；
//!   当前行为 Human 槽位时按请求字段构造提示策略（缺省 Expectimax）。
//!
//! 跨回合树复用：MCTS 类策略在锁 1 取出会话保留的上一棵树，沿「策略上手 →
//! 对手上手」推进根节点续用（`tree_reuse_plan`，对不上自动回退全新建树）；
//! 复用尝试会消费旧树——若最终版本号校验失败结果作废，旧树不恢复（可重新搜索）。

use std::sync::Arc;

use uuid::Uuid;

use crate::app::player::{self, PlayerSlot, StrategyKind};
use crate::app::sync::{read_lock, write_lock};
use banqi_core::core::env::{DarkChessEnv, PositionSnapshot};

use crate::dto::{player_str, AppliedAction, DryRunReq, GameStateResp, MctsStatsDto, NnueStatsDto, SuggestionResp};
use crate::error::ApiError;
use crate::state::{get_mut_session, get_session, AppState, SharedState};
use banqi_core::core::env::types::Slot;

/// 策略回合决策所需数据（锁 1 内取出，不跨 await）。
struct TurnPlan {
    snap: PositionSnapshot,
    /// 生效槽位（Agent 槽位已实时解析为策略槽位；override 指定时为该槽位）
    slot: PlayerSlot,
    /// MCTS 类策略：锁 1 取出的上一棵搜索树（树复用用；其余策略为 None）
    prev_tree: Option<crate::mcts::MctsTree<DarkChessEnv>>,
    /// 规则策略：锁内已完成的同步决策结果（其余策略为 None）
    ready_action: Option<usize>,
}

/// 解析生效槽位：Agent 槽位实时解析为策略槽位（变体校验；搜索配置取当前值）。
///
/// Agent 引用被对局删除等情况在此报错（AGENT_NOT_FOUND / 变体不一致 AGENT_VARIANT_MISMATCH）。
fn resolve_slot(app: &AppState, slot: &PlayerSlot, variant: banqi_core::core::env::Variant) -> Result<PlayerSlot, ApiError> {
    match slot {
        PlayerSlot::Agent(id) => app.resolve_agent_slot(*id, variant),
        other => Ok(other.clone()),
    }
}

/// 锁 1：校验状态并取出决策所需数据，立即释放（不跨 await）。
///
/// 规则策略决策廉价，直接在此锁内同步完成（`ready_action`）；
/// 其余策略仅取出槽位配置，搜索在锁外执行。
///
/// `override_slot`：Agent「单步」指定时生效——忽略当前方槽位绑定
/// （含 Human），以该槽位（须为策略）为当前行棋方执行一步。
fn plan_strategy_turn(
    app: &AppState,
    game_id: Uuid,
    expect_version: u64,
    override_slot: Option<PlayerSlot>,
) -> Result<TurnPlan, ApiError> {
    // 写锁：MCTS 类策略需取出上一棵树供复用（其余策略树保持原位）
    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &game_id.to_string(), game_id)?;
    if s.over_state().0 || s.over_state().1 {
        return Err(ApiError::GameOver);
    }
    if override_slot.is_none() && !s.strategy_to_move() {
        return Err(ApiError::HumanTurn);
    }
    if s.core.version != expect_version {
        return Err(ApiError::BadRequest(format!(
            "版本号已变化: 期望 {expect_version}，当前 {}",
            s.core.version
        )));
    }
    let variant = s.core.variant;
    let slot = match override_slot {
        Some(slot) => {
            // Agent 单步：override 槽位若绑定 Agent 则实时解析；其余直接生效
            resolve_slot(app, &slot, variant)?
        }
        None => resolve_slot(app, s.slots.side(s.core.current.current_player), variant)?,
    };
    // MCTS 类策略：锁 1 取出上一棵树供复用（其余策略保留原树不动，
    // 前端仍可浏览上次决策）
    let prev_tree = match slot.kind() {
        Some(k) if k.is_mcts() => s.take_reusable_mcts_tree(),
        _ => None,
    };
    let mut plan = TurnPlan {
        snap: s.core.current.clone(),
        slot,
        prev_tree,
        ready_action: None,
    };
    if matches!(
        plan.slot.kind(),
        Some(StrategyKind::Random | StrategyKind::RevealFirst | StrategyKind::CaptureFirst)
    ) {
        let action = player::decide(&s.core.current, &plan.slot, None, &app.models)
            .map_err(ApiError::Internal)?
            .action;
        plan.ready_action = Some(action);
    }
    Ok(plan)
}

/// 锁 2：校验版本号后落子（供全部策略路径共用）。
///
/// 返回 `Ok(Some(...))` = 已落子；`Ok(None)` = 版本号变化，结果被丢弃
/// （正常竞态，不视为错误）。
type TurnApplyResult = Result<
    Option<(
        GameStateResp,
        AppliedAction,
        Option<MctsStatsDto>,
        Option<NnueStatsDto>,
    )>,
    ApiError,
>;

#[allow(clippy::too_many_arguments)]
fn apply_strategy_action(
    app: &Arc<AppState>,
    game_id: Uuid,
    expect_version: u64,
    action: usize,
    mcts_tree: Option<crate::mcts::MctsTreeBox>,
    ex_tree: Option<crate::app::expectimax_view::ExpectimaxTree>,
    nnue_stats: Option<NnueStatsDto>,
    // Agent 单步 override：当前方可能是 Human 槽位（单步永远作用于当前行动方），
    // 跳过「行棋方须为策略」的防御检查（版本号一致即有效）
    override_slot: bool,
) -> TurnApplyResult {
    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &game_id.to_string(), game_id)?;
    if s.core.version != expect_version {
        tracing::info!(
            game = %game_id,
            "策略结果作废: 版本号 {expect_version} != {}（期间对局已被修改）",
            s.core.version
        );
        return Ok(None);
    }
    if !override_slot && !s.strategy_to_move() {
        // 版本号一致但行棋方已不是策略（理论上不该发生，防御性处理）
        return Ok(None);
    }
    let view = s.action_view(action);
    let player = s.core.current.current_player;
    let outcome = s.apply_action(action)?;
    // 保留本次搜索树供前端查询（下一步对局状态变更后仍可浏览，直到下次搜索替换；
    // 非搜索类策略落子后清空旧树避免展示过期决策）
    s.mcts_tree = mcts_tree;
    s.expectimax_tree = ex_tree;
    let mcts_stats = MctsStatsDto::from_session(s.mcts_tree.as_ref());
    tracing::debug!(game = %game_id, "策略落子: action={action}，终局={}", outcome.is_over());
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

/// 执行一轮策略走子（供「人类走子后自动策略回合」与「POST /run 单步」共用）。
///
/// - `expect_version`：调用方在锁内看到并准备给策略用的版本号；决策完成后
///   重取锁校验，若期间发生过任何状态变更（走子/悔棋），结果作废。
/// - `override_slot`：Agent「单步」指定时生效（`Some`）——忽略当前方槽位绑定
///   （含 Human），以该策略为当前行棋方执行一步；单步永远作用于当前行动方。
///
/// 返回 `Ok(Some(...))` = 策略已落子；`Ok(None)` = 版本号变化，结果被丢弃。
pub async fn run_strategy_turn(
    app: &SharedState,
    game_id: Uuid,
    expect_version: u64,
    override_slot: Option<PlayerSlot>,
) -> TurnApplyResult {
    let is_override = override_slot.is_some();
    let plan = plan_strategy_turn(app, game_id, expect_version, override_slot)?;

    // 规则策略：锁内已完成决策，直接落子
    if let Some(action) = plan.ready_action {
        return apply_strategy_action(
            app,
            game_id,
            expect_version,
            action,
            None,
            None,
            None,
            is_override,
        );
    }

    // 锁外搜索（blocking 线程池；模型加载 / NN 推理 / Expectimax 均在此完成）
    let app2 = Arc::clone(app);
    let snap = plan.snap;
    let slot = plan.slot;
    let prev_tree = plan.prev_tree;
    let decision = tokio::task::spawn_blocking(move || {
        player::decide(&snap, &slot, prev_tree, &app2.models)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("blocking 任务 join 失败: {e}")))?
    .map_err(ApiError::Internal)?;

    apply_strategy_action(
        app,
        game_id,
        expect_version,
        decision.action,
        decision.mcts_tree,
        decision.ex_tree,
        decision.nnue_stats,
        is_override,
    )
}

/// dry-run：为当前行棋方请求策略建议走法（不落子，不改变版本号）。
///
/// - `req.agent_id` 指定 Agent：以其当前配置（实时参数）为当前行棋方计算建议
///   （变体须一致），忽略 kind / model_file / 当前方槽位绑定；
/// - 当前方槽位为策略：预览该策略将走的棋（Agent 槽位实时解析）；
/// - 当前方槽位为 Human：作为提示，按请求字段构造提示策略
///   （`kind` 缺省 Expectimax；模型类策略解析/自动匹配模型文件）。
///
/// 搜索树（MCTS 类）在版本号未变时存入会话：前端可立即浏览，
/// 后续 run 也可尝试跨回合复用。建议随响应下发 `version`，前端仅在
/// 版本号一致时渲染高亮（防过期）。
pub async fn run_dry_run(
    app: &SharedState,
    game_id: Uuid,
    req: DryRunReq,
) -> Result<SuggestionResp, ApiError> {
    // 锁 1：取出局面 + 生效的提示槽位配置（立即释放，不跨 await）
    let (snap, version, hint_slot) = {
        let games = read_lock(&app.games);
        let s = get_session(&games, &game_id.to_string(), game_id)?;
        if s.over_state().0 || s.over_state().1 {
            return Err(ApiError::GameOver);
        }
        let current = s.core.current.current_player;
        let variant = s.core.variant;
        let hint_slot = match &req.agent_id {
            Some(id) => {
                // Agent 建议：永远作用于当前行动方，与托管阵营无关
                let agent_id = Uuid::parse_str(id.trim())
                    .map_err(|_| ApiError::BadRequest(format!("非法 Agent ID: {id}")))?;
                app.resolve_agent_slot(agent_id, variant)?
            }
            None => {
                let slot = s.slots.side(current).clone();
                match slot {
                    PlayerSlot::Strategy { .. } => slot,
                    PlayerSlot::Agent(id) => app.resolve_agent_slot(id, variant)?,
                    PlayerSlot::Human => {
                        let kind = match req.kind.as_deref() {
                            None | Some("") => StrategyKind::Expectimax,
                            Some(k) => StrategyKind::parse(k).ok_or_else(|| {
                                ApiError::BadRequest(format!(
                                    "非法策略种类: {k}（可选 Random / RevealFirst / CaptureFirst / Expectimax / ExpectimaxNnue / MctsOnnx / MctsDL）"
                                ))
                            })?,
                        };
                        let params = app.params();
                        let model_file = if kind.needs_model() {
                            Some(app.resolve_model_file(req.model_file.as_deref(), variant)?)
                        } else {
                            None
                        };
                        PlayerSlot::strategy(kind, model_file, params)
                    }
                }
            }
        };
        (s.core.current.clone(), s.core.version, hint_slot)
    };

    // 锁外搜索（blocking 线程池；全新搜索，不消费会话树）
    let app2 = Arc::clone(app);
    let decision = tokio::task::spawn_blocking(move || player::dry_run(&snap, &hint_slot, &app2.models))
        .await
        .map_err(|e| ApiError::Internal(format!("blocking 任务 join 失败: {e}")))?
        .map_err(ApiError::Internal)?;

    // 锁 2：构造建议视图；版本号未变时存入搜索树（供浏览与后续 run 复用）
    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &game_id.to_string(), game_id)?;
    let player = s.core.current.current_player;
    let view = s.action_view(decision.action);
    let is_flip = decision.action < s.core.variant.config().reveal_actions_count;
    // 吃明子：目标格为对方已翻开的明子（吃暗子 / 翻棋均为机会动作，已排除）
    let is_capture = !view.is_chance
        && !is_flip
        && view.from != view.to
        && matches!(s.core.current.slots.get(view.to), Some(Slot::Revealed(_)));
    if s.core.version == version {
        s.mcts_tree = decision.mcts_tree;
        s.expectimax_tree = decision.ex_tree;
    }
    tracing::debug!(game = %game_id, "dry-run 建议: action={}（version={version}）", decision.action);
    Ok(SuggestionResp {
        version,
        player: player_str(player),
        action: decision.action,
        from: view.from,
        to: view.to,
        is_chance: view.is_chance,
        is_flip,
        is_capture,
        score: decision.score,
        mcts_stats: decision.mcts_stats,
        nnue_stats: decision.nnue_stats,
    })
}
