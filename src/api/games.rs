//! 对局 REST handlers。全部持锁段为同步操作，不跨 await。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use uuid::Uuid;

use banqi_core::core::env::types::Player;
use banqi_core::core::env::PositionSnapshot;

use banqi_app::sync::{read_lock, write_lock};

use crate::ai;
use crate::config::parse_variant;
use crate::dto::{
    AppliedAction, CreateGameReq, GameCreatedResp, GameListResp, GameStateResp, GameSummary,
    ModelBindReq, MoveReq, MoveResp, UndoReq, player_str,
};
use crate::error::ApiError;
use crate::state::{AiConfig, EngineKind, GameSession, SharedState};

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
    // 局面：BFEN 粘贴重建（变体以 BFEN 自带标识为准）> 按变体新建。
    // 显式同时给出 variant 时校验一致性，避免静默忽略请求方意图。
    let fen_snapshot = match req.fen.as_deref() {
        Some(fen) if !fen.trim().is_empty() => {
            let snap = PositionSnapshot::from_fen(fen.trim()).map_err(|e| {
                ApiError::BadRequest(format!("BFEN 局面解析失败: {e}"))
            })?;
            if let Some(s) = req.variant.as_deref() {
                let want = parse_variant(s).ok_or_else(|| {
                    use banqi_core::core::env::Variant;
                    ApiError::BadRequest(format!(
                        "未知变体: {s}（可用: {}）",
                        Variant::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
                    ))
                })?;
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
            Some(s) => parse_variant(s).ok_or_else(|| {
                use banqi_core::core::env::Variant;
                ApiError::BadRequest(format!(
                    "未知变体: {s}（可用: {}）",
                    Variant::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
                ))
            })?,
        },
    };

    let ai_enabled = req.ai_enabled.unwrap_or(true);
    // 模型加载与对局绑定：AI 对局须绑定模型文件（用户显式指定；缺省按变体 ID
    // 自动匹配唯一候选）。推理后端由扩展名决定：`.nnue` → Expectimax+NNUE，
    // 其余 → MCTS+ONNX。建局即加载（随用随加载），坏文件在此报错。
    let model_file = if ai_enabled {
        match req.model_file.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(f) => Some(f.to_string()),
            None => Some(app.auto_match_model(variant)?),
        }
    } else {
        None
    };
    let engine = EngineKind::from_model_file(model_file.as_deref().expect("AI 对局必有模型文件"));
    let ai = if ai_enabled {
        let model_file_owned = model_file.clone().expect("AI 对局必有模型文件");
        match engine {
            EngineKind::Nnue => {
                // 建局即加载 + 布局校验：坏模型 / 跨变体权重在建局时报错而非对局中途
                app.acquire_nnue_validated(model_file_owned, variant)
                    .await
                    .map_err(|e| ApiError::ModelUnavailable(format!("模型加载失败: {e}")))?;
            }
            EngineKind::MctsOnnx => {
                #[cfg(feature = "onnx")]
                app.acquire_model(model_file_owned)
                    .await
                    .map_err(|e| ApiError::ModelUnavailable(format!("模型加载失败: {e}")))?;
                #[cfg(not(feature = "onnx"))]
                return Err(ApiError::ModelUnavailable(
                    "服务编译未启用 onnx 推理，MCTS+ONNX 对手不可用（请选择 .nnue 模型）".into(),
                ));
            }
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
            engine,
            // 建局时快照固定；未显式指定取服务器当前默认值
            mcts_sims: req.mcts_sims.unwrap_or_else(|| app.default_sims()).max(1),
            nnue_depth: req.nnue_depth.unwrap_or_else(|| app.default_nnue_depth()).max(1),
            nnue_budget: req.nnue_budget.unwrap_or_else(|| app.default_nnue_budget()).max(1),
        })
    } else {
        None
    };

    let game_id = Uuid::new_v4();
    let session = match fen_snapshot {
        Some(snap) => GameSession::from_snapshot(variant, snap, ai, model_file),
        None => GameSession::new(variant, ai, model_file),
    };
    let state = session.view(game_id);

    app.games
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(game_id, session);
    tracing::info!(
        game = %game_id,
        "创建对局: variant={}，ai={:?}，model={:?}",
        variant.as_str(),
        ai.is_some(),
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
    let removed = write_lock(&app.games).remove(&game_id).is_some();
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
        let mut games = write_lock(&app.games);
        let s = games
            .get_mut(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
        let mover = s.core.current.current_player;
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
        match ai::run_ai_turn(&app, game_id, version).await? {
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

    Ok(Json(MoveResp { state, actions, mcts_stats, nnue_stats }))
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
        let s = games
            .get(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
        s.core.version
    };

    match ai::run_ai_turn(&app, game_id, version).await? {
        Some((state, ai_action, mcts_stats, nnue_stats)) => Ok(Json(MoveResp {
            state,
            actions: vec![ai_action],
            mcts_stats,
            nnue_stats,
        })),
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

    // 锁 1：校验 AI 已启用并取出变体（NNUE 布局校验用，立即释放）
    let variant = {
        let games = read_lock(&app.games);
        let s = games
            .get(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
        s.ai.as_ref().ok_or(ApiError::AiDisabled)?;
        s.core.variant
    };

    // 锁外预加载（blocking 线程池，可能耗时；坏文件在此报错）
    match engine {
        EngineKind::Nnue => {
            let evaluator = app
                .acquire_nnue_model(req.model_file.clone())
                .await
                .map_err(|e| {
                    ApiError::ModelUnavailable(format!("模型 {} 绑定失败: {e}", req.model_file))
                })?;
            evaluator.validate_layout(&variant.config()).map_err(|e| {
                ApiError::ModelUnavailable(format!(".nnue 权重与变体 {} 不匹配: {e}", variant.as_str()))
            })?;
        }
        EngineKind::MctsOnnx => {
            #[cfg(feature = "onnx")]
            app.acquire_model(req.model_file.clone())
                .await
                .map_err(|e| {
                    ApiError::ModelUnavailable(format!("模型 {} 绑定失败: {e}", req.model_file))
                })?;
            #[cfg(not(feature = "onnx"))]
            return Err(ApiError::ModelUnavailable(
                "服务编译未启用 onnx 推理，MCTS+ONNX 模型不可用（请绑定 .nnue 模型）".into(),
            ));
        }
    }

    // 锁 2：写入绑定
    let mut games = write_lock(&app.games);
    let s = games
        .get_mut(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
    s.model_file = Some(req.model_file.clone());
    if let Some(ai) = s.ai.as_mut() {
        ai.engine = engine;
    }
    tracing::info!(game = %game_id, engine = ?engine, "对局重绑模型: {}", req.model_file);
    Ok(Json(s.view(game_id)))
}

/// POST /api/games/:id/undo
pub async fn undo(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<UndoReq>,
) -> Result<Json<GameStateResp>, ApiError> {
    let game_id = parse_game_id(&id)?;

    let mut games = write_lock(&app.games);
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
