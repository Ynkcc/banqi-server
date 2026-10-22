//! MCTS 搜索树查询 handlers（树由 AI 回合 / 手动搜索保留在会话中）。
//!
//! 契约见 docs/backend-api.md §5-①；DTO 与 banqi-gui `mcts_view.rs` 同构，
//! 前端 types.ts（snake_case）直传，client 适配层零改动。

use axum::extract::{Path, State};
use axum::Json;

use crate::app::sync::{read_lock, write_lock};
use crate::error::ApiError;
use crate::mcts::{MctsEdge, MctsNodeBoard, MctsNodeDetail, MctsRootInfo};
use crate::state::{get_mut_session, get_session, mcts_tree_of, parse_game_id, SharedState};

/// 解析路径中的节点 ID。
fn parse_node_id(raw: &str) -> Result<usize, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::BadRequest(format!("非法节点 ID: {raw}")))
}

/// GET /api/games/:id/mcts/root
pub async fn root(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<MctsRootInfo>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let tree = mcts_tree_of(get_session(&games, &id, game_id)?)?;
    Ok(Json(tree.root_info()))
}

/// GET /api/games/:id/mcts/node/:node/children
pub async fn children(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<Vec<MctsEdge>>, ApiError> {
    let node_id = parse_node_id(&node)?;
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let tree = mcts_tree_of(get_session(&games, &id, game_id)?)?;
    Ok(Json(tree.children(node_id)))
}

/// GET /api/games/:id/mcts/node/:node
pub async fn detail(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<MctsNodeDetail>, ApiError> {
    let node_id = parse_node_id(&node)?;
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let tree = mcts_tree_of(get_session(&games, &id, game_id)?)?;
    tree.detail(node_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("节点 {node_id} 不存在（超出树范围）")))
}

/// GET /api/games/:id/mcts/node/:node/board
pub async fn board(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<MctsNodeBoard>, ApiError> {
    let node_id = parse_node_id(&node)?;
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let tree = mcts_tree_of(get_session(&games, &id, game_id)?)?;
    tree.board(node_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("节点 {node_id} 不存在或未保存局面")))
}

/// POST /api/games/:id/mcts/search
///
/// 在当前局面上手动触发一次 MCTS 搜索（不走子），替换会话中的浏览树。
/// 推理后端分派与 feature 门控在 `ai::choose_mcts_by_engine` 单点维护；
/// 上一棵树沿「AI 上手 → 对手上手」尝试复用。
pub async fn search(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<MctsRootInfo>, ApiError> {
    let game_id = parse_game_id(&id)?;

    // 锁 1：读取搜索所需数据并取出上一棵树供复用（立即释放，不跨 await）
    let (snap, sims, model_file, prev_tree) = {
        let mut games = write_lock(&app.games);
        let s = get_mut_session(&mut games, &id, game_id)?;
        let sims = s
            .opponent
            .as_ref()
            .map(|o| o.mcts_sims)
            .unwrap_or_else(|| app.default_sims());
        let model_file = s.model_file.clone().ok_or_else(|| {
            ApiError::BadRequest(
                "对局未绑定模型文件，无法执行搜索（创建对局时指定 modelFile 或 PUT /api/games/:id/model 绑定）"
                    .into(),
            )
        })?;
        let prev_tree = s.take_reusable_mcts_tree();
        (s.core.current.clone(), sims, model_file, prev_tree)
    };

    // 锁外取对局绑定的模型（随用随加载：缓存命中直接返回，首次使用时加载），
    // 按扩展名分派推理后端（单一真源：choose_mcts_by_engine）
    let (_action, tree) =
        crate::ai::choose_mcts_by_engine(&app, model_file, snap, sims, prev_tree).await?;
    let info = tree.root_info();

    let mut games = write_lock(&app.games);
    if let Some(s) = games.get_mut(&game_id) {
        s.mcts_tree = Some(tree);
    }
    tracing::info!(game = %game_id, "手动 MCTS 搜索完成");
    Ok(Json(info))
}
