use crate::app::envops::StepOutcome;
use crate::app::mcts_view::MctsTree;
pub use crate::app::models::EngineKind;
use crate::app::player::{Agent, EngineParams, PlayerSlot, Slots, StrategyKind};
use crate::app::search::{BudgetMode, SearchBudget};
use crate::app::session::SnapshotSession;
use crate::app::sync::{lock, read_lock, write_lock};
use crate::config::ServerConfig;
use crate::dto::{
    ActionView, ArchivedGameDetail, GameStateResp, HistoryMoveView, SlotView, SlotsView, winner_str,
};
use crate::error::ApiError;
use banqi_core::core::env::DarkChessEnv;
use banqi_core::core::env::{PositionSnapshot, Variant};
use banqi_engine::nnue::NnueEvaluator;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;
pub type Models = crate::app::models::ModelPool;
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
pub struct GameSession {
    pub core: SnapshotSession,
    pub slots: Slots,
    pub mcts_tree: Option<crate::mcts::MctsTreeBox>,
    pub expectimax_tree: Option<crate::app::expectimax_view::ExpectimaxTree>,

    pub step_stats: Vec<Option<crate::dto::StepStatsDto>>,
    pub created_at: u64,
}
impl GameSession {
    #[allow(dead_code)]
    pub fn new(variant: Variant, slots: Slots) -> Self {
        Self {
            core: SnapshotSession::new(variant),
            slots,
            mcts_tree: None,
            expectimax_tree: None,
            step_stats: Vec::new(),
            created_at: unix_now(),
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
            step_stats: Vec::new(),
            created_at: unix_now(),
        })
    }
    pub fn from_snapshot(variant: Variant, snapshot: PositionSnapshot, slots: Slots) -> Self {
        Self {
            core: SnapshotSession::from_snapshot(variant, snapshot),
            slots,
            mcts_tree: None,
            expectimax_tree: None,
            step_stats: Vec::new(),
            created_at: unix_now(),
        }
    }
    pub fn over_state(&self) -> (bool, bool, Option<i32>) {
        self.core.over_state()
    }
    pub fn apply_action(&mut self, action: usize) -> Result<StepOutcome, ApiError> {
        self.core.apply_action(action).map_err(ApiError::from)
    }
    pub fn undo(&mut self, steps: usize) -> Result<(), ApiError> {
        self.core.undo_steps(steps).map_err(ApiError::from)?;
        self.step_stats.truncate(self.core.history.records().len());
        Ok(())
    }
    pub fn undo_to_current_player(&mut self) -> Result<(), ApiError> {
        self.core.undo_to_current_player().map_err(ApiError::from)?;
        self.step_stats.truncate(self.core.history.records().len());
        Ok(())
    }

    pub fn capture_step_stats(&mut self, stats: crate::dto::StepStatsDto) {
        let idx = self.core.history.records().len();
        if self.step_stats.len() + 1 < idx {
            self.step_stats.resize(idx - 1, None);
        }
        self.step_stats
            .push(if stats.is_empty() { None } else { Some(stats) });
    }
    #[allow(dead_code)]
    pub fn legal_actions(&self) -> Vec<usize> {
        self.core.legal_actions()
    }
    pub fn action_view(&self, action: usize) -> ActionView {
        crate::app::view::derive_action_view(
            self.core.variant,
            action,
            self.core.is_chance_action(action),
        )
        .into()
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
            .cloned()
            .map(ActionView::from)
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

    pub fn archived_detail(&self, game_id: Uuid, finished_at: u64) -> ArchivedGameDetail {
        let v = self.view(game_id);
        let initial_fen = self
            .core
            .history
            .initial()
            .map(|s| s.to_fen())
            .unwrap_or_else(|| self.core.current.to_fen());
        let mut step_stats = self.step_stats.clone();
        step_stats.resize(v.history.len(), None);
        ArchivedGameDetail {
            summary: crate::dto::ArchivedGameSummary {
                game_id: game_id.to_string(),
                variant: self.core.variant.as_str().to_string(),
                created_at: self.created_at,
                finished_at,
                total_steps: v.game.total_step_counter as usize,
                over: v.over,
                winner: v.winner.clone(),
                over_reason: v.over_reason.clone(),
                slots: v.slots,
            },
            initial_fen,
            final_fen: v.game.fen.clone(),
            actions: self
                .core
                .history
                .records()
                .iter()
                .map(|r| crate::dto::ReplayActionDto {
                    action: r.action,
                    revealed: r
                        .revealed
                        .map(|p| banqi_core::core::env::piece_letter(p.piece_type, p.player)),
                })
                .collect(),
            moves: v.history,
            snapshots: self
                .core
                .history
                .snapshots()
                .iter()
                .map(|s| crate::dto::hex_encode(&s.encode()))
                .collect(),
            final_snapshot: Some(crate::dto::hex_encode(&self.core.current.encode())),
            step_stats,
        }
    }
}
pub fn strategy_slot_view(
    kind: StrategyKind,
    model_file: Option<String>,
    params: EngineParams,
) -> SlotView {
    let (b, e) = (&params.nnue_budget, &params.engine_budget);
    let (budget_mode, budget_depth, budget, budget_time_ms, search_timeout_ms) = match kind {
        StrategyKind::ExpectimaxNnue => (
            Some(b.mode.as_str().to_string()),
            (b.mode == BudgetMode::DepthNodes).then_some(b.depth),
            (b.mode != BudgetMode::Time).then_some(b.nodes),
            (b.mode == BudgetMode::Time).then_some(b.time_ms),
            Some(b.timeout_ms),
        ),
        StrategyKind::Expectimax => (
            Some(e.mode.as_str().to_string()),
            (e.mode == BudgetMode::DepthNodes).then_some(e.depth),
            (e.mode != BudgetMode::Time).then_some(e.nodes),
            (e.mode == BudgetMode::Time).then_some(e.time_ms),
            Some(e.timeout_ms),
        ),
        _ => (None, None, None, None, None),
    };
    SlotView {
        kind: kind.as_str().to_string(),
        agent_id: None,
        model_file,
        mcts_sims: kind.is_mcts().then_some(params.mcts_sims),
        engine_budget: (kind == StrategyKind::Expectimax).then_some(e.nodes),
        budget_mode,
        budget_depth,
        budget,
        budget_time_ms,
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

    pub recent: Mutex<VecDeque<ArchivedGameDetail>>,
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
            engine_budget: crate::app::search::SearchBudget {
                mode: crate::app::search::BudgetMode::Nodes,
                nodes: config.default_engine_budget.max(1),
                ..SearchBudget::default()
            },
            nnue_budget: config.default_nnue.clamped(),
        };
        Self {
            params: RwLock::new(params),
            config,
            models,
            games: RwLock::new(HashMap::new()),
            agents: RwLock::new(HashMap::new()),
            recent: Mutex::new(VecDeque::new()),
        }
    }

    pub fn archive_session(&self, game_id: Uuid, s: &GameSession) {
        if self.config.recent_games == 0 {
            return;
        }
        let (terminated, truncated, _) = s.over_state();
        if !terminated && !truncated {
            return;
        }
        let detail = s.archived_detail(game_id, unix_now());
        self.persist_archive(&detail);
        let mut recent = lock(&self.recent);
        recent.push_front(detail);

        while recent.len() > self.config.recent_games {
            match recent.pop_back() {
                Some(removed) => {
                    if let Ok(id) = Uuid::parse_str(&removed.summary.game_id) {
                        self.remove_archive_file(id);
                    }
                }
                None => break,
            }
        }
        tracing::info!(game = %game_id, "对局终局，已归档至最近对局记录（共 {} 条）", recent.len());
    }
    fn archive_file(&self, game_id: Uuid) -> Option<PathBuf> {
        self.config
            .archive_dir
            .as_ref()
            .map(|dir| dir.join(format!("{game_id}.json")))
    }

    fn persist_archive(&self, detail: &ArchivedGameDetail) {
        let Ok(game_id) = parse_game_id(&detail.summary.game_id) else {
            return;
        };
        let Some(path) = self.archive_file(game_id) else {
            return;
        };
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(
                &path,
                serde_json::to_string_pretty(detail).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                })?,
            )
        };
        if let Err(e) = write() {
            tracing::warn!(archive = %detail.summary.game_id, "归档落盘失败（仅保留内存归档）: {e}");
        }
    }
    fn remove_archive_file(&self, game_id: Uuid) {
        if let Some(path) = self.archive_file(game_id) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(game = %game_id, "删除过期归档文件失败: {e}"),
            }
        }
    }

    pub fn load_archives(&self) {
        let Some(dir) = &self.config.archive_dir else {
            return;
        };
        if self.config.recent_games == 0 {
            return;
        }
        let mut details: Vec<ArchivedGameDetail> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .filter_map(|text| serde_json::from_str(&text).ok())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::warn!("读取归档目录 {dir:?} 失败: {e}");
                return;
            }
        };
        details.sort_by(|a, b| b.summary.finished_at.cmp(&a.summary.finished_at));
        details.truncate(self.config.recent_games);
        let count = details.len();
        *(lock(&self.recent)) = details.into();
        if count > 0 {
            tracing::info!("已从 {dir:?} 加载 {count} 条历史归档");
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
        Self::build_budget(
            self.default_nnue_budget(),
            mode,
            nodes,
            depth,
            time_ms,
            timeout_ms,
        )
    }

    pub fn build_engine_budget(
        &self,
        mode: Option<&str>,
        nodes: Option<u64>,
        depth: Option<i32>,
        time_ms: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> Result<SearchBudget, ApiError> {
        Self::build_budget(
            self.params().engine_budget,
            mode,
            nodes,
            depth,
            time_ms,
            timeout_ms,
        )
    }
    pub fn build_budget(
        defaults: SearchBudget,
        mode: Option<&str>,
        nodes: Option<u64>,
        depth: Option<i32>,
        time_ms: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> Result<SearchBudget, ApiError> {
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
        engine_budget: Option<SearchBudget>,
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
        let engine_budget = self.build_engine_budget(
            p.engine_budget_mode.as_deref(),
            p.engine_budget,
            p.engine_depth,
            p.engine_time_ms,
            p.search_timeout_ms,
        )?;
        Ok(EngineParams {
            mcts_sims: p.mcts_sims.unwrap_or(d.mcts_sims).max(1),
            engine_budget,
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
    #[test]
    fn archive_roundtrip_via_disk_and_capacity_cleanup() {
        let dir = std::env::temp_dir().join(format!("banqi-arch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mk_cfg = || ServerConfig {
            models_dir: std::env::temp_dir().join("banqi-server-test-no-models"),
            archive_dir: Some(dir.clone()),
            recent_games: 2,
            ..Default::default()
        };
        let app = AppState::new(mk_cfg());
        let s = GameSession::new(Variant::DarkChess4x8, Slots::all_human());
        let details: Vec<ArchivedGameDetail> = (0..3)
            .map(|i| {
                let d = s.archived_detail(Uuid::new_v4(), 1_000 + i);
                app.persist_archive(&d);
                d
            })
            .collect();

        app.load_archives();
        let recent = crate::app::sync::lock(&app.recent);
        assert_eq!(recent.len(), 2, "应按 recent_games 截断");
        assert_eq!(recent[0].summary.game_id, details[2].summary.game_id);
        assert_eq!(recent[0].snapshots.len(), details[2].snapshots.len());
        assert_eq!(recent[0].final_snapshot, details[2].final_snapshot);
        drop(recent);

        let oldest: Uuid = details[0].summary.game_id.parse().expect("uuid");
        app.remove_archive_file(oldest);
        assert!(!dir.join(format!("{oldest}.json")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
