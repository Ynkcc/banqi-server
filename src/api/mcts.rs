//! MCTS 搜索树查询 handlers（树由 AI 回合 / 手动搜索保留在会话中）。
//!
//! 契约见 docs/backend-api.md §5-①；DTO 与 banqi-gui `mcts_view.rs` 同构，
//! 前端 types.ts（snake_case）直传，client 适配层零改动。

use axum::extract::{Path, State};
use axum::Json;

use crate::app::sync::{read_lock, write_lock};
use crate::error::ApiError;
use crate::mcts::{MctsEdge, MctsNodeBoard, MctsNodeDetail, MctsRootInfo};
use crate::state::{
    get_mut_session, get_session, mcts_tree_of, parse_game_id, parse_id, store_manual_search_trees,
    SharedState,
};

/// 解析路径中的节点 ID。
fn parse_node_id(raw: &str) -> Result<usize, ApiError> {
    parse_id(raw, "节点 ID")
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
/// 决策分派收敛于 `app::player::decide`（单一真源）；模型取当前行棋方
/// 策略槽位绑定的模型（无则按变体自动匹配），上一棵树尝试跨回合复用。
pub async fn search(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<MctsRootInfo>, ApiError> {
    let game_id = parse_game_id(&id)?;

    // 锁 1：读取搜索所需数据并取出上一棵树供复用（立即释放，不跨 await）
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
                // 当前方为 Human / 无模型策略：按变体自动匹配模型 + 服务器默认参数
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

    // 锁外搜索（blocking 线程池；模型随用随加载，缓存命中直接返回）
    let app2 = std::sync::Arc::clone(&app);
    let decision =
        tokio::task::spawn_blocking(move || crate::app::player::decide(&snap, &slot, prev_tree, &app2.models))
            .await
            .map_err(|e| ApiError::Internal(format!("blocking 任务 join 失败: {e}")))?
            .map_err(ApiError::Internal)?;
    let tree = decision.mcts_tree.ok_or_else(|| {
        ApiError::BadRequest("所选模型不产生 MCTS 搜索树（.nnue 为 Expectimax 叶评估后端，请用 POST /api/games/:id/nnue/search）".into())
    })?;
    let info = tree.root_info();

    // 锁 2：版本号一致时存入会话（期间对局被修改则丢弃，正常竞态）
    let mut games = write_lock(&app.games);
    store_manual_search_trees(&mut games, game_id, version, Some(tree), None);
    tracing::info!(game = %game_id, "手动 MCTS 搜索完成");
    Ok(Json(info))
}
