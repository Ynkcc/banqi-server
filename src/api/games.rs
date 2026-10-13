//! 对局 REST handlers。全部持锁段为同步操作，不跨 await。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use uuid::Uuid;

use banqi_core::core::env::types::Player;

use crate::ai;
use crate::config::parse_variant;
use crate::dto::{
    AppliedAction, CreateGameReq, GameCreatedResp, GameListResp, GameStateResp, GameSummary,
    MoveReq, MoveResp, UndoReq, player_str,
};
use crate::error::ApiError;
use crate::state::{AiConfig, GameSession, SharedState};

/// 解析路径中的对局 ID。
fn parse_game_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| ApiError::BadRequest(format!("非法对局 ID: {raw}")))
}

// ============================================================================
// 创建 / 列表 / 查询 / 删除
// ============================================================================

/// POST /api/games
pub async fn create_game(
    State(app): State<SharedState>,
    Json(req): Json<CreateGameReq>,
) -> Result<Json<GameCreatedResp>, ApiError> {
    // 变体：请求指定 > 服务器默认（不硬编码）
    let variant = match req.variant.as_deref() {
        None => app.default_variant(),
        Some(s) => parse_variant(s).ok_or_else(|| {
            use banqi_core::core::env::Variant;
            ApiError::BadRequest(format!(
                "未知变体: {s}（可用: {}）",
                Variant::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
            ))
        })?,
    };

    let ai_enabled = req.ai_enabled.unwrap_or(true);
    let ai = if ai_enabled {
        if !app.models.available(variant) {
            return Err(ApiError::ModelUnavailable(format!(
                "变体 {} 无已加载模型，无法启用 AI（检查 models/ 目录命名：<变体ID>.onnx）",
                variant.as_str()
            )));
        }
        let player = match req.ai_player.as_deref() {
            None | Some("auto") => Player::Black, // 默认 AI 执黑，人类执红先行
            Some("red") => Player::Red,
            Some("black") => Player::Black,
            Some(other) => {
                return Err(ApiError::BadRequest(format!(
                    "非法 aiPlayer: {other}（可选 red / black / auto）"
                )));
            }
        };
        Some(AiConfig {
            player,
            mcts_sims: req.mcts_sims.unwrap_or_else(|| app.default_sims()).max(1),
        })
    } else {
        None
    };

    let game_id = Uuid::new_v4();
    let session = GameSession::new(variant, ai);
    let state = session.view(game_id);

    app.games
        .write()
        .expect("games 锁中毒")
        .insert(game_id, session);
    tracing::info!(game = %game_id, "创建对局: variant={}，ai={:?}", variant.as_str(), ai.is_some());

    Ok(Json(GameCreatedResp {
        game_id: game_id.to_string(),
        state,
    }))
}

/// GET /api/games
pub async fn list_games(State(app): State<SharedState>) -> Json<GameListResp> {
    let games = app.games.read().expect("games 锁中毒");
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
    let games = app.games.read().expect("games 锁中毒");
    let s = games
        .get(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
    Ok(Json(s.view(game_id)))
}

/// DELETE /api/games/:id
pub async fn delete_game(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let game_id = parse_game_id(&id)?;
    let removed = app
        .games
        .write()
        .expect("games 锁中毒")
        .remove(&game_id)
        .is_some();
    if removed {
        tracing::info!(game = %game_id, "删除对局");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::GameNotFound(id))
    }
}

// ============================================================================
// 走子 / 悔棋 / AI 走子
// ============================================================================

/// POST /api/games/:id/move
///
/// 人类走子一步；若启用 AI 且轮到 AI，自动执行 AI 回合并一并返回。
pub async fn move_piece(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<MoveReq>,
) -> Result<Json<MoveResp>, ApiError> {
    let game_id = parse_game_id(&id)?;

    // 锁 1：人类走子（同步操作，立即释放，不跨 await）
    let (mut state, mover, human_view, ai_version) = {
        let mut games = app.games.write().expect("games 锁中毒");
        let s = games
            .get_mut(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
        let mover = s.current.current_player;
        // 先走子（内部校验合法性/终局），再生成动作视图——
        // 非法动作直接 4xx，避免用越界 action 去查环境（core 内按索引访问会 panic）
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
        let ai_version = state.ai_to_move.then_some(s.version);
        (state, mover, human_view, ai_version)
    };

    // 锁外：AI 回合（如轮到）
    let mut actions = vec![AppliedAction {
        player: player_str(mover),
        action: human_view,
    }];
    let mut mcts_stats = None;
    if let Some(version) = ai_version {
        match ai::run_ai_turn(&app, game_id, version).await? {
            Some((new_state, ai_action, stats)) => {
                actions.push(ai_action);
                mcts_stats = stats;
                state = new_state;
            }
            None => {
                // AI 结果作废（期间对局被并发修改）：返回当前实际状态
                let games = app.games.read().expect("games 锁中毒");
                if let Some(s) = games.get(&game_id) {
                    state = s.view(game_id);
                }
            }
        }
    }

    Ok(Json(MoveResp { state, actions, mcts_stats }))
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
        let games = app.games.read().expect("games 锁中毒");
        let s = games
            .get(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
        s.version
    };

    match ai::run_ai_turn(&app, game_id, version).await? {
        Some((state, ai_action, mcts_stats)) => Ok(Json(MoveResp {
            state,
            actions: vec![ai_action],
            mcts_stats,
        })),
        None => Err(ApiError::BadRequest(
            "AI 结果作废（期间对局被并发修改），请重试".to_string(),
        )),
    }
}

/// POST /api/games/:id/undo
pub async fn undo(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<UndoReq>,
) -> Result<Json<GameStateResp>, ApiError> {
    let game_id = parse_game_id(&id)?;

    let mut games = app.games.write().expect("games 锁中毒");
    let s = games
        .get_mut(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
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
    Ok(Json(s.view(game_id)))
}
