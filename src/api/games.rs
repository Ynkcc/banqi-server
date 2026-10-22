//! 对局 REST handlers。全部持锁段为同步操作，不跨 await。
//!
//! 选手槽位协议：红/黑各自独立配置为 Human 或任意策略，建局与对局中
//! （`PUT /slots`）均可随时切换；模型类策略在此解析并预加载模型（坏文件即报错）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::ai;
use crate::api::parse_variant_or_err;
use crate::app::player::{PlayerSlot, StrategyKind};
use crate::app::sync::{read_lock, write_lock};
use banqi_core::core::env::PositionSnapshot;

use crate::dto::{
    AppliedAction, CreateGameReq, DryRunReq, GameCreatedResp, GameListResp, GameStateResp,
    MoveActionResp, MoveReq, MoveResp, PutSlotsReq, SuggestionResp, SlotReq, SlotsReq, UndoReq,
};
use crate::error::ApiError;
use crate::state::{get_mut_session, get_session, parse_game_id, GameSession, SharedState};

// 槽位构建（建局 / 对局中改槽位共用）

/// 解析单个槽位请求：模型类策略解析并预加载模型（坏文件 / 跨变体权重即报错），
/// Agent 槽位校验存在性与变体一致（策略实时读取），参数缺省字段取服务器默认值。
async fn build_slot(
    app: &SharedState,
    req: SlotReq,
    variant: banqi_core::core::env::Variant,
) -> Result<PlayerSlot, ApiError> {
    if req.kind.eq_ignore_ascii_case("human") {
        return Ok(PlayerSlot::Human);
    }
    if req.kind.eq_ignore_ascii_case("agent") {
        let raw = req.agent_id.as_deref().ok_or_else(|| {
            ApiError::BadRequest("kind = \"Agent\" 时须提供 agentId".to_string())
        })?;
        let id = Uuid::parse_str(raw.trim())
            .map_err(|_| ApiError::BadRequest(format!("非法 Agent ID: {raw}")))?;
        // 绑定即校验：存在 + 变体一致（策略/参数此后实时读取）
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
        // 建局/改槽位即加载 + 布局校验：坏模型 / 跨变体权重在此报错而非对局中途
        // （按后端预加载 + NNUE 布局校验收敛于 AppState::preload_engine_model）
        let f = app.resolve_model_file(req.model_file.as_deref(), variant)?;
        let engine = kind.engine_kind().expect("needs_model() 蕴含模型类策略");
        app.preload_engine_model(engine, f.clone(), variant).await?;
        Some(f)
    } else {
        None
    };
    Ok(PlayerSlot::strategy(kind, model_file, params))
}

/// 解析双方槽位请求（缺省方 = Human；双方缺省即原本地双人）。
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

// 创建 / 列表 / 查询 / 删除

/// POST /api/games
pub async fn create_game(
    State(app): State<SharedState>,
    Json(req): Json<CreateGameReq>,
) -> Result<Json<GameCreatedResp>, ApiError> {
    // 局面：BFEN 粘贴重建（变体以 BFEN 自带标识为准）> 按变体新建。
    // 显式同时给出 variant 时校验一致性，避免静默忽略请求方意图。
    let fen_snapshot = match req.fen.as_deref() {
        Some(fen) if !fen.trim().is_empty() => {
            let snap = PositionSnapshot::from_fen(fen.trim()).map_err(|e| {
                ApiError::BadRequest(format!("BFEN 局面解析失败: {e}"))
            })?;
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

    // 变体：BFEN 自带 > 请求指定 > 服务器默认（不硬编码）
    let variant = match fen_snapshot.as_ref() {
        Some(s) => s.variant,
        None => match req.variant.as_deref() {
            None => app.default_variant(),
            Some(s) => parse_variant_or_err(s)?,
        },
    };

    // 场景与 BFEN 互斥：BFEN 自带完整局面，场景只作用于初始局面的生成
    if req.scenario.is_some() && fen_snapshot.is_some() {
        return Err(ApiError::BadRequest(
            "scenario 与 fen 互斥：BFEN 已携带完整局面，请二选一".to_string(),
        ));
    }

    // 红/黑槽位：模型类策略在此解析并预加载模型
    let slots = build_slots(&app, req.slots.as_ref(), variant).await?;

    let game_id = Uuid::new_v4();
    let session = match fen_snapshot {
        Some(snap) => GameSession::from_snapshot(variant, snap, slots),
        None => {
            GameSession::new_with_scenario(variant, req.scenario.as_ref(), slots)?
        }
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

/// GET /api/games
pub async fn list_games(State(app): State<SharedState>) -> Json<GameListResp> {
    let games = read_lock(&app.games);
    let mut summaries: Vec<crate::dto::GameSummary> =
        games.iter().map(|(id, s)| s.summary(*id)).collect();
    summaries.sort_by(|a, b| a.game_id.cmp(&b.game_id));
    Json(GameListResp { games: summaries })
}

/// GET /api/games/:id
pub async fn get_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<GameStateResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let s = get_session(&games, &id, game_id)?;
    Ok(Json(s.view(game_id)))
}

/// DELETE /api/games/:id
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

// 走子 / 悔棋 / 策略单步 / dry-run

/// POST /api/games/:id/move：人类走子一步；若轮到策略方，自动执行其策略回合。
pub async fn move_piece(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<MoveReq>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;

    // 锁 1：人类走子（同步操作，立即释放，不跨 await）
    let (mut state, mover, human_view, strategy_version) = {
        let mut games = write_lock(&app.games);
        let s = get_mut_session(&mut games, &id, game_id)?;
        let mover = s.core.current.current_player;
        // 先走子（内部校验合法性/终局），避免用越界 action 查环境导致 panic
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

    // 锁外：对方策略回合（如轮到）
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
                // 策略结果作废（期间对局被并发修改）：返回当前实际状态
                let games = read_lock(&app.games);
                if let Some(s) = games.get(&game_id) {
                    state = s.view(game_id);
                }
            }
        }
    }

    Ok(Json(MoveResp::new(state, actions, mcts_stats, nnue_stats)))
}

/// POST /api/games/:id/run
///
/// 单步推进：当前行棋方按其槽位策略执行一步（Human 方返回 409 HUMAN_TURN）。
/// 请求体可空；`{ agentId }` 指定 Agent 时以其当前配置（实时参数）为当前
/// 行棋方执行一步——单步永远作用于当前行动方，与该 Agent 托管哪方无关。
/// auto 模式由前端循环调用本端点实现（每步后检查终局/暂停/槽位变更）。
pub async fn run(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    req: Option<Json<crate::dto::RunReq>>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let agent_id = req.and_then(|Json(r)| r.agent_id).filter(|s| !s.trim().is_empty());

    // 锁 1：读取当前版本号 + Agent 单步时解析 override 槽位（立即释放）
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
        Some((state, action, mcts_stats, nnue_stats)) => {
            Ok(Json(MoveResp::new(state, vec![action], mcts_stats, nnue_stats)))
        }
        None => Err(ApiError::BadRequest(
            "策略结果作废（期间对局被并发修改），请重试".to_string(),
        )),
    }
}

/// POST /api/games/:id/dry-run
///
/// 为当前行棋方请求策略建议走法（不落子）：托管方预览 AI 将走的棋，
/// 人类方作提示（kind 缺省 Expectimax）。前端据 `version` 丢弃过期建议。
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

/// PUT /api/games/:id/slots
///
/// 对局中切换任一方槽位（只更新给出的方；模型类策略预加载校验后才写入）。
/// 替代旧 `PUT /model` 换绑语义：策略种类与模型在同一槽位内一并指定。
pub async fn put_slots(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<PutSlotsReq>,
) -> Result<Json<GameStateResp>, ApiError> {
    let game_id = parse_game_id(&id)?;

    // 锁 1：取出变体（模型解析/预加载用，立即释放）
    let variant = {
        let games = read_lock(&app.games);
        let s = get_session(&games, &id, game_id)?;
        s.core.variant
    };

    // 锁外构建并预加载（blocking，可能耗时；坏文件在此报错，不改动现有槽位）
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

    // 锁 2：写入槽位
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

// 坐标→动作 / 悔棋

/// GET /api/games/:id/move-action?from=12&to=13：坐标对 → 动作空间索引
/// （几何映射，只取决于变体；`null` = 坐标不构成任何动作）。
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
    // 几何映射只取决于变体 config：from == to → 翻棋单格动作，否则查移动/炮击
    let action = if q.from == q.to {
        let tables =
            banqi_core::core::env::actions::action_lookup_tables(&s.core.variant.config());
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

/// POST /api/games/:id/undo
pub async fn undo(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<UndoReq>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;

    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &id, game_id)?;
    // 缺省语义：回退到当前行棋方上次行动前（策略托管回合自动一并回退）
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
    // 返回 StepResult 形状（与走子 / 策略回合一致），actions 为空（无悔棋动作）
    Ok(Json(MoveResp::new(s.view(game_id), Vec::new(), None, None)))
}
