use crate::app::mcts_view::mcts_stats_from_session;
use crate::app::player::{self, PlayerSlot, StrategyKind};
use crate::app::sync::{read_lock, write_lock};
use bqsrv_dto::{
    AppliedAction, DryRunReq, GameStateResp, MctsStatsDto, MoveResp, NnueStatsDto, SuggestionResp,
    player_str,
};
use bqsrv_dto::ApiError;
use crate::nnue::{ExRootInfo, ex_root_info};
use crate::state::{
    AppState, SharedState, get_mut_session, get_session, store_manual_search_trees,
};
use banqi_core::core::env::types::Slot;
use banqi_core::core::env::{DarkChessEnv, PositionSnapshot};
use std::sync::Arc;
use uuid::Uuid;
struct TurnPlan {
    snap: PositionSnapshot,
    slot: PlayerSlot,
    prev_tree: Option<crate::mcts::MctsTree<DarkChessEnv>>,
    ready_action: Option<usize>,
}
fn resolve_slot(
    app: &AppState,
    slot: &PlayerSlot,
    variant: banqi_core::core::env::Variant,
) -> Result<PlayerSlot, ApiError> {
    match slot {
        PlayerSlot::Agent(id) => app.resolve_agent_slot(*id, variant),
        other => Ok(other.clone()),
    }
}
fn plan_strategy_turn(
    app: &AppState,
    game_id: Uuid,
    expect_version: u64,
    override_slot: Option<PlayerSlot>,
) -> Result<TurnPlan, ApiError> {
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
        Some(slot) => resolve_slot(app, &slot, variant)?,
        None => resolve_slot(app, s.slots.side(s.core.current.current_player), variant)?,
    };
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
        return Ok(None);
    }
    let view = s.action_view(action);
    let player = s.core.current.current_player;
    let outcome = s.apply_action(action)?;
    s.mcts_tree = mcts_tree;
    s.expectimax_tree = ex_tree;
    let mcts_stats = mcts_stats_from_session(s.mcts_tree.as_ref());

    s.capture_step_stats(bqsrv_dto::StepStatsDto {
        mcts: mcts_stats.clone(),
        nnue: nnue_stats.clone(),
        note: None,
    });
    if outcome.is_over() {
        app.archive_session(game_id, s);
    }
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
pub async fn run_strategy_turn(
    app: &SharedState,
    game_id: Uuid,
    expect_version: u64,
    override_slot: Option<PlayerSlot>,
) -> TurnApplyResult {
    let is_override = override_slot.is_some();
    let plan = plan_strategy_turn(app, game_id, expect_version, override_slot)?;
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
    let app2 = Arc::clone(app);
    let snap = plan.snap;
    let slot = plan.slot;
    let prev_tree = plan.prev_tree;
    let decision =
        crate::app::common::blocking(move || player::decide(&snap, &slot, prev_tree, &app2.models))
            .await?;
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

#[allow(clippy::too_many_arguments)]
pub async fn human_move(
    app: &SharedState,
    game_id: Uuid,
    id: &str,
    action: usize,
    reveal: Option<banqi_core::core::env::types::Piece>,
    mover_label: Option<String>,
    note: Option<String>,
    diag: Option<bqsrv_dto::StepStatsDto>,
) -> Result<MoveResp, ApiError> {
    let (mut state, mover, human_view, strategy_version) = {
        let mut games = write_lock(&app.games);
        let s = get_mut_session(&mut games, id, game_id)?;
        let mover = s.core.current.current_player;
        let outcome = s
            .apply_action_with_reveal(action, reveal, mover_label)
            .map_err(|e| {
                tracing::warn!(
                    game = %game_id,
                    action,
                    reveal = ?reveal.map(|p| p.short_name()),
                    "落子被拒: {e}，当前局面: {}",
                    s.core.current.to_fen()
                );
                e
            })?;

        if note.is_some() || diag.is_some() {
            let mut stats = diag.unwrap_or_default();
            if stats.note.is_none() {
                stats.note = note;
            }
            s.capture_step_stats(stats);
        }
        if outcome.is_over() {
            app.archive_session(game_id, s);
        }
        let human_view = s.action_view(action);
        tracing::info!(
            game = %game_id,
            "人类走子: action={}，终局={}，winner={:?}",
            action,
            outcome.is_over(),
            outcome.winner
        );
        let state = s.view(game_id);
        let strategy_version = s.strategy_to_move().then_some(s.core.version);
        (state, mover, human_view, strategy_version)
    };
    let mut actions = vec![AppliedAction {
        player: player_str(mover),
        action: human_view,
    }];
    let mut mcts_stats = None;
    let mut nnue_stats = None;
    if let Some(version) = strategy_version {
        match run_strategy_turn(app, game_id, version, None).await? {
            Some((new_state, strategy_action, m, n)) => {
                actions.push(strategy_action);
                mcts_stats = m;
                nnue_stats = n;
                state = new_state;
            }
            None => {
                let games = read_lock(&app.games);
                if let Some(s) = games.get(&game_id) {
                    state = s.view(game_id);
                }
            }
        }
    }
    Ok(MoveResp::new(state, actions, mcts_stats, nnue_stats))
}

pub async fn manual_expectimax_search(
    app: &SharedState,
    game_id: Uuid,
    id: &str,
) -> Result<ExRootInfo, ApiError> {
    enum EvalSel {
        Rule,

        Nnue(String),
    }
    let (variant, snap, budget, eval_sel, version) = {
        let games = read_lock(&app.games);
        let s = get_session(&games, id, game_id)?;
        let side = s.slots.side(s.core.current.current_player);
        let (budget, eval_sel) = match side {
            PlayerSlot::Strategy {
                kind: StrategyKind::Expectimax,
                params,
                ..
            } => (params.engine_budget, EvalSel::Rule),
            PlayerSlot::Strategy {
                kind: StrategyKind::ExpectimaxNnue,
                model_file,
                params,
                ..
            } => {
                let f = match model_file.clone() {
                    Some(f) => f,
                    None => app.resolve_model_file(None, s.core.variant)?,
                };
                (params.nnue_budget, EvalSel::Nnue(f))
            }
            _ => (
                app.default_nnue_budget(),
                EvalSel::Nnue(app.resolve_model_file(None, s.core.variant)?),
            ),
        };
        (
            s.core.variant,
            s.core.current.clone(),
            budget,
            eval_sel,
            s.core.version,
        )
    };
    let search = |evaluator, budget, label| async move {
        crate::app::common::blocking(move || {
            use banqi_core::core::env::SnapshotEnv;
            let env =
                DarkChessEnv::from_snapshot(&snap).map_err(|e| format!("重建环境失败: {e}"))?;
            crate::app::search::expectimax_search_tree(&env, evaluator, budget, label)
        })
        .await
    };
    let kept = match eval_sel {
        EvalSel::Rule => {
            let evaluator =
                Arc::new(banqi_core::core::expectimax::rule_eval::RuleMaterialEval::default());
            search(evaluator, budget, "rule").await?.1
        }
        EvalSel::Nnue(model_file) => {
            if !model_file.ends_with(".nnue") {
                return Err(ApiError::BadRequest(
                    "Expectimax 手动搜索需要 .nnue 叶评估模型（.onnx / .pt 为 MCTS 策略模型，走 /mcts/search）"
                        .into(),
                ));
            }
            let evaluator = app
                .acquire_nnue_validated(model_file, variant)
                .await
                .map_err(ApiError::ModelUnavailable)?;
            search(evaluator, budget, "nnue").await?.1
        }
    };
    let info = ex_root_info(&kept).ok_or_else(|| ApiError::Internal("搜索未产生轨迹树".into()))?;
    let mut games = write_lock(&app.games);
    store_manual_search_trees(&mut games, game_id, version, None, Some(kept));
    tracing::info!(game = %game_id, "手动 Expectimax 搜索完成");
    Ok(info)
}
pub async fn run_dry_run(
    app: &SharedState,
    game_id: Uuid,
    req: DryRunReq,
) -> Result<SuggestionResp, ApiError> {
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
                            Some(k) => StrategyKind::parse(k)
                                .ok_or_else(|| ApiError::BadRequest(StrategyKind::invalid_kind_msg(k)))?,
                        };
                        let params = app.slot_params(req.params.as_ref())?;
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
    let app2 = Arc::clone(app);
    let decision = crate::app::common::blocking(move || {
        player::dry_run(&snap, &hint_slot, &app2.models)
    })
    .await?;
    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &game_id.to_string(), game_id)?;
    let player = s.core.current.current_player;
    let view = s.action_view(decision.action);
    let is_flip = crate::app::envops::is_flip_action(s.core.variant, decision.action);
    let is_capture = !view.is_chance
        && !is_flip
        && view.from != view.to
        && matches!(s.core.current.slots.get(view.to), Some(Slot::Revealed(_)));

    let search_info = decision.ex_tree.as_ref().and_then(ex_root_info);
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
        search_info,
    })
}
