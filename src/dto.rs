//! 请求 / 响应 JSON 类型。

use serde::{Deserialize, Serialize};

// 请求

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGameReq {
    /// 棋盘变体
    #[serde(default)]
    pub variant: Option<String>,
    /// 红 / 黑双方选手槽位（None = 双方 Human，原本地双人）
    #[serde(default)]
    pub slots: Option<SlotsReq>,
    /// BFEN 局面串：提供时由该局面重建对局（变体以 BFEN 自带标识为准）。
    #[serde(default)]
    pub fen: Option<String>,
}

/// POST /api/games 请求中的双方槽位（缺省方 = Human）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotsReq {
    #[serde(default)]
    pub red: Option<SlotReq>,
    #[serde(default)]
    pub black: Option<SlotReq>,
}

/// 单个选手槽位请求：`{ kind, modelFile?, params?, agentId? }`。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotReq {
    /// "Human" / 策略种类（见 `StrategyKind::parse`）/ "Agent"
    pub kind: String,
    /// 模型类策略绑定的模型文件（须在模型目录内）；缺省按变体 ID 自动匹配
    #[serde(default)]
    pub model_file: Option<String>,
    /// 引擎参数（缺省字段取服务器当前默认值，建局时快照固定）
    #[serde(default)]
    pub params: Option<SlotParamsReq>,
    /// `kind = "Agent"` 时必填：绑定 Agent 实体（其策略/搜索配置实时生效）
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// 单槽位引擎参数（全字段可选，缺省取服务器当前默认值）。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SlotParamsReq {
    /// Gumbel MCTS 模拟次数（Mcts 类策略）
    #[serde(default)]
    pub mcts_sims: Option<usize>,
    /// Expectimax 强引擎节点预算
    #[serde(default)]
    pub engine_budget: Option<u64>,
    /// Expectimax 预算模式："nodes" / "depth_nodes" / "time"
    #[serde(default)]
    pub nnue_budget_mode: Option<String>,
    /// Expectimax 搜索深度（depth_nodes 模式）
    #[serde(default)]
    pub nnue_depth: Option<i32>,
    /// Expectimax 节点预算（nodes / depth_nodes 模式）
    #[serde(default)]
    pub nnue_budget: Option<u64>,
    /// Expectimax 时间预算毫秒（time 模式）
    #[serde(default)]
    pub nnue_time_ms: Option<u64>,
    /// 搜索硬超时毫秒（全模式生效）
    #[serde(default)]
    pub search_timeout_ms: Option<u64>,
}

/// PUT /api/games/:id/slots：对局中切换任一方槽位（只更新给出的方）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutSlotsReq {
    #[serde(default)]
    pub red: Option<SlotReq>,
    #[serde(default)]
    pub black: Option<SlotReq>,
}

/// POST /api/games/:id/dry-run：为当前行棋方请求策略建议走法（不落子）。
///
/// 当前方槽位为策略时忽略请求字段（预览该策略将走的棋）；
/// 当前方为 Human 时作为提示：`kind` 缺省为 Expectimax，模型类策略
/// 按变体自动匹配模型。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunReq {
    /// Human 方提示用的策略种类（缺省 Expectimax；兼容旧名 Engine/Nnue）
    #[serde(default)]
    pub kind: Option<String>,
    /// 提示用策略的模型文件（模型类策略需要；缺省按变体自动匹配）
    #[serde(default)]
    pub model_file: Option<String>,
    /// 指定 Agent：以其当前配置为当前行棋方计算建议（变体须一致），
    /// 忽略 kind / model_file / 当前方槽位绑定——建议永远作用于当前行动方
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// POST /games/{id}/run 请求体（可空）：单步执行当前行棋方槽位策略一步。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReq {
    /// 指定 Agent：以其当前配置为当前行棋方执行一步（变体须一致），
    /// 忽略当前方槽位绑定——单步永远作用于当前行动方
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// 创建 Agent 请求：`{ variant, kind, modelFile?, params? }`。
/// 创建后除搜索参数（params）外只读；模型类策略在创建时预加载校验。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAgentReq {
    /// 适用的棋盘变体 ID（参与对局时变体须一致）
    pub variant: String,
    /// 策略种类（见 `StrategyKind::parse`；不接受 "Human" / "Agent"）
    pub kind: String,
    /// 模型类策略绑定的模型文件；缺省按变体 ID 自动匹配
    #[serde(default)]
    pub model_file: Option<String>,
    /// 搜索参数（缺省字段取服务器当前默认值；可实时调整）
    #[serde(default)]
    pub params: Option<SlotParamsReq>,
}

/// Agent 视图（创建后只读；搜索参数随 `PUT /api/agents/{id}/settings` 实时变化）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentResp {
    pub id: String,
    /// 适用的棋盘变体 ID
    pub variant: String,
    #[serde(flatten)]
    pub slot: SlotView,
}

#[derive(Debug, Serialize)]
pub struct AgentsListResp {
    pub agents: Vec<AgentResp>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsReq {
    /// 默认 MCTS 搜索预算（模拟次数，运行时生效；至少 1）。缺省不修改。
    #[serde(default)]
    pub mcts_sims: Option<usize>,
    /// 默认 Expectimax 强引擎节点预算（运行时生效；至少 1）。缺省不修改。
    #[serde(default)]
    pub engine_budget: Option<u64>,
    /// 默认 NNUE 预算模式："nodes" / "depth_nodes" / "time"。缺省不修改。
    #[serde(default)]
    pub nnue_budget_mode: Option<String>,
    /// 默认 NNUE 搜索深度（depth_nodes 模式；至少 1）。缺省不修改。
    #[serde(default)]
    pub nnue_depth: Option<i32>,
    /// 默认 NNUE 节点预算（nodes/depth_nodes 模式；至少 1）。缺省不修改。
    #[serde(default)]
    pub nnue_budget: Option<u64>,
    /// 默认 NNUE 时间预算毫秒（time 模式；0 = 仅超时控制）。缺省不修改。
    #[serde(default)]
    pub nnue_time_ms: Option<u64>,
    /// 默认搜索硬超时毫秒（全模式必选生效；至少 1）。缺省不修改。
    #[serde(default)]
    pub search_timeout_ms: Option<u64>,
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
    /// 显式回退步数（按快照逐帧回退）。
    /// 缺省时执行「悔棋到当前行棋方上次行动前」语义：自动回退若干步，
    /// 直到回到当前行棋方上次行动前的局面（AI 对局中含 AI 回合）。
    #[serde(default)]
    pub steps: Option<usize>,
}

/// POST /api/engine/move：无状态引擎查询（供连线程序调用）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineMoveReq {
    /// 局面 BFEN 串（变体以 BFEN 自带标识为准；不编码 last_action 与 true_board）
    pub fen: String,
    /// MCTS 模拟次数；缺省时取服务器当前默认值
    #[serde(default)]
    pub mcts_sims: Option<usize>,
    /// NNUE 预算模式："nodes" / "depth_nodes"（默认）/ "time"；缺省取服务器当前默认值
    #[serde(default)]
    pub nnue_budget_mode: Option<String>,
    /// NNUE 搜索深度（仅 `.nnue` 模型，depth_nodes 模式生效）；缺省时取服务器当前默认值
    #[serde(default)]
    pub nnue_depth: Option<i32>,
    /// NNUE 节点预算（仅 `.nnue` 模型，nodes/depth_nodes 模式生效）；缺省时取服务器当前默认值
    #[serde(default)]
    pub nnue_budget: Option<u64>,
    /// NNUE 时间预算毫秒（仅 `.nnue` 模型，time 模式生效）；缺省时取服务器当前默认值
    #[serde(default)]
    pub nnue_time_ms: Option<u64>,
    /// 搜索硬超时毫秒（全模式必选生效）；缺省时取服务器当前默认值（60s）
    #[serde(default)]
    pub search_timeout_ms: Option<u64>,
    /// 模型文件（须在模型目录内，`.onnx` / `.nnue` 均可；推理后端由扩展名决定）。
    /// 缺省时按变体 ID 自动匹配唯一文件名候选（如 `dark_chess_4x8.onnx`）；
    /// 无法唯一确定时报错。
    #[serde(default)]
    pub model_file: Option<String>,
}

// 响应

#[derive(Debug, Serialize)]
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

/// 单个选手槽位视图（对局状态 / 摘要下发）。
#[derive(Debug, Serialize)]
pub struct SlotView {
    /// "Human" / 策略种类 / "Agent"（与前端契约一致）
    pub kind: String,
    /// Agent 槽位绑定的 Agent ID（其余为 None；实际策略由 Agent 实时持有）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// 模型类策略绑定的模型文件
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_file: Option<String>,
    /// Gumbel MCTS 模拟次数（Mcts 类策略）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcts_sims: Option<usize>,
    /// Expectimax 强引擎节点预算
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_budget: Option<u64>,
    /// Expectimax 预算模式："nodes" / "depth_nodes" / "time"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_mode: Option<String>,
    /// Expectimax 搜索深度（depth_nodes 模式）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_depth: Option<i32>,
    /// Expectimax 节点预算（nodes / depth_nodes 模式）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_budget: Option<u64>,
    /// Expectimax 时间预算毫秒（time 模式）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_time_ms: Option<u64>,
    /// 搜索硬超时毫秒（全模式生效）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_timeout_ms: Option<u64>,
}

impl SlotView {
    pub fn human() -> Self {
        Self {
            kind: "Human".to_string(),
            agent_id: None,
            model_file: None,
            mcts_sims: None,
            engine_budget: None,
            budget_mode: None,
            nnue_depth: None,
            nnue_budget: None,
            nnue_time_ms: None,
            search_timeout_ms: None,
        }
    }
}

/// 红 / 黑双方槽位视图。
#[derive(Debug, Serialize)]
pub struct SlotsView {
    pub red: SlotView,
    pub black: SlotView,
}

/// POST /api/games/:id/dry-run 响应：当前行棋方策略的建议走法（不落子）。
#[derive(Debug, Serialize)]
pub struct SuggestionResp {
    /// 建议产生时的对局版本号（前端据此丢弃过期建议）
    pub version: u64,
    /// 行棋方："red" / "black"
    pub player: String,
    /// 动作空间索引
    pub action: usize,
    /// 源格索引（翻棋时 == to）
    pub from: usize,
    /// 目标格索引
    pub to: usize,
    /// 是否机会动作（翻棋 / 吃暗子）
    pub is_chance: bool,
    /// 是否纯翻棋
    pub is_flip: bool,
    /// 是否吃子（吃明子；吃暗子为机会动作）
    pub is_capture: bool,
    /// 根评估值（行棋方视角；不可用时为 null）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// MCTS 类策略的搜索统计
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcts_stats: Option<MctsStatsDto>,
    /// ExpectimaxNnue 策略的搜索统计
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_stats: Option<NnueStatsDto>,
}

/// GET /api/games/:id 响应：前端 `GameState`（types.ts）为基座的完整状态
/// + server 专属扩展字段（对局 ID / 版本号 / 合法动作 / 对手信息等）。
#[derive(Debug, Serialize)]
pub struct GameStateResp {
    /// 前端 GameState 全量字段（flatten 展开为顶层）
    #[serde(flatten)]
    pub game: crate::app::view::GameStateView,
    pub game_id: String,
    /// 单调递增版本号（并发/竞态防护：AI 结果与客户端操作基于版本对齐）
    pub version: u64,
    pub max_steps: usize,
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
    pub legal_actions: Vec<ActionView>,
    /// 决策历史（棋谱）：每手棋的实际执行结果，含翻棋/吃暗子翻出的真实棋子。
    /// 悔棋后同步截断；重开对局清空。
    pub history: Vec<HistoryMoveView>,
    /// 红 / 黑双方选手槽位（Human 或策略配置）
    pub slots: SlotsView,
    /// 当前行棋方是否为策略（true 时可 POST /run 单步执行或纳入 auto 循环）
    pub can_run: bool,
    /// 当前是否可悔棋（当前行棋方存在更早的行动快照且对局未结束）
    pub can_undo: bool,
}

/// 一手棋的决策记录视图（对齐前端 types.ts `HistoryMove`）。
#[derive(Debug, Serialize)]
pub struct HistoryMoveView {
    /// 手数（1 起，对局内单调递增）
    pub ply: usize,
    /// 行棋方："Red" / "Black"
    pub player: String,
    /// 动作空间索引
    pub action: usize,
    /// 源格索引（翻棋时 == to）
    pub from: usize,
    /// 目标格索引
    pub to: usize,
    /// 纯翻棋
    pub is_flip: bool,
    /// 机会动作（翻棋 / 吃暗子，结果有随机性）
    pub is_chance: bool,
    /// 吃明子（目标是对方已翻开的明子）
    pub is_capture: bool,
    /// 机会动作实际翻出的棋子短码（"R_Sol" 等；翻棋 = 翻出的子；吃暗子含炮打暗子 = 打出的子）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revealed: Option<String>,
    /// 被消灭的棋子短码（吃明子的被吃子；吃暗子时 == revealed）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AppliedAction {
    /// 行棋方："red" / "black"
    pub player: String,
    pub action: ActionView,
}

/// 一次 Gumbel MCTS 搜索统计（对齐前端 types.ts `MctsSearchStats`，snake_case）。
#[derive(Debug, Clone, Serialize)]
pub struct MctsStatsDto {
    pub reused: bool,
    pub budget: usize,
    pub actual_sims: u32,
    pub max_depth: usize,
    pub nodes: usize,
    /// 搜索本体（`mcts.run()`）耗时（毫秒）
    pub elapsed_ms: u64,
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
            elapsed_ms: stats.elapsed_ms,
        })
    }
}

/// 一次 Expectimax 搜索统计（对齐前端 types.ts `NnueSearchStats`，snake_case）。
#[derive(Debug, Clone, Serialize)]
pub struct NnueStatsDto {
    /// 推理后端标识（恒为 "nnue"）
    pub engine: String,
    /// 生效的预算模式："nodes" / "depth_nodes" / "time"
    pub mode: String,
    /// 请求的迭代加深深度上限（0 = 不限，由预算/超时控制）
    pub depth: i32,
    /// 请求的节点预算（0 = 不限）
    pub budget: u64,
    /// 生效的时间上限毫秒（模式时间与硬超时的较小者）
    pub time_limit_ms: u64,
    /// 实际消耗的节点数
    pub nodes: u64,
    /// 完成的最深迭代层数
    pub reached_depth: i32,
    /// 搜索本体耗时（毫秒）
    pub elapsed_ms: u64,
}

/// 走子 / AI 回合 / 悔棋响应：对齐前端 types.ts `StepResult`
/// （state + terminated/truncated/winner + 搜索统计），附 server 专属 actions。
#[derive(Debug, Serialize)]
pub struct MoveResp {
    pub state: GameStateResp,
    /// 胜负分出而结束（对齐前端 StepResult.terminated）
    pub terminated: bool,
    /// 步数/规则截断而结束（对齐前端 StepResult.truncated）
    pub truncated: bool,
    /// 全局视角胜者：1=红 / -1=黑 / 0=平局 / null=未结束（对齐前端 StepResult.winner）
    pub winner: Option<i32>,
    /// 本次请求实际执行的动作序列（人类 1 步 + 可能的 AI 回应 1 步；server 扩展字段）
    pub actions: Vec<AppliedAction>,
    /// AI 回合的 MCTS 搜索统计（本请求含 MCTS 类 AI 落子时才有）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcts_stats: Option<MctsStatsDto>,
    /// AI 回合的 NNUE 搜索统计（本请求含 Nnue AI 落子时才有）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_stats: Option<NnueStatsDto>,
}

impl MoveResp {
    /// 由状态视图组装响应：`terminated` / `truncated` / `winner` 均取自 `state`
    /// （走子 / 强制 AI / 悔棋三个 handler 共用，避免逐字段重复组装）。
    pub fn new(
        state: GameStateResp,
        actions: Vec<AppliedAction>,
        mcts_stats: Option<MctsStatsDto>,
        nnue_stats: Option<NnueStatsDto>,
    ) -> Self {
        let terminated = state.terminated;
        let truncated = state.truncated;
        let winner = winner_code(state.winner.as_deref());
        Self {
            state,
            terminated,
            truncated,
            winner,
            actions,
            mcts_stats,
            nnue_stats,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GameSummary {
    pub game_id: String,
    pub variant: String,
    pub version: u64,
    pub current_player: String,
    pub total_steps: usize,
    pub over: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
    /// 红 / 黑双方槽位（kind 摘要）
    pub slots: SlotsView,
}

#[derive(Debug, Serialize)]
pub struct GameListResp {
    pub games: Vec<GameSummary>,
}

#[derive(Debug, Serialize)]
pub struct GameCreatedResp {
    pub game_id: String,
    pub state: GameStateResp,
}

/// 变体元信息：与前端 `VariantInfo`（types.ts）字段一一对应（snake_case）。
#[derive(Debug, Serialize)]
pub struct VariantInfo {
    /// 变体 ID（`Variant::as_str()`）
    pub id: String,
    /// 面向用户的显示名（`Variant::label()`）
    pub label: String,
    pub rows: usize,
    pub cols: usize,
    pub initial_health: i32,
    pub max_consecutive_moves_for_draw: usize,
}

#[derive(Debug, Serialize)]
pub struct VariantsResp {
    pub variants: Vec<VariantInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsResp {
    /// 当前默认 MCTS 搜索预算（模拟次数）
    pub mcts_sims: usize,
    /// 当前默认 Expectimax 强引擎节点预算
    pub engine_budget: u64,
    /// 当前默认 NNUE 预算模式："nodes" / "depth_nodes" / "time"
    pub nnue_budget_mode: String,
    /// 当前默认 NNUE 搜索深度（depth_nodes 模式）
    pub nnue_depth: i32,
    /// 当前默认 NNUE 节点预算（nodes/depth_nodes 模式）
    pub nnue_budget: u64,
    /// 当前默认 NNUE 时间预算毫秒（time 模式）
    pub nnue_time_ms: u64,
    /// 当前默认搜索硬超时毫秒（全模式必选生效）
    pub search_timeout_ms: u64,
}

impl From<crate::app::player::EngineParams> for SettingsResp {
    fn from(p: crate::app::player::EngineParams) -> Self {
        Self {
            mcts_sims: p.mcts_sims,
            engine_budget: p.engine_budget,
            nnue_budget_mode: p.nnue_budget.mode.as_str().to_string(),
            nnue_depth: p.nnue_budget.depth,
            nnue_budget: p.nnue_budget.nodes,
            nnue_time_ms: p.nnue_budget.time_ms,
            search_timeout_ms: p.nnue_budget.timeout_ms,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// 模型文件路径（创建对局 / 绑定模型时作为 modelFile 使用）
    pub file: String,
    /// 是否已加载到内存（随用随加载：未加载仅表示尚无对局使用过它）
    pub loaded: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResp {
    pub ok: bool,
    pub models: Vec<ModelInfo>,
    pub sessions: usize,
    pub device: String,
    /// NN 推理缓存条目上限（0 = 关闭）
    pub nn_cache_entries: usize,
}

/// POST /api/engine/move 响应：给定局面的引擎决策（不落子、无会话状态）。
#[derive(Debug, Serialize)]
pub struct EngineMoveResp {
    /// 变体 ID（由 BFEN 解出）
    pub variant: String,
    /// 该局面的行棋方："red" / "black"
    pub current_player: String,
    /// 动作空间索引
    pub action: usize,
    /// 源格索引（翻棋时 == to）
    pub from: usize,
    /// 目标格索引
    pub to: usize,
    /// 是否机会动作（翻棋 / 吃暗子，结果有随机性）
    pub is_chance: bool,
    /// 是否纯翻棋（action < reveal_actions_count；翻棋必为机会动作）
    pub is_flip: bool,
    /// MCTS+ONNX 路径的搜索统计（模型为 .onnx 时）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcts_stats: Option<MctsStatsDto>,
    /// NNUE 路径的搜索统计（模型为 .nnue 时）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_stats: Option<NnueStatsDto>,
}

/// GET /api/capabilities 响应：编译期启用的推理后端
/// （前端据此隐藏不可用的对手与模型类型；对齐前端 types.ts `Capabilities`）。
#[derive(Debug, Serialize)]
pub struct CapabilitiesResp {
    /// TorchScript 推理（MctsDL / .pt 模型）
    pub torch: bool,
    /// ONNX Runtime 推理（MctsOnnx / .onnx 模型）
    pub onnx: bool,
    /// NNUE 推理（无外部依赖，恒可用）
    pub nnue: bool,
}

/// GET /api/models 响应：模型目录与其中的模型文件（对齐前端 types.ts `ModelList`）。
#[derive(Debug, Serialize)]
pub struct ModelListResp {
    /// 模型目录路径（服务端 --models 参数）
    pub dir: String,
    pub models: Vec<ModelEntryDto>,
}

/// 单个模型文件条目（对齐前端 types.ts `ModelEntry`）。
#[derive(Debug, Serialize)]
pub struct ModelEntryDto {
    /// 文件名（展示用）
    pub name: String,
    /// 完整路径（创建对局 / 绑定模型时作为 modelFile 使用）
    pub path: String,
}

/// GET /api/games/:id/move-action 响应：坐标对 → 动作空间索引
/// （几何映射，只取决于变体；`null` = 坐标不构成任何动作）。
#[derive(Debug, Serialize)]
pub struct MoveActionResp {
    pub action: Option<usize>,
}

// 序列化辅助

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

/// 胜者字符串转前端 StepResult 的数字编码（1=红 / -1=黑 / 0=平局 / null）。
pub fn winner_code(w: Option<&str>) -> Option<i32> {
    match w {
        Some("red") => Some(1),
        Some("black") => Some(-1),
        Some("draw") => Some(0),
        _ => None,
    }
}
