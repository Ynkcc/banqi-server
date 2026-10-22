//! 对局 REST handlers。全部持锁段为同步操作，不跨 await。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::app::opponent::OpponentKind;
use crate::app::sync::{read_lock, write_lock};
use banqi_core::core::env::types::Player;
use banqi_core::core::env::PositionSnapshot;

use crate::ai;
use crate::api::parse_variant_or_err;
use crate::dto::{
    AppliedAction, CreateGameReq, GameCreatedResp, GameListResp, GameStateResp, GameSummary,
    ModelBindReq, MoveActionResp, MoveReq, MoveResp, OpponentResp, UndoReq, player_str,
};
use crate::error::ApiError;
use crate::state::{
    get_mut_session, get_session, parse_game_id, EngineKind, GameSession, OpponentConfig,
    SharedState,
};

/// 解析 AI 执子方（"auto" = AI 执黑，人类执红先行）。
fn parse_ai_player(s: Option<&str>) -> Result<Player, ApiError> {
    match s {
        None | Some("auto") => Ok(Player::Black),
        Some("red") => Ok(Player::Red),
        Some("black") => Ok(Player::Black),
        Some(other) => Err(ApiError::BadRequest(format!(
            "非法 aiPlayer: {other}（可选 red / black / auto）"
        ))),
    }
}

/// 按对手种类构建配置：模型类对手在此解析并预加载绑定模型（坏文件即报错）；
/// 规则 / Engine 对手无模型。`model_file` 为调用方已解析的绑定模型文件
/// （None = 内部按请求字段 + 服务器默认值解析）。返回 `(对手配置, 绑定的模型文件)`。
async fn build_opponent(
    app: &SharedState,
    kind: OpponentKind,
    req: &CreateGameReq,
    variant: banqi_core::core::env::Variant,
    model_file: Option<String>,
) -> Result<(OpponentConfig, Option<String>), ApiError> {
    let model_file = if kind.needs_model() {
        let f = match model_file {
            Some(f) => f,
            None => app.resolve_model_file(req.model_file.as_deref(), variant)?,
        };
        // 建局即加载 + 布局校验：坏模型 / 跨变体权重在建局时报错而非对局中途
        // （按后端预加载 + NNUE 布局校验收敛于 AppState::preload_engine_model）
        let engine = kind.engine_kind().expect("needs_model() 蕴含模型类对手");
        app.preload_engine_model(engine, f.clone(), variant).await?;
        Some(f)
    } else {
        None
    };
    Ok((
        OpponentConfig {
            player: parse_ai_player(req.ai_player.as_deref())?,
            kind,
            // 建局时快照固定；未显式指定取服务器当前默认值
            mcts_sims: req.mcts_sims.unwrap_or_else(|| app.default_sims()).max(1),
            engine_budget: req
                .engine_budget
                .unwrap_or_else(|| app.default_engine_budget())
                .max(1),
            search_budget: app.build_search_budget(
                req.nnue_budget_mode.as_deref(),
                req.nnue_budget,
                req.nnue_depth,
                req.nnue_time_ms,
                req.search_timeout_ms,
            )?,
        },
        model_file,
    ))
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

    // 对手解析：显式 opponent 字段 > 传统 aiEnabled 语义（缺省 true → 模型对手）。
    // 模型类对手（Nnue / MctsOnnx）在此解析并预加载模型；规则 / Engine 对手无模型。
    let requested = req.opponent.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (opponent, model_file): (Option<OpponentConfig>, Option<String>) = match requested {
        Some(s) if s.eq_ignore_ascii_case("pvp") => {
            if req.ai_enabled.is_some() {
                return Err(ApiError::BadRequest(
                    "opponent 与 aiEnabled 不能同时指定（opponent 显式给出时以 opponent 为准）".into(),
                ));
            }
            (None, None)
        }
        Some(s) => {
            let kind = OpponentKind::parse(s).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "非法 opponent: {s}（可选 PvP / Random / RevealFirst / CaptureFirst / Engine / Nnue / MctsOnnx / MctsDL）"
                ))
            })?;
            let (opp, model) = build_opponent(&app, kind, &req, variant, None).await?;
            (Some(opp), model)
        }
        None => {
            if req.ai_enabled.unwrap_or(true) {
                // 传统路径：模型文件决定推理后端（`.nnue` → Nnue，`.pt` → MctsDL，
                // 其余 → MctsOnnx）；解析结果直接传入 build_opponent，不重复解析
                let model_file = app.resolve_model_file(req.model_file.as_deref(), variant)?;
                let kind = OpponentKind::from_engine_kind(EngineKind::from_model_file(&model_file));
                let (opp, model) = build_opponent(&app, kind, &req, variant, Some(model_file)).await?;
                (Some(opp), model)
            } else {
                (None, None)
            }
        }
    };

    let game_id = Uuid::new_v4();
    let session = match fen_snapshot {
        Some(snap) => GameSession::from_snapshot(variant, snap, opponent, model_file),
        None => GameSession::new(variant, opponent, model_file),
    };
    let state = session.view(game_id);

    app.games
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(game_id, session);
    tracing::info!(
        game = %game_id,
        "创建对局: variant={}，opponent={:?}，model={:?}",
        variant.as_str(),
        opponent.map(|o| o.kind.as_str()),
        req.model_file
    );

    Ok(Json(GameCreatedResp {
        game_id: game_id.to_string(),
        state,
    }))
}

/// GET /api/games
pub async fn list_games(State(app): State<SharedState>) -> Json<GameListResp> {
    let games = read_lock(&app.games);
    let mut summaries: Vec<GameSummary> =
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

// 走子 / 悔棋 / AI 走子

/// POST /api/games/:id/move：人类走子一步；若启用 AI 且轮到 AI，自动执行 AI 回合。
pub async fn move_piece(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<MoveReq>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;

    // 锁 1：人类走子（同步操作，立即释放，不跨 await）
    let (mut state, mover, human_view, ai_version) = {
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
        let ai_version = state.ai_to_move.then_some(s.core.version);
        (state, mover, human_view, ai_version)
    };

    // 锁外：AI 回合（如轮到）
    let mut actions = vec![AppliedAction {
        player: player_str(mover),
        action: human_view,
    }];
    let mut mcts_stats = None;
    let mut nnue_stats = None;
    if let Some(version) = ai_version {
        match ai::run_opponent_turn(&app, game_id, version).await? {
            Some((new_state, ai_action, m, n)) => {
                actions.push(ai_action);
                mcts_stats = m;
                nnue_stats = n;
                state = new_state;
            }
            None => {
                // AI 结果作废（期间对局被并发修改）：返回当前实际状态
                let games = read_lock(&app.games);
                if let Some(s) = games.get(&game_id) {
                    state = s.view(game_id);
                }
            }
        }
    }

    Ok(Json(MoveResp::new(state, actions, mcts_stats, nnue_stats)))
}

/// POST /api/games/:id/ai-move
///
/// 强制 AI 走子（人类代 AI 触发，如 AI 执先手时开局）。
pub async fn ai_move(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;

    // 锁 1：读取当前版本号（立即释放）
    let version = {
        let games = read_lock(&app.games);
        let s = get_session(&games, &id, game_id)?;
        s.core.version
    };

    match ai::run_opponent_turn(&app, game_id, version).await? {
        Some((state, ai_action, mcts_stats, nnue_stats)) => {
            Ok(Json(MoveResp::new(state, vec![ai_action], mcts_stats, nnue_stats)))
        }
        None => Err(ApiError::BadRequest(
            "AI 结果作废（期间对局被并发修改），请重试".to_string(),
        )),
    }
}

/// PUT /api/games/:id/model
///
/// 为进行中的对局重新绑定（并预加载）用户选择的模型文件。
/// 推理后端由扩展名决定（`.nnue` → Expectimax+NNUE，其余 → MCTS+ONNX），
/// 换绑时同步切换；此后该对局的 AI 回合均使用该模型。
pub async fn bind_model(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<ModelBindReq>,
) -> Result<Json<GameStateResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let engine = EngineKind::from_model_file(&req.model_file);

    // 锁 1：校验已启用对手并取出变体（NNUE 布局校验用，立即释放）
    let variant = {
        let games = read_lock(&app.games);
        let s = get_session(&games, &id, game_id)?;
        s.opponent.as_ref().ok_or(ApiError::AiDisabled)?;
        s.core.variant
    };

    // 锁外预加载（blocking 线程池，可能耗时；坏文件在此报错；
    // 按后端分派 + NNUE 布局校验收敛于 AppState::preload_engine_model）
    app.preload_engine_model(engine, req.model_file.clone(), variant)
        .await?;

    // 锁 2：写入绑定（换绑模型时同步切换对手种类：PvP 之外的规则 / Engine
    // 对手在绑定模型后转为对应的模型类对手）
    let mut games = write_lock(&app.games);
    let s = get_mut_session(&mut games, &id, game_id)?;
    s.model_file = Some(req.model_file.clone());
    if let Some(opp) = s.opponent.as_mut() {
        opp.kind = OpponentKind::from_engine_kind(engine);
    }
    tracing::info!(game = %game_id, engine = ?engine, "对局重绑模型: {}", req.model_file);
    Ok(Json(s.view(game_id)))
}

// 对手查询 / 坐标→动作

/// GET /api/games/:id/opponent：当前对局的对手种类（PvP 对局返回 "PvP"）。
pub async fn get_opponent(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<OpponentResp>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let s = get_session(&games, &id, game_id)?;
    let opponent = s
        .opponent
        .map(|o| o.kind.as_str().to_string())
        .unwrap_or_else(|| "PvP".to_string());
    Ok(Json(OpponentResp { opponent }))
}

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
    use banqi_core::core::env::SnapshotEnv;

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
    // 缺省语义：回退到当前行棋方上次行动前（AI 对局中自动含 AI 回合）
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
    // 返回 StepResult 形状（与走子 / AI 回合一致），actions 为空（无悔棋动作）
    Ok(Json(MoveResp::new(s.view(game_id), Vec::new(), None, None)))
}
