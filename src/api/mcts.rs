use crate::app::sync::{read_lock, write_lock};
use crate::error::ApiError;
use crate::mcts::{MctsEdge, MctsNodeBoard, MctsNodeDetail, MctsRootInfo};
use crate::state::{
    SharedState, get_mut_session, get_session, mcts_tree_of, parse_game_id, parse_id,
    store_manual_search_trees,
};
use axum::Json;
use axum::extract::{Path, State};
fn parse_node_id(raw: &str) -> Result<usize, ApiError> {
    parse_id(raw, "节点 ID")
}
pub async fn root(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<MctsRootInfo>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let games = read_lock(&app.games);
    let tree = mcts_tree_of(get_session(&games, &id, game_id)?)?;
    Ok(Json(tree.root_info()))
}
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
pub async fn search(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<MctsRootInfo>, ApiError> {
    let game_id = parse_game_id(&id)?;
    let (snap, slot, prev_tree, version) = {
        let mut games = write_lock(&app.games);
        let s = get_mut_session(&mut games, &id, game_id)?;
        let current = s.core.current.current_player;
        let slot = match s.slots.side(current) {
            crate::app::player::PlayerSlot::Strategy {
                kind,
                model_file,
                params,
            } if kind.needs_model() => crate::app::player::PlayerSlot::Strategy {
                kind: *kind,
                model_file: model_file.clone(),
                params: *params,
            },
            _ => {
                let model_file = app.resolve_model_file(None, s.core.variant)?;
                crate::app::player::PlayerSlot::strategy(
                    crate::app::player::StrategyKind::from_engine_kind(
                        crate::app::models::EngineKind::from_model_file(&model_file),
                    ),
                    Some(model_file),
                    app.params(),
                )
            }
        };
        let prev_tree = s.take_reusable_mcts_tree();
        (s.core.current.clone(), slot, prev_tree, s.core.version)
    };
    let app2 = std::sync::Arc::clone(&app);
    let decision = tokio::task::spawn_blocking(move || {
        crate::app::player::decide(&snap, &slot, prev_tree, &app2.models)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("blocking 任务 join 失败: {e}")))?
    .map_err(ApiError::Internal)?;
    let tree = decision.mcts_tree.ok_or_else(|| {
        ApiError::BadRequest("所选模型不产生 MCTS 搜索树（.nnue 为 Expectimax 叶评估后端，请用 POST /api/games/:id/nnue/search）".into())
    })?;
    let info = tree.root_info();
    let mut games = write_lock(&app.games);
    store_manual_search_trees(&mut games, game_id, version, Some(tree), None);
    tracing::info!(game = %game_id, "手动 MCTS 搜索完成");
    Ok(Json(info))
}
