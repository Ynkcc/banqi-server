use serde::{Deserialize, Serialize};
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGameReq {
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub slots: Option<SlotsReq>,
    #[serde(default)]
    pub fen: Option<String>,
    #[serde(default)]
    pub scenario: Option<banqi_core::core::env::OpeningScenario>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotsReq {
    #[serde(default)]
    pub red: Option<SlotReq>,
    #[serde(default)]
    pub black: Option<SlotReq>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotReq {
    pub kind: String,
    #[serde(default)]
    pub model_file: Option<String>,
    #[serde(default)]
    pub params: Option<SlotParamsReq>,
    #[serde(default)]
    pub agent_id: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SlotParamsReq {
    #[serde(default)]
    pub mcts_sims: Option<usize>,
    #[serde(default)]
    pub engine_budget: Option<u64>,
    #[serde(default)]
    pub nnue_budget_mode: Option<String>,
    #[serde(default)]
    pub nnue_depth: Option<i32>,
    #[serde(default)]
    pub nnue_budget: Option<u64>,
    #[serde(default)]
    pub nnue_time_ms: Option<u64>,
    #[serde(default)]
    pub search_timeout_ms: Option<u64>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutSlotsReq {
    #[serde(default)]
    pub red: Option<SlotReq>,
    #[serde(default)]
    pub black: Option<SlotReq>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunReq {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub model_file: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReq {
    #[serde(default)]
    pub agent_id: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAgentReq {
    pub variant: String,
    pub kind: String,
    #[serde(default)]
    pub model_file: Option<String>,
    #[serde(default)]
    pub params: Option<SlotParamsReq>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentResp {
    pub id: String,
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
    #[serde(default)]
    pub mcts_sims: Option<usize>,
    #[serde(default)]
    pub engine_budget: Option<u64>,
    #[serde(default)]
    pub nnue_budget_mode: Option<String>,
    #[serde(default)]
    pub nnue_depth: Option<i32>,
    #[serde(default)]
    pub nnue_budget: Option<u64>,
    #[serde(default)]
    pub nnue_time_ms: Option<u64>,
    #[serde(default)]
    pub search_timeout_ms: Option<u64>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveReq {
    pub action: usize,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoReq {
    #[serde(default)]
    pub steps: Option<usize>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineMoveReq {
    pub fen: String,
    #[serde(default)]
    pub mcts_sims: Option<usize>,
    #[serde(default)]
    pub nnue_budget_mode: Option<String>,
    #[serde(default)]
    pub nnue_depth: Option<i32>,
    #[serde(default)]
    pub nnue_budget: Option<u64>,
    #[serde(default)]
    pub nnue_time_ms: Option<u64>,
    #[serde(default)]
    pub search_timeout_ms: Option<u64>,
    #[serde(default)]
    pub model_file: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct ActionView {
    pub action: usize,
    pub from: usize,
    pub to: usize,
    pub is_chance: bool,
}
#[derive(Debug, Serialize)]
pub struct SlotView {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcts_sims: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_budget: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_depth: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_budget: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_time_ms: Option<u64>,
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
#[derive(Debug, Serialize)]
pub struct SlotsView {
    pub red: SlotView,
    pub black: SlotView,
}
#[derive(Debug, Serialize)]
pub struct SuggestionResp {
    pub version: u64,
    pub player: String,
    pub action: usize,
    pub from: usize,
    pub to: usize,
    pub is_chance: bool,
    pub is_flip: bool,
    pub is_capture: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcts_stats: Option<MctsStatsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_stats: Option<NnueStatsDto>,
}
#[derive(Debug, Serialize)]
pub struct GameStateResp {
    #[serde(flatten)]
    pub game: crate::app::view::GameStateView,
    pub game_id: String,
    pub version: u64,
    pub max_steps: usize,
    pub over: bool,
    pub terminated: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_action: Option<usize>,
    pub legal_actions: Vec<ActionView>,
    pub history: Vec<HistoryMoveView>,
    pub slots: SlotsView,
    pub can_run: bool,
    pub can_undo: bool,
}
#[derive(Debug, Serialize)]
pub struct HistoryMoveView {
    pub ply: usize,
    pub player: String,
    pub action: usize,
    pub from: usize,
    pub to: usize,
    pub is_flip: bool,
    pub is_chance: bool,
    pub is_capture: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revealed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct AppliedAction {
    pub player: String,
    pub action: ActionView,
}
#[derive(Debug, Clone, Serialize)]
pub struct MctsStatsDto {
    pub reused: bool,
    pub budget: usize,
    pub actual_sims: u32,
    pub max_depth: usize,
    pub nodes: usize,
    pub elapsed_ms: u64,
}
impl MctsStatsDto {
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
#[derive(Debug, Clone, Serialize)]
pub struct NnueStatsDto {
    pub engine: String,
    pub mode: String,
    pub depth: i32,
    pub budget: u64,
    pub time_limit_ms: u64,
    pub nodes: u64,
    pub reached_depth: i32,
    pub elapsed_ms: u64,
}
#[derive(Debug, Serialize)]
pub struct MoveResp {
    pub state: GameStateResp,
    pub terminated: bool,
    pub truncated: bool,
    pub winner: Option<i32>,
    pub actions: Vec<AppliedAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcts_stats: Option<MctsStatsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_stats: Option<NnueStatsDto>,
}
impl MoveResp {
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
#[derive(Debug, Serialize)]
pub struct VariantInfo {
    pub id: String,
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
pub struct VariantScenarios {
    pub variant: String,
    pub scenarios: Vec<&'static banqi_core::core::env::ScenarioInfo>,
}
#[derive(Debug, Serialize)]
pub struct ScenariosResp {
    pub variants: Vec<VariantScenarios>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsResp {
    pub mcts_sims: usize,
    pub engine_budget: u64,
    pub nnue_budget_mode: String,
    pub nnue_depth: i32,
    pub nnue_budget: u64,
    pub nnue_time_ms: u64,
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
    pub file: String,
    pub loaded: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResp {
    pub ok: bool,
    pub models: Vec<ModelInfo>,
    pub sessions: usize,
    pub device: String,
    pub nn_cache_entries: usize,
}
#[derive(Debug, Serialize)]
pub struct EngineMoveResp {
    pub variant: String,
    pub current_player: String,
    pub action: usize,
    pub from: usize,
    pub to: usize,
    pub is_chance: bool,
    pub is_flip: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcts_stats: Option<MctsStatsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nnue_stats: Option<NnueStatsDto>,
}
#[derive(Debug, Serialize)]
pub struct CapabilitiesResp {
    pub torch: bool,
    pub onnx: bool,
    pub nnue: bool,
}
#[derive(Debug, Serialize)]
pub struct ModelListResp {
    pub dir: String,
    pub models: Vec<ModelEntryDto>,
}
#[derive(Debug, Serialize)]
pub struct ModelEntryDto {
    pub name: String,
    pub path: String,
}
#[derive(Debug, Serialize)]
pub struct MoveActionResp {
    pub action: Option<usize>,
}
pub fn player_str(p: banqi_core::core::env::types::Player) -> String {
    match p {
        banqi_core::core::env::types::Player::Red => "red".to_string(),
        banqi_core::core::env::types::Player::Black => "black".to_string(),
    }
}
pub fn winner_str(w: Option<i32>) -> Option<String> {
    match w {
        Some(1) => Some("red".to_string()),
        Some(-1) => Some("black".to_string()),
        Some(0) => Some("draw".to_string()),
        _ => None,
    }
}
pub fn winner_code(w: Option<&str>) -> Option<i32> {
    match w {
        Some("red") => Some(1),
        Some("black") => Some(-1),
        Some("draw") => Some(0),
        _ => None,
    }
}
