//! 请求 / 响应 JSON 类型。

use serde::{Deserialize, Serialize};

// ============================================================================
// 请求
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGameReq {
    /// 棋盘变体
    #[serde(default)]
    pub variant: Option<String>,
    /// 是否启用 AI 对手（默认 true）
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    /// AI 执子方："red" / "black" / "auto"（默认 "auto" = AI 执黑，人类执红先行）
    #[serde(default)]
    pub ai_player: Option<String>,
    /// MCTS 模拟次数（默认取服务器 --default-sims）
    #[serde(default)]
    pub mcts_sims: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveReq {
    /// 动作空间索引
    pub action: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoReq {
    /// 回退步数（默认 1；每步含 AI 回合时一并回退——按快照逐帧回退）
    #[serde(default)]
    pub steps: Option<usize>,
}

// ============================================================================
// 响应
// ============================================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PieceView {
    /// 棋子类型（小写英文："soldier" / "cannon" / ...）
    #[serde(rename = "type")]
    pub piece_type: String,
    /// 归属方："red" / "black"
    pub side: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CellView {
    /// 格位索引（0..rows*cols，行优先）
    pub index: usize,
    /// 行号
    pub row: usize,
    /// 列号
    pub col: usize,
    /// "empty" / "hidden" / "revealed"
    pub state: String,
    /// 仅 revealed 时给出（暗子的真实身份不泄露）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub piece: Option<PieceView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionView {
    /// 动作空间索引（走子请求使用）
    pub action: usize,
    /// 源格索引（翻棋时 == to）
    pub from: usize,
    /// 目标格索引
    pub to: usize,
    /// 是否机会动作（翻棋 / 吃暗子，结果有随机性）
    pub is_chance: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiInfoView {
    pub enabled: bool,
    /// AI 执子方："red" / "black"（未启用时为 null）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player: Option<String>,
    /// MCTS 模拟次数
    pub mcts_sims: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStateResp {
    pub game_id: String,
    pub variant: String,
    /// 单调递增版本号（并发/竞态防护：AI 结果与客户端操作基于版本对齐）
    pub version: u64,
    pub current_player: String,
    /// 连续无吃子步数
    pub move_count: usize,
    /// 总步数
    pub total_steps: usize,
    pub max_steps: usize,
    pub red_score: i32,
    pub black_score: i32,
    pub red_hp: i32,
    pub black_hp: i32,
    /// 对局是否结束
    pub over: bool,
    /// 胜负分出而结束（对齐前端 StepResult.terminated）
    pub terminated: bool,
    /// 步数/规则截断而结束（对齐前端 StepResult.truncated）
    pub truncated: bool,
    /// 终局原因："checkmate"（胜负分出）/ "draw"（无吃子判和）/ "max_steps"（步数截断）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_reason: Option<String>,
    /// "red" / "black" / "draw"（未结束时为 null）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_action: Option<usize>,
    pub board: Vec<CellView>,
    pub legal_actions: Vec<ActionView>,
    pub ai: AiInfoView,
    /// 当前是否轮到 AI 行棋
    pub ai_to_move: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedAction {
    /// 行棋方："red" / "black"
    pub player: String,
    pub action: ActionView,
}

/// 一次 Gumbel MCTS 搜索统计（对齐前端 types.ts `MctsSearchStats`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MctsStatsDto {
    pub reused: bool,
    pub budget: usize,
    pub actual_sims: u32,
    pub max_depth: usize,
    pub nodes: usize,
}

impl MctsStatsDto {
    /// 从会话中保留的搜索树提取统计（无树时返回 None）。
    pub fn from_session(tree: Option<&crate::mcts::MctsTreeBox>) -> Option<Self> {
        let stats = &tree?.root_info().stats;
        Some(Self {
            reused: stats.reused,
            budget: stats.budget,
            actual_sims: stats.actual_sims,
            max_depth: stats.max_depth,
            nodes: stats.nodes,
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveResp {
    pub state: GameStateResp,
    /// 本次请求实际执行的动作序列（人类 1 步 + 可能的 AI 回应 1 步）
    pub actions: Vec<AppliedAction>,
    /// AI 回合的 MCTS 搜索统计（本请求含 AI 落子时才有）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcts_stats: Option<MctsStatsDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameSummary {
    pub game_id: String,
    pub variant: String,
    pub version: u64,
    pub current_player: String,
    pub total_steps: usize,
    pub over: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
    pub ai: AiInfoView,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameListResp {
    pub games: Vec<GameSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameCreatedResp {
    pub game_id: String,
    pub state: GameStateResp,
}

/// 变体元信息：与前端 `VariantInfo`（types.ts）字段一一对应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantInfo {
    /// 变体 ID（`Variant::as_str()`）
    pub id: String,
    /// 面向用户的显示名（`Variant::label()`）
    pub label: String,
    pub rows: usize,
    pub cols: usize,
    pub initial_health: i32,
    pub max_consecutive_moves_for_draw: usize,
    /// 是否有已加载的模型（AI 可用）
    pub ai_available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantsResp {
    pub variants: Vec<VariantInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// 模型文件路径
    pub file: String,
    /// 文件名 stem（仅提示用途）
    pub stem: String,
    /// 按输入维度匹配到的变体 ID 列表
    pub variants: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResp {
    pub ok: bool,
    pub models: Vec<ModelInfo>,
    pub sessions: usize,
    pub device: String,
}

// ============================================================================
// 序列化辅助
// ============================================================================

pub fn player_str(p: banqi_core::core::env::types::Player) -> String {
    match p {
        banqi_core::core::env::types::Player::Red => "red".to_string(),
        banqi_core::core::env::types::Player::Black => "black".to_string(),
    }
}

/// 全局视角胜者编码（Some(1)=红，Some(-1)=黑，Some(0)=平局）转字符串。
pub fn winner_str(w: Option<i32>) -> Option<String> {
    match w {
        Some(1) => Some("red".to_string()),
        Some(-1) => Some("black".to_string()),
        Some(0) => Some("draw".to_string()),
        _ => None,
    }
}

pub fn piece_type_str(t: banqi_core::core::env::types::PieceType) -> String {
    use banqi_core::core::env::types::PieceType::*;
    match t {
        Soldier => "soldier",
        Cannon => "cannon",
        Horse => "horse",
        Chariot => "chariot",
        Elephant => "elephant",
        Advisor => "advisor",
        General => "general",
    }
    .to_string()
}
