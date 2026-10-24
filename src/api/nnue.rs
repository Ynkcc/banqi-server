use crate::app::sync::{read_lock, write_lock};
use crate::error::ApiError;
use crate::nnue::{
    ExEdge, ExNodeBoard, ExNodeSummary, ExRootInfo, ex_edge_list, ex_node_board, ex_node_summary,
    ex_root_info,
};
use crate::state::{SharedState, parse_game_id, parse_id, store_manual_search_trees};
use axum::Json;
use axum::extract::{Path, State};
use banqi_core::core::env::DarkChessEnv;
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
    let (variant, snap, budget, model_file, version) = {
        let games = read_lock(&app.games);
        let s = games
            .get(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
        let budget = match s.slots.side(s.core.current.current_player) {
            crate::app::player::PlayerSlot::Strategy {
                kind: crate::app::player::StrategyKind::ExpectimaxNnue,
                params,
                ..
            } => params.nnue_budget,
            _ => app.default_nnue_budget(),
        };
        let model_file = match s.slots.side(s.core.current.current_player) {
            crate::app::player::PlayerSlot::Strategy {
                model_file: Some(f),
                ..
            } => f.clone(),
            _ => app.resolve_model_file(None, s.core.variant)?,
        };
        (
            s.core.variant,
            s.core.current.clone(),
            budget,
            model_file,
            s.core.version,
        )
    };
    if !model_file.ends_with(".nnue") {
        return Err(ApiError::BadRequest(
            "Expectimax 手动搜索需要 .nnue 叶评估模型（.onnx / .pt 为 MCTS 策略模型，走 /mcts/search）"
                .into(),
        ));
    }
    let evaluator = app
        .acquire_nnue_validated(model_file, variant)
        .await
        .map_err(ApiError::ModelUnavailable)?;
    let kept = tokio::task::spawn_blocking(move || {
        use banqi_core::core::env::SnapshotEnv;
        let env = DarkChessEnv::from_snapshot(&snap).map_err(|e| format!("重建环境失败: {e}"))?;
        crate::app::search::nnue_search_tree(&env, evaluator, budget)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("blocking 任务 join 失败: {e}")))?
    .map_err(ApiError::Internal)?
    .1;
    let info = ex_root_info(&kept).ok_or_else(|| ApiError::Internal("搜索未产生轨迹树".into()))?;
    let mut games = write_lock(&app.games);
    store_manual_search_trees(&mut games, game_id, version, None, Some(kept));
    tracing::info!(game = %game_id, "手动 Expectimax 搜索完成");
    Ok(Json(info))
}
