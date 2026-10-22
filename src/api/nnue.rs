//! Expectimax 搜索轨迹树查询 handlers（树由 NNUE 对手回合 / 手动搜索保留在会话中）。
//!
//! 契约与 `api/mcts.rs` 同构；DTO 见 `crate::app::expectimax_view`，
//! 前端 types.ts（snake_case）直传。
//!
//! 树捕获语义（Alpha-Beta 搜索轨迹，非完整博弈树）：
//! - `flag` 区分精确值 / 下界 / 上界（非 PV 分支的值是界）；
//! - `mark = "tt"` 的节点为 TT 命中截断，子树未展开；
//! - 机会 outcome 边携带真实概率（Star1 区间早退跳过的 outcome 无节点）。

use axum::extract::{Path, State};
use axum::Json;
use banqi_core::core::env::DarkChessEnv;
use uuid::Uuid;

use crate::app::sync::{read_lock, write_lock};
use crate::error::ApiError;
use crate::nnue::{
    ex_edge_list, ex_node_board, ex_node_summary, ex_root_info, ExEdge, ExNodeBoard,
    ExNodeSummary, ExRootInfo,
};
use crate::state::SharedState;

fn parse_ids(id: String, node: String) -> Result<(Uuid, usize), ApiError> {
    let game_id = Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest(format!("非法对局 ID: {id}")))?;
    let node_id: usize =
        node.parse().map_err(|_| ApiError::BadRequest(format!("非法节点 ID: {node}")))?;
    Ok((game_id, node_id))
}

/// GET /api/games/:id/nnue/root
pub async fn root(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<ExRootInfo>, ApiError> {
    let game_id = Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest(format!("非法对局 ID: {id}")))?;
    let games = read_lock(&app.games);
    let s = games
        .get(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
    let kept = s.expectimax_tree.as_ref().ok_or(ApiError::NnueTreeMissing)?;
    ex_root_info(kept).ok_or(ApiError::NnueTreeMissing).map(Json)
}

/// GET /api/games/:id/nnue/node/:node/children
pub async fn children(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<Vec<ExEdge>>, ApiError> {
    let (game_id, node_id) = parse_ids(id, node)?;
    let games = read_lock(&app.games);
    let s = games.get(&game_id).ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let kept = s.expectimax_tree.as_ref().ok_or(ApiError::NnueTreeMissing)?;
    if node_id >= kept.tree.nodes.len() {
        return Err(ApiError::BadRequest(format!("节点 {node_id} 不存在（超出树范围）")));
    }
    Ok(Json(ex_edge_list(&kept.tree, node_id)))
}

/// GET /api/games/:id/nnue/node/:node
pub async fn detail(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<ExNodeSummary>, ApiError> {
    let (game_id, node_id) = parse_ids(id, node)?;
    let games = read_lock(&app.games);
    let s = games.get(&game_id).ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let kept = s.expectimax_tree.as_ref().ok_or(ApiError::NnueTreeMissing)?;
    ex_node_summary(&kept.tree, node_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("节点 {node_id} 不存在（超出树范围）")))
}

/// GET /api/games/:id/nnue/node/:node/board
pub async fn board(
    State(app): State<SharedState>,
    Path((id, node)): Path<(String, String)>,
) -> Result<Json<ExNodeBoard>, ApiError> {
    let (game_id, node_id) = parse_ids(id, node)?;
    let games = read_lock(&app.games);
    let s = games.get(&game_id).ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let kept = s.expectimax_tree.as_ref().ok_or(ApiError::NnueTreeMissing)?;
    ex_node_board(&kept.tree, node_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("节点 {node_id} 不存在或未保存局面")))
}

/// POST /api/games/:id/nnue/search
///
/// 在当前局面上手动触发一次 Expectimax+NNUE 搜索（不走子），替换会话中的
/// 轨迹树。要求对局绑定 `.nnue` 模型（随用随加载 + 布局校验）。
/// 可视化约束（threads=1、关 ASP、树捕获）由 `nnue_search_tree` 统一施加；
/// 预算取对局快照的 `search_budget`（创建对局时的 NNUE 搜索预算）。
pub async fn search(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<ExRootInfo>, ApiError> {
    let game_id = Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest(format!("非法对局 ID: {id}")))?;

    // 锁 1：读取搜索所需数据（立即释放，不跨 await）
    let (_variant, snap, budget, model_file) = {
        let games = read_lock(&app.games);
        let s = games
            .get(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
        let budget = s
            .opponent
            .map(|o| o.search_budget)
            .unwrap_or_else(|| app.default_nnue_budget());
        let model_file = s.model_file.clone().ok_or_else(|| {
            ApiError::BadRequest(
                "对局未绑定模型文件，无法执行 Expectimax 搜索（创建对局时指定 modelFile 或 PUT /api/games/:id/model 绑定 .nnue）"
                    .into(),
            )
        })?;
        (s.core.variant, s.core.current.clone(), budget, model_file)
    };

    // 锁外取对局绑定的 .nnue 模型并执行树捕获搜索
    if !model_file.ends_with(".nnue") {
        return Err(ApiError::BadRequest(
            "Expectimax 手动搜索需要 .nnue 叶评估模型（.onnx / .pt 为 MCTS 策略模型，走 /mcts/search）"
                .into(),
        ));
    }
    let evaluator = app
        .acquire_nnue_validated(model_file, _variant)
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
    if let Some(s) = games.get_mut(&game_id) {
        s.expectimax_tree = Some(kept);
    }
    tracing::info!(game = %game_id, "手动 Expectimax 搜索完成");
    Ok(Json(info))
}
