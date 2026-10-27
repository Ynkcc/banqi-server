use crate::ai;
use crate::api::parse_variant_or_err;
use crate::app::player::{PlayerSlot, StrategyKind};
use crate::app::sync::{lock, read_lock, write_lock};
use crate::dto::{
    ArchivedGameDetail, CreateGameReq, DryRunReq, GameCreatedResp, GameListResp,
    GameStateResp, MoveActionResp, MoveReq, MoveResp, PutSlotsReq, RecentGamesQuery,
    RecentGamesResp, ReplayGameReq, ReplayResp, SlotReq, SlotsReq, SuggestionResp, UndoReq,
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

    app.archive_session(game_id, &session);
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
    Ok(Json(ai::human_move(&app, game_id, &id, req.action).await?))
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
    let action = crate::app::envops::action_for_coords(s.core.variant, &s.core.current, q.from, q.to)
        .map_err(ApiError::BadRequest)?;
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

fn find_archived(app: &SharedState, id: &str) -> Result<ArchivedGameDetail, ApiError> {
    lock(&app.recent)
        .iter()
        .find(|g| g.summary.game_id == id)
        .cloned()
        .ok_or_else(|| ApiError::GameNotFound(format!("最近对局归档 {id}")))
}

pub async fn list_recent_games(
    State(app): State<SharedState>,
    axum::extract::Query(q): axum::extract::Query<RecentGamesQuery>,
) -> Json<RecentGamesResp> {
    let recent = lock(&app.recent);
    let limit = q.limit.unwrap_or(recent.len()).min(recent.len());
    Json(RecentGamesResp {
        games: recent
            .iter()
            .take(limit)
            .map(|g| g.summary.clone())
            .collect(),
    })
}

pub async fn get_recent_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<ArchivedGameDetail>, ApiError> {
    Ok(Json(find_archived(&app, &id)?))
}

pub async fn replay_recent_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<ReplayResp>, ApiError> {
    let detail = find_archived(&app, &id)?;
    let resp =
        crate::app::replay::build_replay(&detail).map_err(ApiError::Internal)?;
    Ok(Json(resp))
}

pub async fn replay_as_new_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    req: Option<Json<ReplayGameReq>>,
) -> Result<Json<GameCreatedResp>, ApiError> {
    let detail = find_archived(&app, &id)?;
    let snap = PositionSnapshot::from_fen(detail.initial_fen.trim())
        .map_err(|e| ApiError::Internal(format!("归档初始局面解析失败: {e}")))?;
    let variant = snap.variant;
    let slots = build_slots(&app, req.and_then(|Json(r)| r.slots).as_ref(), variant).await?;
    let new_id = Uuid::new_v4();
    let session = GameSession::from_snapshot(variant, snap, slots);
    let state = session.view(new_id);
    app.games
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(new_id, session);
    tracing::info!(
        archive = %id,
        game = %new_id,
        "从最近对局归档复演建局: variant={}",
        variant.as_str()
    );
    Ok(Json(GameCreatedResp {
        game_id: new_id.to_string(),
        state,
    }))
}
