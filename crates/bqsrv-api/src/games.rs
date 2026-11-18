use crate::parse_variant_or_err;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use banqi_core::core::env::{OpeningScenario, PositionSnapshot};
use bqsrv_app::ai;
use bqsrv_app::app::player::{PlayerSlot, StrategyKind};
use bqsrv_app::app::sync::{lock, read_lock, write_lock};
use bqsrv_app::state::{
    ArchivedGame, GameSession, SharedState, get_mut_session, get_session, parse_game_id,
};
use bqsrv_dto::{
    ApiError, ArchivedGameDetail, CreateGameReq, DryRunReq, GameCreatedResp, GameListResp,
    GameStateResp, MoveActionResp, MoveReq, MoveResp, PutSlotsReq, RecentGamesQuery,
    RecentGamesResp, ReplayGameReq, ReplayResp, SlotReq, SlotsReq, SuggestionResp, UndoReq,
};
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
            "非法策略种类: {}（可选 Human / {} / Agent）",
            req.kind,
            StrategyKind::KIND_CHOICES
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
) -> Result<(bqsrv_app::app::player::Slots, [Option<String>; 2]), ApiError> {
    let Some(req) = req else {
        return Ok((bqsrv_app::app::player::Slots::all_human(), [None, None]));
    };
    let red = match &req.red {
        Some(r) => build_slot(app, r.clone(), variant).await?,
        None => PlayerSlot::Human,
    };
    let black = match &req.black {
        Some(b) => build_slot(app, b.clone(), variant).await?,
        None => PlayerSlot::Human,
    };
    let labels = [
        req.red.as_ref().and_then(|r| r.label.clone()),
        req.black.as_ref().and_then(|b| b.label.clone()),
    ];
    Ok((bqsrv_app::app::player::Slots::new(red, black), labels))
}

fn resolve_fen_scenario(
    fen: Option<&str>,
    scenario: Option<OpeningScenario>,
    req_variant: Option<&str>,
) -> Result<
    (
        Option<OpeningScenario>,
        Option<banqi_core::core::env::Variant>,
    ),
    ApiError,
> {
    let fen_raw = fen.map(str::trim).filter(|s| !s.is_empty());
    let scenario = match (fen_raw, scenario) {
        (Some(_), Some(sc)) if !matches!(sc, OpeningScenario::FromFen(_)) => {
            return Err(ApiError::BadRequest(
                "scenario 与 fen 互斥：仅 scenario kind = from_fen 时允许同时携带 fen，请二选一"
                    .to_string(),
            ));
        }
        (Some(fen), None) => Some(
            OpeningScenario::try_from_fen(fen)
                .map_err(|e| ApiError::BadRequest(format!("BFEN 局面解析失败: {e}")))?,
        ),
        (_, sc) => sc,
    };
    let fen_variant = match scenario.as_ref() {
        Some(OpeningScenario::FromFen(s)) => {
            let snap = PositionSnapshot::from_fen(s.fen.as_str())
                .map_err(|e| ApiError::BadRequest(format!("BFEN 局面解析失败: {e}")))?;
            {
                let probe = GameSession::from_snapshot(
                    snap.variant,
                    snap.clone(),
                    bqsrv_app::app::player::Slots::all_human(),
                );
                let (terminated, truncated, _) = probe.over_state();
                if terminated || truncated {
                    return Err(ApiError::GameOver);
                }
            }
            if let Some(s) = req_variant {
                let want = parse_variant_or_err(s)?;
                if want != snap.variant {
                    return Err(ApiError::BadRequest(format!(
                        "BFEN 变体（{}）与请求变体（{}）不一致",
                        snap.variant.as_str(),
                        s
                    )));
                }
            }
            Some(snap.variant)
        }
        _ => None,
    };
    Ok((scenario, fen_variant))
}
pub async fn create_game(
    State(app): State<SharedState>,
    Json(req): Json<CreateGameReq>,
) -> Result<Json<GameCreatedResp>, ApiError> {
    let (scenario, fen_variant) =
        resolve_fen_scenario(req.fen.as_deref(), req.scenario, req.variant.as_deref())?;
    let variant = match fen_variant {
        Some(v) => v,
        None => match req.variant.as_deref() {
            None => app.default_variant(),
            Some(s) => parse_variant_or_err(s)?,
        },
    };
    let (slots, slot_labels) = build_slots(&app, req.slots.as_ref(), variant).await?;
    let game_id = Uuid::new_v4();
    let mut session = GameSession::new_with_scenario(variant, scenario.as_ref(), slots)?;
    session.slot_labels = slot_labels;
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
        scenario.as_ref().map(|s| s.kind_id())
    );
    Ok(Json(GameCreatedResp {
        game_id: game_id.to_string(),
        state,
    }))
}
pub async fn list_games(State(app): State<SharedState>) -> Json<GameListResp> {
    let games = read_lock(&app.games);
    let mut summaries: Vec<bqsrv_dto::GameSummary> =
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
#[derive(Debug, Deserialize)]
pub struct DeleteGameQuery {
    #[serde(default)]
    pub archive: Option<bool>,
}
pub async fn delete_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<DeleteGameQuery>,
) -> Result<StatusCode, ApiError> {
    let game_id = parse_game_id(&id)?;
    let session = write_lock(&app.games).remove(&game_id);
    match session {
        Some(s) => {
            if q.archive.unwrap_or(false) {
                app.archive_session_final(game_id, &s);
            }
            tracing::info!(game = %game_id, "删除对局（archive={}）", q.archive.unwrap_or(false));
            Ok(StatusCode::NO_CONTENT)
        }
        None => Err(ApiError::GameNotFound(id)),
    }
}
pub async fn move_piece(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<MoveReq>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let reveal = match req.reveal {
        None => None,
        Some(code) => Some(bqsrv_dto::piece_from_display_code(code).ok_or_else(|| {
            ApiError::BadRequest(format!("非法 reveal 棋子码: {code}（应为明子码 0-13）"))
        })?),
    };
    Ok(Json(
        ai::human_move(
            &app, game_id, &id, req.action, reveal, req.mover, req.note, req.diag,
        )
        .await?,
    ))
}
pub async fn run(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    req: Option<Json<bqsrv_dto::RunReq>>,
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
        params: None,
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
    let mut updates: Vec<(
        banqi_core::core::env::types::Player,
        PlayerSlot,
        Option<String>,
    )> = Vec::new();
    if let Some(r) = &req.red {
        updates.push((
            banqi_core::core::env::types::Player::Red,
            build_slot(&app, r.clone(), variant).await?,
            r.label.clone(),
        ));
    }
    if let Some(b) = &req.black {
        updates.push((
            banqi_core::core::env::types::Player::Black,
            build_slot(&app, b.clone(), variant).await?,
            b.label.clone(),
        ));
    }
    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &id, game_id)?;
    for (player, slot, label) in updates {
        s.set_slot_label(player, label);
        tracing::info!(game = %game_id, "切换槽位: {} → {}", bqsrv_dto::player_str(player),
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
    let action =
        bqsrv_app::app::envops::action_for_coords(s.core.variant, &s.core.current, q.from, q.to)
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

fn find_archived(app: &SharedState, id: &str) -> Result<ArchivedGame, ApiError> {
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
    let game = find_archived(&app, &id)?;

    let detail = bqsrv_app::app::replay::to_detail(&game).map_err(ApiError::Internal)?;
    Ok(Json(detail))
}

pub async fn replay_recent_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<ReplayResp>, ApiError> {
    let game = find_archived(&app, &id)?;
    let resp = bqsrv_app::app::replay::build_replay(&game).map_err(ApiError::Internal)?;
    Ok(Json(resp))
}

pub async fn replay_as_new_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    req: Option<Json<ReplayGameReq>>,
) -> Result<Json<GameCreatedResp>, ApiError> {
    let game = find_archived(&app, &id)?;
    let req = req.map(|Json(r)| r).unwrap_or(ReplayGameReq {
        slots: None,
        from_ply: None,
    });
    let from_ply = req.from_ply.unwrap_or(0);

    let snap =
        bqsrv_app::app::replay::snapshot_at_ply(&game, from_ply).map_err(ApiError::Internal)?;
    let variant = snap.variant;
    let (slots, slot_labels) = build_slots(&app, req.slots.as_ref(), variant).await?;
    let new_id = Uuid::new_v4();
    let mut session = GameSession::from_snapshot(variant, snap, slots);
    session.slot_labels = slot_labels;
    let state = session.view(new_id);
    app.games
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(new_id, session);
    tracing::info!(
        archive = %id,
        game = %new_id,
        from_ply,
        "从最近对局归档复演建局: variant={}",
        variant.as_str()
    );
    Ok(Json(GameCreatedResp {
        game_id: new_id.to_string(),
        state,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use banqi_core::core::env::{DarkChessEnv, Player, SnapshotEnv, Variant};

    fn sample_fen(turn: char) -> String {
        use banqi_core::core::env::seed::SeedableEnv;
        let mut env = DarkChessEnv::with_config_and_scenario(Variant::DarkChess4x8.config(), None)
            .expect("默认建局应成功");
        SeedableEnv::set_seed(&mut env, 20261118);
        let mut snap = env.to_snapshot();
        snap.current_player = match turn {
            'b' => Player::Black,
            _ => Player::Red,
        };
        snap.to_fen()
    }

    #[test]
    fn fen_only_converts_to_from_fen_scenario() {
        let fen = sample_fen('b');
        let (scenario, variant) =
            resolve_fen_scenario(Some(&fen), None, None).expect("fen-only 应归一化为场景");
        let variant = variant.expect("from_fen 场景应派生变体");
        assert_eq!(variant, Variant::DarkChess4x8, "变体应从 FEN 派生");
        assert!(matches!(scenario, Some(OpeningScenario::FromFen(_))));

        let session = GameSession::new_with_scenario(
            variant,
            scenario.as_ref(),
            bqsrv_app::app::player::Slots::all_human(),
        )
        .expect("from_fen 场景建局应成功");
        assert_eq!(
            session.core.current.current_player,
            Player::Black,
            "建局后行棋方应与 FEN 一致"
        );
        assert!(
            !session.core.legal_actions().is_empty(),
            "from_fen 建局后应有合法动作"
        );
        assert_eq!(session.core.version, 0);
    }

    #[test]
    fn fen_with_non_from_fen_scenario_is_rejected() {
        let fen = sample_fen('r');
        let err =
            resolve_fen_scenario(Some(&fen), Some(OpeningScenario::with_general(5, 19)), None)
                .expect_err("fen + 非 from_fen scenario 应互斥报错");
        assert!(matches!(err, ApiError::BadRequest(_)), "实际 {err:?}");

        let (scenario, variant) =
            resolve_fen_scenario(None, Some(OpeningScenario::with_general(5, 19)), None)
                .expect("纯 scenario 不应报错");
        assert!(variant.is_none(), "非 from_fen 场景不应派生变体");
        assert!(matches!(
            scenario,
            Some(OpeningScenario::CannonStrikeGeneral(_))
        ));
    }

    #[test]
    fn from_fen_scenario_coexists_with_fen() {
        let fen = sample_fen('r');
        let sc = OpeningScenario::try_from_fen(&fen).expect("from_fen 场景应可构造");
        let (scenario, variant) = resolve_fen_scenario(Some(&fen), Some(sc), None)
            .expect("from_fen scenario + fen 应允许（以 scenario 为准）");
        assert!(matches!(scenario, Some(OpeningScenario::FromFen(_))));
        assert_eq!(variant, Some(Variant::DarkChess4x8));
    }

    #[test]
    fn fen_variant_mismatch_is_rejected() {
        let fen = sample_fen('r');
        let err = resolve_fen_scenario(Some(&fen), None, Some("4x2"))
            .expect_err("请求变体与 FEN 变体不一致应报错");
        assert!(
            matches!(err, ApiError::BadRequest(ref msg) if msg.contains("不一致")),
            "实际 {err:?}"
        );

        let long_fen = "x".repeat(banqi_core::core::env::scenario::FROM_FEN_MAX_BYTES + 1);
        let err = resolve_fen_scenario(Some(&long_fen), None, None).expect_err("超长 FEN 应报错");
        assert!(matches!(err, ApiError::BadRequest(_)), "实际 {err:?}");
    }
}
