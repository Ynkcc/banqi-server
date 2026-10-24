use crate::ai;
use crate::api::parse_variant_or_err;
use crate::app::player::{PlayerSlot, StrategyKind};
use crate::app::sync::{read_lock, write_lock};
use crate::dto::{
    AppliedAction, CreateGameReq, DryRunReq, GameCreatedResp, GameListResp, GameStateResp,
    MoveActionResp, MoveReq, MoveResp, PutSlotsReq, SlotReq, SlotsReq, SuggestionResp, UndoReq,
};
use crate::error::ApiError;
use crate::state::{GameSession, SharedState, get_mut_session, get_session, parse_game_id};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use banqi_core::core::env::PositionSnapshot;
use serde::Deserialize;
use uuid::Uuid;
async fn build_slot(
    app: &SharedState,
    req: SlotReq,
    variant: banqi_core::core::env::Variant,
) -> Result<PlayerSlot, ApiError> {
    if req.kind.eq_ignore_ascii_case("human") {
        return Ok(PlayerSlot::Human);
    }
    if req.kind.eq_ignore_ascii_case("agent") {
        let raw = req
            .agent_id
            .as_deref()
            .ok_or_else(|| ApiError::BadRequest("kind = \"Agent\" 时须提供 agentId".to_string()))?;
        let id = Uuid::parse_str(raw.trim())
            .map_err(|_| ApiError::BadRequest(format!("非法 Agent ID: {raw}")))?;
        app.get_agent_for_variant(id, variant)?;
        return Ok(PlayerSlot::Agent(id));
    }
    let kind = StrategyKind::parse(&req.kind).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "非法策略种类: {}（可选 Human / Random / RevealFirst / CaptureFirst / Expectimax / ExpectimaxNnue / MctsOnnx / MctsDL / Agent）",
            req.kind
        ))
    })?;
    let params = app.slot_params(req.params.as_ref())?;
    let model_file = if kind.needs_model() {
        let f = app.resolve_model_file(req.model_file.as_deref(), variant)?;
        let engine = kind.engine_kind().expect("needs_model() 蕴含模型类策略");
        app.preload_engine_model(engine, f.clone(), variant).await?;
        Some(f)
    } else {
        None
    };
    Ok(PlayerSlot::strategy(kind, model_file, params))
}
async fn build_slots(
    app: &SharedState,
    req: Option<&SlotsReq>,
    variant: banqi_core::core::env::Variant,
) -> Result<crate::app::player::Slots, ApiError> {
    let Some(req) = req else {
        return Ok(crate::app::player::Slots::all_human());
    };
    let red = match &req.red {
        Some(r) => build_slot(app, r.clone(), variant).await?,
        None => PlayerSlot::Human,
    };
    let black = match &req.black {
        Some(b) => build_slot(app, b.clone(), variant).await?,
        None => PlayerSlot::Human,
    };
    Ok(crate::app::player::Slots::new(red, black))
}
pub async fn create_game(
    State(app): State<SharedState>,
    Json(req): Json<CreateGameReq>,
) -> Result<Json<GameCreatedResp>, ApiError> {
    let fen_snapshot = match req.fen.as_deref() {
        Some(fen) if !fen.trim().is_empty() => {
            let snap = PositionSnapshot::from_fen(fen.trim())
                .map_err(|e| ApiError::BadRequest(format!("BFEN 局面解析失败: {e}")))?;
            if let Some(s) = req.variant.as_deref() {
                let want = parse_variant_or_err(s)?;
                if want != snap.variant {
                    return Err(ApiError::BadRequest(format!(
                        "BFEN 变体（{}）与请求变体（{}）不一致",
                        snap.variant.as_str(),
                        s
                    )));
                }
            }
            Some(snap)
        }
        _ => None,
    };
    let variant = match fen_snapshot.as_ref() {
        Some(s) => s.variant,
        None => match req.variant.as_deref() {
            None => app.default_variant(),
            Some(s) => parse_variant_or_err(s)?,
        },
    };
    if req.scenario.is_some() && fen_snapshot.is_some() {
        return Err(ApiError::BadRequest(
            "scenario 与 fen 互斥：BFEN 已携带完整局面，请二选一".to_string(),
        ));
    }
    let slots = build_slots(&app, req.slots.as_ref(), variant).await?;
    let game_id = Uuid::new_v4();
    let session = match fen_snapshot {
        Some(snap) => GameSession::from_snapshot(variant, snap, slots),
        None => GameSession::new_with_scenario(variant, req.scenario.as_ref(), slots)?,
    };
    let state = session.view(game_id);
    app.games
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(game_id, session);
    tracing::info!(
        game = %game_id,
        "创建对局: variant={}，red={}，black={}，scenario={:?}",
        variant.as_str(),
        state.slots.red.kind,
        state.slots.black.kind,
        req.scenario.as_ref().map(|s| s.kind_id())
    );
    Ok(Json(GameCreatedResp {
        game_id: game_id.to_string(),
        state,
    }))
}
pub async fn list_games(State(app): State<SharedState>) -> Json<GameListResp> {
    let games = read_lock(&app.games);
    let mut summaries: Vec<crate::dto::GameSummary> =
        games.iter().map(|(id, s)| s.summary(*id)).collect();
    summaries.sort_by(|a, b| a.game_id.cmp(&b.game_id));
    Json(GameListResp { games: summaries })
}
pub async fn get_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<GameStateResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let s = get_session(&games, &id, game_id)?;
    Ok(Json(s.view(game_id)))
}
pub async fn delete_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let game_id = parse_game_id(&id)?;
    let removed = write_lock(&app.games).remove(&game_id).is_some();
    if removed {
        tracing::info!(game = %game_id, "删除对局");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::GameNotFound(id))
    }
}
pub async fn move_piece(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<MoveReq>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let (mut state, mover, human_view, strategy_version) = {
        let mut games = write_lock(&app.games);
        let s = get_mut_session(&mut games, &id, game_id)?;
        let mover = s.core.current.current_player;
        let outcome = s.apply_action(req.action)?;
        let human_view = s.action_view(req.action);
        tracing::info!(
            game = %game_id,
            "人类走子: action={}，终局={}，winner={:?}",
            req.action,
            outcome.is_over(),
            outcome.winner
        );
        let state = s.view(game_id);
        let strategy_version = s.strategy_to_move().then_some(s.core.version);
        (state, mover, human_view, strategy_version)
    };
    let mut actions = vec![AppliedAction {
        player: crate::dto::player_str(mover),
        action: human_view,
    }];
    let mut mcts_stats = None;
    let mut nnue_stats = None;
    if let Some(version) = strategy_version {
        match ai::run_strategy_turn(&app, game_id, version, None).await? {
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
    Ok(Json(MoveResp::new(state, actions, mcts_stats, nnue_stats)))
}
pub async fn run(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    req: Option<Json<crate::dto::RunReq>>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let agent_id = req
        .and_then(|Json(r)| r.agent_id)
        .filter(|s| !s.trim().is_empty());
    let (version, override_slot) = {
        let games = read_lock(&app.games);
        let s = get_session(&games, &id, game_id)?;
        let override_slot = match agent_id.as_deref() {
            Some(raw) => {
                let agent_id = Uuid::parse_str(raw.trim())
                    .map_err(|_| ApiError::BadRequest(format!("非法 Agent ID: {raw}")))?;
                Some(app.resolve_agent_slot(agent_id, s.core.variant)?)
            }
            None => None,
        };
        (s.core.version, override_slot)
    };
    match ai::run_strategy_turn(&app, game_id, version, override_slot).await? {
        Some((state, action, mcts_stats, nnue_stats)) => Ok(Json(MoveResp::new(
            state,
            vec![action],
            mcts_stats,
            nnue_stats,
        ))),
        None => Err(ApiError::BadRequest(
            "策略结果作废（期间对局被并发修改），请重试".to_string(),
        )),
    }
}
pub async fn dry_run(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    req: Option<Json<DryRunReq>>,
) -> Result<Json<SuggestionResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let req = req.map(|Json(r)| r).unwrap_or(DryRunReq {
        kind: None,
        model_file: None,
        agent_id: None,
    });
    Ok(Json(ai::run_dry_run(&app, game_id, req).await?))
}
pub async fn put_slots(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<PutSlotsReq>,
) -> Result<Json<GameStateResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let variant = {
        let games = read_lock(&app.games);
        let s = get_session(&games, &id, game_id)?;
        s.core.variant
    };
    let mut updates: Vec<(banqi_core::core::env::types::Player, PlayerSlot)> = Vec::new();
    if let Some(r) = &req.red {
        updates.push((
            banqi_core::core::env::types::Player::Red,
            build_slot(&app, r.clone(), variant).await?,
        ));
    }
    if let Some(b) = &req.black {
        updates.push((
            banqi_core::core::env::types::Player::Black,
            build_slot(&app, b.clone(), variant).await?,
        ));
    }
    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &id, game_id)?;
    for (player, slot) in updates {
        tracing::info!(game = %game_id, "切换槽位: {} → {}", crate::dto::player_str(player),
            match &slot { PlayerSlot::Human => "Human".to_string(),
                PlayerSlot::Agent(agent_id) => format!("Agent({agent_id})"),
                PlayerSlot::Strategy { kind, .. } => kind.as_str().to_string() });
        *s.slots.side_mut(player) = slot;
    }
    Ok(Json(s.view(game_id)))
}
#[derive(Debug, Deserialize)]
pub struct MoveActionQuery {
    from: usize,
    to: usize,
}
pub async fn move_action(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<MoveActionQuery>,
) -> Result<Json<MoveActionResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let s = get_session(&games, &id, game_id)?;
    let total = {
        let cfg = s.core.variant.config();
        cfg.rows * cfg.cols
    };
    if q.from >= total || q.to >= total {
        return Err(ApiError::BadRequest(format!(
            "坐标越界: from={} to={}（变体 {} 共 {total} 格）",
            q.from,
            q.to,
            s.core.variant.as_str()
        )));
    }
    let action = if q.from == q.to {
        let tables = banqi_core::core::env::actions::action_lookup_tables(&s.core.variant.config());
        let a = tables.single_to_action[q.from];
        (a >= 0).then_some(a as usize)
    } else {
        use banqi_core::core::env::SnapshotEnv;
        let env = banqi_core::core::env::DarkChessEnv::from_snapshot(&s.core.current)
            .map_err(|e| ApiError::Internal(format!("重建环境失败: {e}")))?;
        env.get_action_for_coords(&[q.from, q.to])
    };
    Ok(Json(MoveActionResp { action }))
}
pub async fn undo(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<UndoReq>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &id, game_id)?;
    match req.steps {
        Some(steps) => {
            s.undo(steps)?;
            tracing::info!(game = %game_id, "悔棋 {steps} 步");
        }
        None => {
            s.undo_to_current_player()?;
            tracing::info!(game = %game_id, "悔棋至当前行棋方上次行动前");
        }
    }
    Ok(Json(MoveResp::new(s.view(game_id), Vec::new(), None, None)))
}
