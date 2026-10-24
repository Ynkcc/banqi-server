use crate::app::envops::{StepOutcome, coords_for_action};
use crate::app::mcts_view::MctsTree;
pub use crate::app::models::EngineKind;
use crate::app::player::{Agent, EngineParams, PlayerSlot, Slots, StrategyKind};
use crate::app::search::{BudgetMode, SearchBudget};
use crate::app::session::SnapshotSession;
use crate::app::sync::{read_lock, write_lock};
use crate::config::ServerConfig;
use crate::dto::{ActionView, GameStateResp, HistoryMoveView, SlotView, SlotsView, winner_str};
use crate::error::ApiError;
use banqi_core::core::env::DarkChessEnv;
use banqi_core::core::env::{PositionSnapshot, Variant};
use banqi_engine::nnue::NnueEvaluator;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
pub type Models = crate::app::models::ModelPool;
pub struct GameSession {
    pub core: SnapshotSession,
    pub slots: Slots,
    pub mcts_tree: Option<crate::mcts::MctsTreeBox>,
    pub expectimax_tree: Option<crate::app::expectimax_view::ExpectimaxTree>,
}
impl GameSession {
    #[allow(dead_code)]
    pub fn new(variant: Variant, slots: Slots) -> Self {
        Self {
            core: SnapshotSession::new(variant),
            slots,
            mcts_tree: None,
            expectimax_tree: None,
        }
    }
    pub fn new_with_scenario(
        variant: Variant,
        scenario: Option<&banqi_core::core::env::OpeningScenario>,
        slots: Slots,
    ) -> Result<Self, ApiError> {
        let core = SnapshotSession::new_with_scenario(variant, scenario)?;
        Ok(Self {
            core,
            slots,
            mcts_tree: None,
            expectimax_tree: None,
        })
    }
    pub fn from_snapshot(variant: Variant, snapshot: PositionSnapshot, slots: Slots) -> Self {
        Self {
            core: SnapshotSession::from_snapshot(variant, snapshot),
            slots,
            mcts_tree: None,
            expectimax_tree: None,
        }
    }
    pub fn over_state(&self) -> (bool, bool, Option<i32>) {
        self.core.over_state()
    }
    pub fn apply_action(&mut self, action: usize) -> Result<StepOutcome, ApiError> {
        self.core.apply_action(action).map_err(ApiError::from)
    }
    pub fn undo(&mut self, steps: usize) -> Result<(), ApiError> {
        self.core.undo_steps(steps).map_err(ApiError::from)
    }
    pub fn undo_to_current_player(&mut self) -> Result<(), ApiError> {
        self.core.undo_to_current_player().map_err(ApiError::from)
    }
    #[allow(dead_code)]
    pub fn legal_actions(&self) -> Vec<usize> {
        self.core.legal_actions()
    }
    pub fn action_view(&self, action: usize) -> ActionView {
        let coords = coords_for_action(self.core.variant, action).unwrap_or_default();
        ActionView {
            action,
            from: coords.first().copied().unwrap_or(action),
            to: coords.last().copied().unwrap_or(action),
            is_chance: self.core.is_chance_action(action),
        }
    }
    pub fn strategy_to_move(&self) -> bool {
        let (terminated, truncated, _) = self.over_state();
        !terminated && !truncated && !self.slots.side(self.core.current.current_player).is_human()
    }
    pub fn undoable(&self) -> bool {
        self.core.undoable()
    }
    pub fn take_reusable_mcts_tree(&mut self) -> Option<MctsTree<DarkChessEnv>> {
        let tree = self.mcts_tree.take()?;
        tree.into_any()
            .downcast::<MctsTree<DarkChessEnv>>()
            .ok()
            .map(|b| *b)
    }
    pub fn view(&self, game_id: Uuid) -> GameStateResp {
        let slots_view = SlotsView {
            red: slot_view(&self.slots.red),
            black: slot_view(&self.slots.black),
        };
        let v = crate::app::view::game_view(&self.core);
        let s = &v.scalars;
        let over = s.terminated || s.truncated;
        let legal_actions: Vec<ActionView> = v
            .legal_actions
            .iter()
            .map(|a| ActionView {
                action: a.action,
                from: a.from,
                to: a.to,
                is_chance: a.is_chance,
            })
            .collect();
        let history: Vec<HistoryMoveView> = v
            .history
            .iter()
            .map(|e| HistoryMoveView {
                ply: e.ply,
                player: crate::app::common::player_label(e.player),
                action: e.action,
                from: e.from,
                to: e.to,
                is_flip: e.is_flip,
                is_chance: e.is_chance,
                is_capture: e.is_capture,
                revealed: e.revealed.map(|p| p.short_name().to_string()),
                captured: e.captured.map(|p| p.short_name().to_string()),
            })
            .collect();
        GameStateResp {
            game: crate::app::view::game_state_view(&self.core.current),
            game_id: game_id.to_string(),
            version: self.core.version,
            max_steps: s.max_steps,
            over,
            terminated: s.terminated,
            truncated: s.truncated,
            over_reason: s.over_reason.map(|r| r.as_str().to_string()),
            winner: winner_str(s.winner),
            last_action: s.last_action,
            legal_actions,
            history,
            slots: slots_view,
            can_run: self.strategy_to_move(),
            can_undo: self.undoable(),
        }
    }
    pub fn summary(&self, game_id: Uuid) -> crate::dto::GameSummary {
        let (_, _, winner) = self.over_state();
        let v = self.view(game_id);
        crate::dto::GameSummary {
            game_id: game_id.to_string(),
            variant: self.core.variant.as_str().to_string(),
            version: self.core.version,
            current_player: v.game.current_player,
            total_steps: v.game.total_step_counter as usize,
            over: v.over,
            winner: winner_str(winner),
            slots: v.slots,
        }
    }
}
pub fn strategy_slot_view(
    kind: StrategyKind,
    model_file: Option<String>,
    params: EngineParams,
) -> SlotView {
    let b = &params.nnue_budget;
    let (budget_mode, nnue_depth, nnue_budget, nnue_time_ms, search_timeout_ms) = match kind {
        StrategyKind::ExpectimaxNnue => (
            Some(b.mode.as_str().to_string()),
            (b.mode == BudgetMode::DepthNodes).then_some(b.depth),
            (b.mode != BudgetMode::Time).then_some(b.nodes),
            (b.mode == BudgetMode::Time).then_some(b.time_ms),
            Some(b.timeout_ms),
        ),
        StrategyKind::Expectimax => (None, None, None, None, Some(b.timeout_ms)),
        _ => (None, None, None, None, None),
    };
    SlotView {
        kind: kind.as_str().to_string(),
        agent_id: None,
        model_file,
        mcts_sims: kind.is_mcts().then_some(params.mcts_sims),
        engine_budget: (kind == StrategyKind::Expectimax).then_some(params.engine_budget),
        budget_mode,
        nnue_depth,
        nnue_budget,
        nnue_time_ms,
        search_timeout_ms,
    }
}
fn slot_view(slot: &PlayerSlot) -> SlotView {
    match slot {
        PlayerSlot::Human => SlotView::human(),
        PlayerSlot::Agent(id) => SlotView {
            kind: "Agent".to_string(),
            agent_id: Some(id.to_string()),
            ..SlotView::human()
        },
        PlayerSlot::Strategy {
            kind,
            model_file,
            params,
        } => strategy_slot_view(*kind, model_file.clone(), *params),
    }
}
pub struct AppState {
    pub config: ServerConfig,
    pub params: RwLock<EngineParams>,
    pub models: Models,
    pub games: RwLock<HashMap<Uuid, GameSession>>,
    pub agents: RwLock<HashMap<Uuid, Agent>>,
}
pub type SharedState = std::sync::Arc<AppState>;
pub fn parse_id<T: std::str::FromStr>(raw: &str, label: &str) -> Result<T, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::BadRequest(format!("非法{label}: {raw}")))
}
pub fn parse_game_id(raw: &str) -> Result<Uuid, ApiError> {
    parse_id(raw, "对局 ID")
}
pub fn store_manual_search_trees(
    games: &mut HashMap<Uuid, GameSession>,
    game_id: Uuid,
    expect_version: u64,
    mcts_tree: Option<crate::mcts::MctsTreeBox>,
    ex_tree: Option<crate::app::expectimax_view::ExpectimaxTree>,
) -> bool {
    let Some(s) = games.get_mut(&game_id) else {
        return false;
    };
    if s.core.version != expect_version {
        tracing::info!(
            game = %game_id,
            "手动搜索结果作废: 版本号 {expect_version} != {}（期间对局已被修改）",
            s.core.version
        );
        return false;
    }
    if mcts_tree.is_some() {
        s.mcts_tree = mcts_tree;
    }
    if ex_tree.is_some() {
        s.expectimax_tree = ex_tree;
    }
    true
}
pub fn get_session<'a>(
    games: &'a HashMap<Uuid, GameSession>,
    label: &str,
    game_id: Uuid,
) -> Result<&'a GameSession, ApiError> {
    games
        .get(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(label.to_string()))
}
pub fn get_mut_session<'a>(
    games: &'a mut HashMap<Uuid, GameSession>,
    label: &str,
    game_id: Uuid,
) -> Result<&'a mut GameSession, ApiError> {
    games
        .get_mut(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(label.to_string()))
}
pub fn mcts_tree_of(s: &GameSession) -> Result<&dyn crate::mcts::MctsTreeView, ApiError> {
    s.mcts_tree.as_deref().ok_or(ApiError::MctsTreeMissing)
}
impl AppState {
    pub fn new(config: ServerConfig) -> Self {
        let models = Models::new(
            &config.models_dir,
            config.sessions,
            &config.device,
            config.nn_cache_entries,
        );
        let params = EngineParams {
            mcts_sims: config.default_sims.max(1),
            engine_budget: config.default_engine_budget.max(1),
            nnue_budget: config.default_nnue.clamped(),
        };
        Self {
            params: RwLock::new(params),
            config,
            models,
            games: RwLock::new(HashMap::new()),
            agents: RwLock::new(HashMap::new()),
        }
    }
    pub fn params(&self) -> EngineParams {
        *read_lock(&self.params)
    }
    pub fn default_sims(&self) -> usize {
        self.params().mcts_sims
    }
    pub fn default_nnue_budget(&self) -> SearchBudget {
        self.params().nnue_budget
    }
    pub fn build_search_budget(
        &self,
        mode: Option<&str>,
        nodes: Option<u64>,
        depth: Option<i32>,
        time_ms: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> Result<SearchBudget, ApiError> {
        let defaults = self.default_nnue_budget();
        let mode = match mode {
            None => defaults.mode,
            Some(s) => BudgetMode::parse(s).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "非法预算模式: {s}（可选 nodes / depth_nodes / time）"
                ))
            })?,
        };
        Ok(SearchBudget {
            mode,
            nodes: nodes.unwrap_or(defaults.nodes).max(1),
            depth: depth.unwrap_or(defaults.depth).max(1),
            time_ms: time_ms.unwrap_or(defaults.time_ms),
            timeout_ms: timeout_ms.unwrap_or(defaults.timeout_ms).max(1),
        })
    }
    pub fn update_params(
        &self,
        mcts_sims: Option<usize>,
        engine_budget: Option<u64>,
        nnue_budget: Option<SearchBudget>,
    ) -> EngineParams {
        let mut p = write_lock(&self.params);
        *p = p.updated(mcts_sims, engine_budget, nnue_budget);
        *p
    }
    pub fn register_agent(&self, agent: Agent) -> Agent {
        write_lock(&self.agents).insert(agent.id, agent.clone());
        agent
    }
    pub fn get_agent(&self, id: Uuid) -> Result<Agent, ApiError> {
        read_lock(&self.agents)
            .get(&id)
            .cloned()
            .ok_or(ApiError::AgentNotFound(id.to_string()))
    }
    pub fn get_agent_for_variant(&self, id: Uuid, variant: Variant) -> Result<Agent, ApiError> {
        let agent = self.get_agent(id)?;
        if agent.variant != variant {
            return Err(ApiError::AgentVariantMismatch {
                agent_variant: agent.variant.as_str().to_string(),
                game_variant: variant.as_str().to_string(),
            });
        }
        Ok(agent)
    }
    pub fn remove_agent(&self, id: Uuid) -> Result<Agent, ApiError> {
        write_lock(&self.agents)
            .remove(&id)
            .ok_or(ApiError::AgentNotFound(id.to_string()))
    }
    pub fn resolve_agent_slot(&self, id: Uuid, variant: Variant) -> Result<PlayerSlot, ApiError> {
        let agent = self.get_agent_for_variant(id, variant)?;
        Ok(PlayerSlot::Strategy {
            kind: agent.kind,
            model_file: agent.model_file,
            params: agent.params,
        })
    }
    pub fn slot_params(
        &self,
        params: Option<&crate::dto::SlotParamsReq>,
    ) -> Result<EngineParams, ApiError> {
        let d = self.params();
        let Some(p) = params else {
            return Ok(d);
        };
        let nnue_budget = self.build_search_budget(
            p.nnue_budget_mode.as_deref(),
            p.nnue_budget,
            p.nnue_depth,
            p.nnue_time_ms,
            p.search_timeout_ms,
        )?;
        Ok(EngineParams {
            mcts_sims: p.mcts_sims.unwrap_or(d.mcts_sims).max(1),
            engine_budget: p.engine_budget.unwrap_or(d.engine_budget).max(1),
            nnue_budget,
        })
    }
    pub fn default_variant(&self) -> Variant {
        self.config.default_variant.unwrap_or(Variant::ALL[0])
    }
    pub fn resolve_model_file(
        &self,
        model_file: Option<&str>,
        variant: Variant,
    ) -> Result<String, ApiError> {
        match model_file.map(str::trim).filter(|s| !s.is_empty()) {
            Some(f) => Ok(f.to_string()),
            None => self.auto_match_model(variant),
        }
    }
    pub fn auto_match_model(&self, variant: Variant) -> Result<String, ApiError> {
        let id = variant.as_str();
        let candidates: Vec<String> = self
            .models
            .files()
            .into_iter()
            .filter(|f| {
                Path::new(f)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case(id))
            })
            .collect();
        match candidates.len() {
            1 => Ok(candidates.into_iter().next().expect("候选数已判为 1")),
            0 => Err(ApiError::BadRequest(format!(
                "未指定 modelFile 且未找到文件名为 {id}.onnx / {id}.nnue 的模型（可用清单见 GET /api/health）"
            ))),
            _ => Err(ApiError::BadRequest(format!(
                "变体 {id} 匹配到多个候选，须显式指定 modelFile: {}",
                candidates.join(", ")
            ))),
        }
    }
    #[cfg(feature = "onnx")]
    pub async fn acquire_model(
        self: &Arc<Self>,
        model_file: String,
    ) -> Result<Arc<banqi_engine::inference::onnx::OnnxModel>, String> {
        let app = Arc::clone(self);
        tokio::task::spawn_blocking(move || app.models.acquire_onnx(&model_file))
            .await
            .map_err(|e| format!("模型加载任务失败: {e}"))?
    }
    #[cfg(feature = "torch")]
    pub async fn acquire_torch_model(
        self: &Arc<Self>,
        model_file: String,
    ) -> Result<Arc<banqi_engine::engine::ModelWrapper>, String> {
        let app = Arc::clone(self);
        tokio::task::spawn_blocking(move || app.models.acquire_torch(&model_file))
            .await
            .map_err(|e| format!("模型加载任务失败: {e}"))?
    }
    pub async fn acquire_nnue_model(
        self: &Arc<Self>,
        model_file: String,
    ) -> Result<Arc<NnueEvaluator>, String> {
        let app = Arc::clone(self);
        tokio::task::spawn_blocking(move || app.models.acquire_nnue(&model_file))
            .await
            .map_err(|e| format!("模型加载任务失败: {e}"))?
    }
    pub async fn acquire_nnue_validated(
        self: &Arc<Self>,
        model_file: String,
        variant: Variant,
    ) -> Result<Arc<NnueEvaluator>, String> {
        let app = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            app.models
                .acquire_nnue_validated(&model_file, &variant.config())
        })
        .await
        .map_err(|e| format!("模型加载任务失败: {e}"))?
    }
    pub async fn preload_engine_model(
        self: &Arc<Self>,
        engine: EngineKind,
        model_file: String,
        variant: Variant,
    ) -> Result<(), ApiError> {
        match engine {
            EngineKind::Nnue => {
                let evaluator = self
                    .acquire_nnue_model(model_file.clone())
                    .await
                    .map_err(|e| {
                        ApiError::ModelUnavailable(format!("模型 {model_file} 加载失败: {e}"))
                    })?;
                evaluator.validate_layout(&variant.config()).map_err(|e| {
                    ApiError::ModelUnavailable(format!(
                        ".nnue 权重与变体 {} 不匹配: {e}",
                        variant.as_str()
                    ))
                })
            }
            EngineKind::MctsOnnx => {
                #[cfg(feature = "onnx")]
                {
                    self.acquire_model(model_file.clone()).await.map_err(|e| {
                        ApiError::ModelUnavailable(format!("模型 {model_file} 加载失败: {e}"))
                    })?;
                    Ok(())
                }
                #[cfg(not(feature = "onnx"))]
                {
                    let _ = model_file;
                    Err(ApiError::ModelUnavailable(
                        "服务编译未启用 onnx 推理，MCTS+ONNX 路径不可用（可改用 .nnue / .pt 模型）"
                            .into(),
                    ))
                }
            }
            EngineKind::MctsDL => {
                #[cfg(feature = "torch")]
                {
                    self.acquire_torch_model(model_file.clone())
                        .await
                        .map_err(|e| {
                            ApiError::ModelUnavailable(format!("模型 {model_file} 加载失败: {e}"))
                        })?;
                    Ok(())
                }
                #[cfg(not(feature = "torch"))]
                {
                    let _ = model_file;
                    Err(ApiError::ModelUnavailable(
                        "服务编译未启用 torch 推理，MCTS+DL 路径不可用（可改用 .onnx / .nnue 模型）"
                            .into(),
                    ))
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::sync::{read_lock, write_lock};
    use crate::config::ServerConfig;
    fn test_app() -> AppState {
        let cfg = ServerConfig {
            models_dir: std::env::temp_dir().join("banqi-server-test-no-models"),
            ..Default::default()
        };
        AppState::new(cfg)
    }
    #[test]
    fn set_default_sims_clamps_to_at_least_one() {
        let app = test_app();
        let p = app.update_params(Some(0), None, None);
        assert_eq!(p.mcts_sims, 1);
        assert_eq!(app.default_sims(), 1);
        assert_eq!(app.default_variant(), Variant::ALL[0]);
        let b = app.default_nnue_budget();
        assert_eq!(b.mode, crate::app::search::BudgetMode::DepthNodes);
        assert_eq!(b.timeout_ms, 60_000);
    }
    #[test]
    fn session_delegates_apply_and_undo() {
        let mut s = GameSession::new(Variant::DarkChess4x8, Slots::all_human());
        assert!(!s.undoable());
        let illegal = (0..)
            .find(|&a| !s.legal_actions().contains(&a))
            .expect("总有非法动作");
        assert!(matches!(
            s.apply_action(illegal),
            Err(ApiError::IllegalAction { .. })
        ));
        let flip = *s
            .legal_actions()
            .iter()
            .find(|&&a| s.action_view(a).is_chance)
            .expect("初始局面必有可翻棋动作");
        s.apply_action(flip).expect("翻棋应成功");
        assert!(!s.undoable());
        let flip2 = *s
            .legal_actions()
            .iter()
            .find(|&&a| s.action_view(a).is_chance)
            .expect("仍可翻棋");
        s.apply_action(flip2).expect("第二次翻棋应成功");
        assert!(s.undoable());
        s.undo_to_current_player().expect("悔棋应成功");
        assert_eq!(s.core.version, 3);
    }
    #[test]
    fn read_write_lock_recovery_helpers() {
        let games: RwLock<HashMap<Uuid, GameSession>> = RwLock::new(HashMap::new());
        assert!(read_lock(&games).is_empty());
        write_lock(&games).insert(
            Uuid::new_v4(),
            GameSession::new(Variant::DarkChess4x8, Slots::all_human()),
        );
        assert_eq!(read_lock(&games).len(), 1);
    }
}
