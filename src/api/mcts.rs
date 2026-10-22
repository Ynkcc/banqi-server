//! MCTS 搜索树查询 handlers（树由 AI 回合 / 手动搜索保留在会话中）。
//!
//! 契约见 docs/backend-api.md §5-①；DTO 与 banqi-gui `mcts_view.rs` 同构，
//! 前端 types.ts（snake_case）直传，client 适配层零改动。

use axum::extract::{Path, State};
use axum::Json;
use uuid::Uuid;

use crate::app::sync::read_lock;
use crate::app::sync::write_lock;
use crate::state::EngineKind;

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
    let games = read_lock(&app.games);
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
    let games = read_lock(&app.games);
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
    let games = read_lock(&app.games);
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
    let games = read_lock(&app.games);
    let s = games.get(&game_id).ok_or(ApiError::GameNotFound(game_id.to_string()))?;
    let tree = s.mcts_tree.as_ref().ok_or(ApiError::MctsTreeMissing)?;
    tree.board(node_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("节点 {node_id} 不存在或未保存局面")))
}

/// POST /api/games/:id/mcts/search
///
/// 在当前局面上手动触发一次 MCTS 搜索（不走子），替换会话中的浏览树。
/// 按对局绑定模型的扩展名分派推理后端（`.pt` → TorchScript 需 torch，
/// 其余 → ONNX 需 onnx）；上一棵树沿「AI 上手 → 对手上手」尝试复用。
#[allow(unreachable_code)] // 无 onnx/torch feature 时全部分支 return，尾部不可达
pub async fn search(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<MctsRootInfo>, ApiError> {
    let game_id = Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest(format!("非法对局 ID: {id}")))?;

    // 锁 1：读取搜索所需数据并取出上一棵树供复用（立即释放，不跨 await）
    let (_variant, snap, sims, model_file, prev_tree) = {
        let mut games = write_lock(&app.games);
        let s = games
            .get_mut(&game_id)
            .ok_or_else(|| ApiError::GameNotFound(id.clone()))?;
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
        (s.core.variant, s.core.current.clone(), sims, model_file, prev_tree)
    };

    // 锁外取对局绑定的模型（随用随加载：缓存命中直接返回，首次使用时加载），
    // 按扩展名分派推理后端
    let engine = EngineKind::from_model_file(&model_file);
    let (_action, tree): (usize, crate::mcts::MctsTreeBox) = match engine {
        EngineKind::Nnue => {
            return Err(ApiError::BadRequest(
                "MCTS 手动搜索需要 .onnx / .pt 策略价值模型（.nnue 为 NNUE 叶评估后端，无搜索树）"
                    .into(),
            ));
        }
        EngineKind::MctsOnnx => {
            #[cfg(feature = "onnx")]
            {
                let model = app
                    .acquire_model(model_file)
                    .await
                    .map_err(ApiError::ModelUnavailable)?;
                crate::ai::choose_blocking(snap, model, sims, prev_tree)
                    .await
                    .map_err(ApiError::Internal)?
                    .ok_or_else(|| ApiError::Internal("MCTS 未返回动作".to_string()))?
            }
            #[cfg(not(feature = "onnx"))]
            {
                let _ = (_variant, snap, sims, prev_tree, &model_file);
                return Err(ApiError::ModelUnavailable(
                    "服务编译未启用 onnx 推理，MCTS+ONNX 手动搜索不可用（可绑定 .pt 模型走 torch 路径）"
                        .into(),
                ));
            }
        }
        EngineKind::MctsDL => {
            #[cfg(feature = "torch")]
            {
                let model = app
                    .acquire_torch_model(model_file)
                    .await
                    .map_err(ApiError::ModelUnavailable)?;
                crate::ai::choose_torch_blocking(snap, model, sims, prev_tree)
                    .await
                    .map_err(ApiError::Internal)?
                    .ok_or_else(|| ApiError::Internal("MCTS 未返回动作".to_string()))?
            }
            #[cfg(not(feature = "torch"))]
            {
                let _ = (_variant, snap, sims, prev_tree, &model_file);
                return Err(ApiError::ModelUnavailable(
                    "服务编译未启用 torch 推理，MCTS+DL 手动搜索不可用（可绑定 .onnx 模型走 onnx 路径）"
                        .into(),
                ));
            }
        }
    };
    let info = tree.root_info();

    let mut games = write_lock(&app.games);
    if let Some(s) = games.get_mut(&game_id) {
        s.mcts_tree = Some(tree);
    }
    tracing::info!(game = %game_id, "手动 MCTS 搜索完成");
    Ok(Json(info))
}
