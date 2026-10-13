//! MCTS 搜索树查询 handlers（树由 AI 回合 / 手动搜索保留在会话中）。
//!
//! 契约见 docs/backend-api.md §5-①；DTO 与 banqi-gui `mcts_view.rs` 同构，
//! 前端 types.ts（snake_case）直传，client 适配层零改动。

use axum::extract::{Path, State};
use axum::Json;
use uuid::Uuid;

use crate::error::ApiError;
use crate::mcts::{MctsEdge, MctsNodeBoard, MctsNodeDetail, MctsRootInfo};
use crate::state::SharedState;

fn parse_ids(id: String, node: String) -> Result<(Uuid, usize), ApiError> {
    let game_id = Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest(format!("非法对局 ID: {id}")))?;
    let node_id: usize =
        node.parse().map_err(|_| ApiError::BadRequest(format!("非法节点 ID: {node}")))?;
    Ok((game_id, node_id))
}

/// GET /api/games/:id/mcts/root
pub async fn root(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<MctsRootInfo>, ApiError> {
    let game_id = Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest(format!("非法对局 ID: {id}")))?;
    let games = app.games.read().expect("games 锁中毒");
    let s = games
        .get(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
    let tree = s.mcts_tree.as_ref().ok_or(ApiError::MctsTreeMissing)?;
    Ok(Json(tree.root_info()))
}

/// GET /api/games/:id/mcts/node/:node/children
pub async fn children(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<Vec<MctsEdge>>, ApiError> {
    let (game_id, node_id) = parse_ids(id, node)?;
    let games = app.games.read().expect("games 锁中毒");
    let s = games.get(&game_id).ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let tree = s.mcts_tree.as_ref().ok_or(ApiError::MctsTreeMissing)?;
    Ok(Json(tree.children(node_id)))
}

/// GET /api/games/:id/mcts/node/:node
pub async fn detail(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<MctsNodeDetail>, ApiError> {
    let (game_id, node_id) = parse_ids(id, node)?;
    let games = app.games.read().expect("games 锁中毒");
    let s = games.get(&game_id).ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let tree = s.mcts_tree.as_ref().ok_or(ApiError::MctsTreeMissing)?;
    tree.detail(node_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("节点 {node_id} 不存在（超出树范围）")))
}

/// GET /api/games/:id/mcts/node/:node/board
pub async fn board(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<MctsNodeBoard>, ApiError> {
    let (game_id, node_id) = parse_ids(id, node)?;
    let games = app.games.read().expect("games 锁中毒");
    let s = games.get(&game_id).ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let tree = s.mcts_tree.as_ref().ok_or(ApiError::MctsTreeMissing)?;
    tree.board(node_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("节点 {node_id} 不存在或未保存局面")))
}

/// POST /api/games/:id/mcts/search
///
/// 在当前局面上手动触发一次 MCTS 搜索（不走子），替换会话中的浏览树。
pub async fn search(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<MctsRootInfo>, ApiError> {
    let game_id = Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest(format!("非法对局 ID: {id}")))?;

    // 锁 1：读取搜索所需数据（立即释放，不跨 await）
    let (variant, snap, model, sims) = {
        let games = app.games.read().expect("games 锁中毒");
        let s = games
            .get(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
        let sims = s.ai.as_ref().map(|a| a.mcts_sims).unwrap_or(app.config.default_sims);
        let model = app
            .models
            .get(s.variant)
            .cloned()
            .ok_or_else(|| ApiError::ModelUnavailable(format!("变体 {} 无已加载模型", s.variant.as_str())))?;
        (s.variant, s.current.clone(), model, sims)
    };

    let (_action, tree) = crate::ai::choose_blocking(variant, snap, model, sims)
        .await
        .map_err(ApiError::Internal)?
        .ok_or_else(|| ApiError::Internal("MCTS 未返回动作".to_string()))?;
    let info = tree.root_info();

    let mut games = app.games.write().expect("games 锁中毒");
    if let Some(s) = games.get_mut(&game_id) {
        s.mcts_tree = Some(tree);
    }
    tracing::info!(game = %game_id, "手动 MCTS 搜索完成");
    Ok(Json(info))
}
