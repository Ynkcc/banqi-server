use bqsrv_config::EngineParams;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreateGameReq {
    #[serde(default)]
    #[ts(optional)]
    pub variant: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub slots: Option<SlotsReq>,
    #[serde(default)]
    #[ts(optional)]
    pub fen: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<OpeningScenarioTs>")]
    pub scenario: Option<banqi_core::core::env::OpeningScenario>,
}
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SlotsReq {
    #[serde(default)]
    #[ts(optional)]
    pub red: Option<SlotReq>,
    #[serde(default)]
    #[ts(optional)]
    pub black: Option<SlotReq>,
}
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SlotReq {
    pub kind: String,
    #[serde(default)]
    #[ts(optional)]
    pub model_file: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub params: Option<SlotParamsReq>,
    #[serde(default)]
    #[ts(optional)]
    pub agent_id: Option<String>,

    #[serde(default)]
    #[ts(optional)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BudgetParams {
    #[serde(default)]
    #[ts(optional)]
    pub engine_budget: Option<u64>,
    #[serde(default)]
    #[ts(optional)]
    pub engine_budget_mode: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub engine_depth: Option<i32>,
    #[serde(default)]
    #[ts(optional)]
    pub engine_time_ms: Option<u64>,
    #[serde(default)]
    #[ts(optional)]
    pub nnue_budget_mode: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub nnue_depth: Option<i32>,
    #[serde(default)]
    #[ts(optional)]
    pub nnue_budget: Option<u64>,
    #[serde(default)]
    #[ts(optional)]
    pub nnue_time_ms: Option<u64>,
    #[serde(default)]
    #[ts(optional)]
    pub search_timeout_ms: Option<u64>,
}
#[derive(Debug, Clone, Deserialize, Default, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SlotParamsReq {
    #[serde(default)]
    #[ts(optional)]
    pub mcts_sims: Option<usize>,
    #[serde(flatten)]
    pub budget: BudgetParams,
}
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PutSlotsReq {
    #[serde(default)]
    #[ts(optional)]
    pub red: Option<SlotReq>,
    #[serde(default)]
    #[ts(optional)]
    pub black: Option<SlotReq>,
}
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DryRunReq {
    #[serde(default)]
    #[ts(optional)]
    pub kind: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub model_file: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub agent_id: Option<String>,

    #[serde(default)]
    #[ts(optional)]
    pub params: Option<SlotParamsReq>,
}
#[derive(Debug, Default, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunReq {
    #[serde(default)]
    #[ts(optional)]
    pub agent_id: Option<String>,
}
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreateAgentReq {
    pub variant: String,
    pub kind: String,
    #[serde(default)]
    #[ts(optional)]
    pub model_file: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub params: Option<SlotParamsReq>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AgentResp {
    pub id: String,
    pub variant: String,
    #[serde(flatten)]
    pub slot: SlotView,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct AgentsListResp {
    pub agents: Vec<AgentResp>,
}
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SettingsReq {
    #[serde(default)]
    #[ts(optional)]
    pub mcts_sims: Option<usize>,
    #[serde(flatten)]
    pub budget: BudgetParams,
}
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MoveReq {
    pub action: usize,

    #[serde(default)]
    #[ts(optional)]
    pub reveal: Option<i64>,

    #[serde(default)]
    #[ts(optional)]
    pub mover: Option<String>,

    #[serde(default)]
    #[ts(optional)]
    pub note: Option<String>,

    #[serde(default)]
    #[ts(optional)]
    pub diag: Option<StepStatsDto>,
}
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct UndoReq {
    #[serde(default)]
    #[ts(optional)]
    pub steps: Option<usize>,
}
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EngineMoveReq {
    pub fen: String,

    #[serde(default)]
    #[ts(optional, type = "unknown[]")]
    pub frames: Option<Vec<serde_json::Value>>,

    #[serde(default)]
    #[ts(optional)]
    pub engine: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub mcts_sims: Option<usize>,

    #[serde(flatten)]
    pub budget: BudgetParams,
    #[serde(default)]
    #[ts(optional)]
    pub model_file: Option<String>,
}
#[derive(Debug, Clone, Copy, Serialize, TS)]
#[ts(export)]
pub struct ActionView {
    pub action: usize,
    pub from: usize,
    pub to: usize,
    pub is_chance: bool,
}
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct GameStateView {
    pub board: Vec<String>,
    pub current_player: String,
    pub move_counter: u32,
    pub total_step_counter: u32,
    pub dead_red: Vec<String>,
    pub dead_black: Vec<String>,
    pub hidden_red: Vec<String>,
    pub hidden_black: Vec<String>,
    pub action_masks: Vec<i32>,
    pub reveal_probabilities: Vec<f32>,
    pub bitboards: std::collections::HashMap<String, Vec<bool>>,
    pub hp_red: i32,
    pub hp_black: i32,
    pub variant: String,
    pub fen: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SlotView {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcts_sims: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub engine_budget: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub budget_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub budget_depth: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub budget: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub budget_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub search_timeout_ms: Option<u64>,
}
impl SlotView {
    pub fn human() -> Self {
        Self {
            kind: "Human".to_string(),
            label: None,
            agent_id: None,
            model_file: None,
            mcts_sims: None,
            engine_budget: None,
            budget_mode: None,
            budget_depth: None,
            budget: None,
            budget_time_ms: None,
            search_timeout_ms: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SlotsView {
    pub red: SlotView,
    pub black: SlotView,
}
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ExSearchStats {
    pub mode: String,
    pub depth: i32,
    pub budget: u64,
    pub time_limit_ms: u64,
    pub nodes: u64,
    pub reached_depth: i32,
    pub elapsed_ms: u64,
}
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ExNodeSummary {
    pub id: usize,
    pub action: usize,
    pub value: f32,
    pub alpha: f32,
    pub beta: f32,
    pub depth: i32,
    pub flag: String,
    pub mark: Option<String>,
    pub is_chance: bool,
    pub is_outcome: bool,

    pub is_flip: bool,
    pub chance_prob: f32,
    pub is_root: bool,
    pub is_null_probe: bool,
    pub player: String,
    pub child_count: usize,

    pub spent_nodes: u64,
}
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ExRootInfo {
    pub root: ExNodeSummary,
    pub chosen_action: usize,
    pub stats: ExSearchStats,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
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
    #[ts(optional)]
    pub score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcts_stats: Option<MctsStatsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub nnue_stats: Option<NnueStatsDto>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub search_info: Option<ExRootInfo>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GameStateResp {
    #[serde(flatten)]
    pub game: GameStateView,
    pub game_id: String,
    pub version: u64,
    pub max_steps: usize,
    pub over: bool,
    pub terminated: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub over_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub winner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_action: Option<usize>,
    pub legal_actions: Vec<ActionView>,
    pub history: Vec<HistoryMoveView>,
    pub slots: SlotsView,
    pub can_run: bool,
    pub can_undo: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HistoryMoveView {
    pub ply: usize,
    pub player: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mover: Option<String>,
    pub action: usize,
    pub from: usize,
    pub to: usize,
    pub is_flip: bool,
    pub is_chance: bool,
    pub is_capture: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revealed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub captured: Option<String>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct AppliedAction {
    pub player: String,
    pub action: ActionView,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MctsStatsDto {
    pub reused: bool,
    pub budget: usize,
    pub actual_sims: u32,
    pub max_depth: usize,
    pub nodes: usize,
    pub elapsed_ms: u64,
}
impl From<&banqi_engine::engine::MctsSearchStats> for MctsStatsDto {
    fn from(s: &banqi_engine::engine::MctsSearchStats) -> Self {
        Self {
            reused: s.reused,
            budget: s.budget,
            actual_sims: s.actual_sims,
            max_depth: s.max_depth,
            nodes: s.nodes,
            elapsed_ms: s.elapsed_ms,
        }
    }
}
#[derive(Debug, Clone)]
pub struct NnueSearchStats {
    pub engine: &'static str,
    pub mode: &'static str,
    pub depth: i32,
    pub budget: u64,
    pub time_limit_ms: u64,
    pub nodes: u64,
    pub reached_depth: i32,
    pub elapsed_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
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
impl From<&NnueSearchStats> for NnueStatsDto {
    fn from(s: &NnueSearchStats) -> Self {
        Self {
            engine: s.engine.to_string(),
            mode: s.mode.to_string(),
            depth: s.depth,
            budget: s.budget,
            time_limit_ms: s.time_limit_ms,
            nodes: s.nodes,
            reached_depth: s.reached_depth,
            elapsed_ms: s.elapsed_ms,
        }
    }
}
impl From<&NnueSearchStats> for ExSearchStats {
    fn from(s: &NnueSearchStats) -> Self {
        Self {
            mode: s.mode.to_string(),
            depth: s.depth,
            budget: s.budget,
            time_limit_ms: s.time_limit_ms,
            nodes: s.nodes,
            reached_depth: s.reached_depth,
            elapsed_ms: s.elapsed_ms,
        }
    }
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct MoveResp {
    pub state: GameStateResp,
    pub terminated: bool,
    pub truncated: bool,
    pub winner: Option<i32>,
    pub actions: Vec<AppliedAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcts_stats: Option<MctsStatsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
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
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GameSummary {
    pub game_id: String,
    pub variant: String,
    pub version: u64,
    pub current_player: String,
    pub total_steps: usize,
    pub over: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub winner: Option<String>,
    pub slots: SlotsView,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GameListResp {
    pub games: Vec<GameSummary>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ArchivedGameSummary {
    pub game_id: String,
    pub variant: String,
    pub created_at: u64,
    pub finished_at: u64,
    pub total_steps: usize,
    pub over: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub winner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub over_reason: Option<String>,
    pub slots: SlotsView,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReplayActionDto {
    pub action: usize,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revealed: Option<char>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StepStatsDto {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional)]
    pub mcts: Option<MctsStatsDto>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional)]
    pub nnue: Option<NnueStatsDto>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional)]
    pub note: Option<String>,
}
impl StepStatsDto {
    pub fn is_empty(&self) -> bool {
        self.mcts.is_none() && self.nnue.is_none() && self.note.is_none()
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, TS)]
#[ts(export)]
pub struct HiddenBagDto {
    pub red: [u8; 7],
    pub black: [u8; 7],
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ReplayStepDto {
    pub ply: usize,
    pub player: String,
    pub action: usize,
    pub from: usize,
    pub to: usize,

    pub kind: String,
    pub is_chance: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revealed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub captured: Option<String>,
    pub move_counter: u32,

    pub scores: [i32; 2],

    pub board: Vec<String>,

    pub revealed_at: Vec<bool>,
    pub hidden_bag: HiddenBagDto,

    pub board_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stats: Option<StepStatsDto>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ReplayResp {
    pub game_id: String,
    pub variant: String,

    pub termination: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub winner: Option<String>,

    pub side_red: String,
    pub side_black: String,
    pub initial_fen: String,
    pub initial_board: Vec<String>,
    pub initial_shown: Vec<bool>,
    pub initial_scores: [i32; 2],
    pub initial_hidden_bag: HiddenBagDto,
    pub final_board: Vec<String>,
    pub final_shown: Vec<bool>,

    pub final_scores: [i32; 2],

    pub final_hidden_bag: HiddenBagDto,

    pub board_source: String,
    pub moves: Vec<ReplayStepDto>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ArchivedGameDetail {
    #[serde(flatten)]
    pub summary: ArchivedGameSummary,
    pub initial_fen: String,
    pub final_fen: String,
    pub actions: Vec<ReplayActionDto>,
    pub moves: Vec<HistoryMoveView>,

    #[serde(default)]
    pub snapshots: Vec<String>,

    #[serde(default)]
    pub final_snapshot: Option<String>,

    #[serde(default)]
    pub step_stats: Vec<Option<StepStatsDto>>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct RecentGamesResp {
    pub games: Vec<ArchivedGameSummary>,
}
#[derive(Debug, Deserialize)]
pub struct RecentGamesQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ReplayGameReq {
    #[serde(default)]
    #[ts(optional)]
    pub slots: Option<SlotsReq>,

    #[serde(default)]
    #[ts(optional)]
    pub from_ply: Option<usize>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GameCreatedResp {
    pub game_id: String,
    pub state: GameStateResp,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct VariantInfo {
    pub id: String,
    pub label: String,
    pub rows: usize,
    pub cols: usize,
    pub initial_health: i32,
    pub max_consecutive_moves_for_draw: usize,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct VariantsResp {
    pub variants: Vec<VariantInfo>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct VariantScenarios {
    pub variant: String,
    #[ts(as = "Vec<ScenarioInfoTs>")]
    pub scenarios: Vec<&'static banqi_core::core::env::ScenarioInfo>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ScenariosResp {
    pub variants: Vec<VariantScenarios>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SettingsResp {
    pub mcts_sims: usize,
    pub engine_budget: u64,
    pub engine_budget_mode: String,
    pub engine_depth: i32,
    pub engine_time_ms: u64,
    pub nnue_budget_mode: String,
    pub nnue_depth: i32,
    pub nnue_budget: u64,
    pub nnue_time_ms: u64,
    pub search_timeout_ms: u64,
}
impl From<EngineParams> for SettingsResp {
    fn from(p: EngineParams) -> Self {
        Self {
            mcts_sims: p.mcts_sims,
            engine_budget: p.engine_budget.nodes,
            engine_budget_mode: p.engine_budget.mode.as_str().to_string(),
            engine_depth: p.engine_budget.depth,
            engine_time_ms: p.engine_budget.time_ms,
            nnue_budget_mode: p.nnue_budget.mode.as_str().to_string(),
            nnue_depth: p.nnue_budget.depth,
            nnue_budget: p.nnue_budget.nodes,
            nnue_time_ms: p.nnue_budget.time_ms,
            search_timeout_ms: p.nnue_budget.timeout_ms,
        }
    }
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub file: String,
    pub loaded: bool,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct HealthResp {
    pub ok: bool,
    pub models: Vec<ModelInfo>,
    pub sessions: usize,
    pub device: String,
    pub nn_cache_entries: usize,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct EngineMoveResp {
    pub variant: String,
    pub current_player: String,
    pub action: usize,
    pub from: usize,
    pub to: usize,
    pub is_chance: bool,
    pub is_flip: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcts_stats: Option<MctsStatsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub nnue_stats: Option<NnueStatsDto>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct CapabilitiesResp {
    pub torch: bool,
    pub onnx: bool,
    pub nnue: bool,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ModelListResp {
    pub dir: String,
    pub models: Vec<ModelEntryDto>,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ModelEntryDto {
    pub name: String,
    pub path: String,
}
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct MoveActionResp {
    pub action: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "ScenarioParamInfo")]
pub struct ScenarioParamInfoTs {
    pub name: String,
    pub label: String,
    pub required: bool,
    pub default_value: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "ScenarioInfo")]
pub struct ScenarioInfoTs {
    pub id: String,
    pub label: String,
    pub description: String,
    pub params: Vec<ScenarioParamInfoTs>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[ts(export, rename = "AllRevealed")]
pub struct AllRevealedTs {}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[ts(export, rename = "CannonStrikeGeneral")]
pub struct CannonStrikeGeneralTs {
    pub cannon_sq: Option<usize>,
    pub general_sq: Option<usize>,
    pub random_reveals: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[ts(export, rename = "OpeningScenario")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpeningScenarioTs {
    CannonStrikeGeneral(CannonStrikeGeneralTs),
    AllRevealed(AllRevealedTs),
}

pub fn player_label(p: banqi_core::core::env::types::Player) -> String {
    match p {
        banqi_core::core::env::types::Player::Red => "Red".to_string(),
        banqi_core::core::env::types::Player::Black => "Black".to_string(),
    }
}
pub fn player_str(p: banqi_core::core::env::types::Player) -> String {
    player_label(p)
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

pub use bq_util::hex_encode;

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    bq_util::hex_decode(s).ok()
}

pub fn termination_label(reason: Option<&str>) -> String {
    use banqi_core::core::env::{OverReason, Termination};
    const REASONS: [OverReason; 3] = [
        OverReason::Checkmate,
        OverReason::Draw,
        OverReason::MaxSteps,
    ];
    let termination = reason
        .and_then(|r| REASONS.iter().copied().find(|o| o.as_str() == r))
        .map(Termination::from_over_reason)
        .unwrap_or(Termination::Unspecified);
    termination.as_str().to_string()
}

pub fn describe_side(slot: &SlotView) -> String {
    if let Some(label) = slot.label.as_deref().filter(|s| !s.trim().is_empty()) {
        return label.to_string();
    }
    match slot.kind.as_str() {
        "Human" => "人类玩家".to_string(),
        "Agent" => {
            let id = slot.agent_id.as_deref().unwrap_or("?");
            let short: String = id.chars().take(8).collect();
            match &slot.model_file {
                Some(m) => format!("Agent({short}) · {}", model_short(m)),
                None => format!("Agent({short})"),
            }
        }

        k if slot.model_file.is_none() => format!("规则策略（{k}）"),
        k => format!(
            "{k} · {}",
            model_short(slot.model_file.as_deref().unwrap_or(""))
        ),
    }
}
fn model_short(file: &str) -> String {
    std::path::Path::new(file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file)
        .to_string()
}

pub fn piece_from_display_code(code: i64) -> Option<banqi_core::core::env::types::Piece> {
    use banqi_core::core::env::types::Piece;
    let (player, piece_type) = banqi_core::core::env::piece_from_code(u8::try_from(code).ok()?)?;
    Some(Piece::new(piece_type, player))
}

pub fn mover_label(mover: Option<&str>, fallback: &str) -> String {
    match mover {
        Some("engine") => "引擎".to_string(),
        Some("enemy") => "敌人".to_string(),
        Some(other) => other.to_string(),
        None => fallback.to_string(),
    }
}

#[cfg(test)]
mod dto_tests {
    use super::*;

    #[test]
    fn core_type_mirrors_match_json() {
        for info in banqi_core::core::env::scenario::all_scenario_infos() {
            let json = serde_json::to_value(info).expect("ScenarioInfo 序列化应成功");
            let mirror: ScenarioInfoTs =
                serde_json::from_value(json).expect("ScenarioInfoTs 反序列化应成功");
            assert_eq!(mirror.id, info.id);
            assert_eq!(mirror.params.len(), info.params.len());
        }
        let s = banqi_core::core::env::OpeningScenario::with_general(5, 19);
        let json = serde_json::to_value(s).expect("OpeningScenario 序列化应成功");
        assert_eq!(
            json.get("kind").and_then(|k| k.as_str()),
            Some("cannon_strike_general")
        );
        let mirror: OpeningScenarioTs =
            serde_json::from_value(json).expect("OpeningScenarioTs 反序列化应成功");
        assert!(matches!(mirror, OpeningScenarioTs::CannonStrikeGeneral(_)));
        let all = banqi_core::core::env::OpeningScenario::all_revealed();
        let json = serde_json::to_value(all).expect("序列化应成功");
        let mirror: OpeningScenarioTs = serde_json::from_value(json).expect("反序列化应成功");
        assert!(matches!(mirror, OpeningScenarioTs::AllRevealed(_)));
    }
}
