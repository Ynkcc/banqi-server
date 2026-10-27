use crate::app::sync::read_lock;
use crate::error::ApiError;
use crate::nnue::{
    ExEdge, ExNodeBoard, ExNodeSummary, ExRootInfo, ex_edge_list, ex_node_board, ex_node_summary,
    ex_root_info,
};
use crate::state::{SharedState, parse_game_id, parse_id};
use axum::Json;
use axum::extract::{Path, State};
pub async fn root(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<ExRootInfo>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let s = games
        .get(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
    let kept = s
        .expectimax_tree
        .as_ref()
        .ok_or(ApiError::NnueTreeMissing)?;
    ex_root_info(kept)
        .ok_or(ApiError::NnueTreeMissing)
        .map(Json)
}
pub async fn children(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<Vec<ExEdge>>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let node_id: usize = parse_id(&node, "节点 ID")?;
    let games = read_lock(&app.games);
    let s = games
        .get(&game_id)
        .ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let kept = s
        .expectimax_tree
        .as_ref()
        .ok_or(ApiError::NnueTreeMissing)?;
    if node_id >= kept.tree.nodes.len() {
        return Err(ApiError::BadRequest(format!(
            "节点 {node_id} 不存在（超出树范围）"
        )));
    }
    Ok(Json(ex_edge_list(&kept.tree, node_id)))
}
pub async fn detail(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<ExNodeSummary>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let node_id: usize = parse_id(&node, "节点 ID")?;
    let games = read_lock(&app.games);
    let s = games
        .get(&game_id)
        .ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let kept = s
        .expectimax_tree
        .as_ref()
        .ok_or(ApiError::NnueTreeMissing)?;
    ex_node_summary(&kept.tree, node_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("节点 {node_id} 不存在（超出树范围）")))
}
pub async fn board(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<ExNodeBoard>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let node_id: usize = parse_id(&node, "节点 ID")?;
    let games = read_lock(&app.games);
    let s = games
        .get(&game_id)
        .ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let kept = s
        .expectimax_tree
        .as_ref()
        .ok_or(ApiError::NnueTreeMissing)?;
    ex_node_board(&kept.tree, node_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("节点 {node_id} 不存在或未保存局面")))
}
pub async fn search(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<ExRootInfo>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let info = crate::ai::manual_expectimax_search(&app, game_id, &id).await?;
    Ok(Json(info))
}
