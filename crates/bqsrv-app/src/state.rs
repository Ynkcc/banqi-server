use crate::app::envops::StepOutcome;
use crate::app::mcts_view::MctsTree;
pub use crate::app::models::EngineKind;
use crate::app::player::{Agent, PlayerSlot, SlotParams, Slots, StrategyKind};
use crate::app::session::SnapshotSession;
use crate::app::sync::{lock, read_lock, write_lock};
use banqi_core::core::env::DarkChessEnv;
use banqi_core::core::env::{PositionSnapshot, Variant};
use banqi_engine::nnue::QNnueEvaluator;
use bqsrv_config::{ExpectimaxPartial, ExpectimaxRun, MctsSchema, ServerConfig, TimeBudgetMode};
use bqsrv_dto::ApiError;
use bqsrv_dto::{ActionView, GameStateResp, HistoryMoveView, SlotView, SlotsView, winner_str};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;
pub type Models = crate::app::models::ModelPool;

fn depth_partial(
    deterministic_depth: Option<i32>,
    chance_outcome_depth: Option<i32>,
) -> ExpectimaxPartial {
    serde_json::from_value(serde_json::json!({
        "deterministic_depth": deterministic_depth,
        "chance_outcome_depth": chance_outcome_depth,
    }))
    .expect("双字段 ExpectimaxPartial 构造不应失败")
}
pub use bq_util::unix_now;
pub struct GameSession {
    pub core: SnapshotSession,
    pub slots: Slots,
    pub mcts_tree: Option<crate::mcts::MctsTreeBox>,
    pub expectimax_tree: Option<crate::app::expectimax_view::ExpectimaxTree>,

    pub step_stats: Vec<Option<bqsrv_dto::StepStatsDto>>,

    pub movers: Vec<Option<String>>,

    pub slot_labels: [Option<String>; 2],
    pub created_at: u64,
}
impl GameSession {
    pub fn set_slot_label(
        &mut self,
        player: banqi_core::core::env::types::Player,
        label: Option<String>,
    ) {
        self.slot_labels[match player {
            banqi_core::core::env::types::Player::Red => 0,
            banqi_core::core::env::types::Player::Black => 1,
        }] = label;
    }
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
            movers: Vec::new(),
            slot_labels: [None, None],
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
            movers: Vec::new(),
            slot_labels: [None, None],
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
            movers: Vec::new(),
            slot_labels: [None, None],
            created_at: unix_now(),
        }
    }
    pub fn over_state(&self) -> (bool, bool, Option<i32>) {
        self.core.over_state()
    }
    pub fn apply_action(&mut self, action: usize) -> Result<StepOutcome, ApiError> {
        self.core.apply_action(action)
    }

    pub fn apply_action_with_reveal(
        &mut self,
        action: usize,
        reveal: Option<banqi_core::core::env::types::Piece>,
        mover: Option<String>,
    ) -> Result<StepOutcome, ApiError> {
        let outcome = self.core.apply_action_with_reveal(action, reveal)?;
        self.movers.truncate(self.core.history.records().len());
        self.movers.push(mover);
        Ok(outcome)
    }
    pub fn undo(&mut self, steps: usize) -> Result<(), ApiError> {
        self.core.undo_steps(steps)?;
        self.step_stats.truncate(self.core.history.records().len());
        self.movers.truncate(self.core.history.records().len());
        Ok(())
    }
    pub fn undo_to_current_player(&mut self) -> Result<(), ApiError> {
        self.core.undo_to_current_player()?;
        self.step_stats.truncate(self.core.history.records().len());
        self.movers.truncate(self.core.history.records().len());
        Ok(())
    }

    pub fn capture_step_stats(&mut self, stats: bqsrv_dto::StepStatsDto) {
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
            red: labeled_slot_view(&self.slots.red, self.slot_labels[0].clone()),
            black: labeled_slot_view(&self.slots.black, self.slot_labels[1].clone()),
        };
        let v = crate::app::view::game_view(&self.core);
        let s = &v.scalars;
        let over = s.terminated || s.truncated;
        let legal_actions: Vec<ActionView> = v.legal_actions.to_vec();
        let history: Vec<HistoryMoveView> = v
            .history
            .iter()
            .map(|e| HistoryMoveView {
                ply: e.ply,
                player: crate::app::common::player_label(e.player),
                mover: self.movers.get(e.ply.saturating_sub(1)).cloned().flatten(),
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
    pub fn summary(&self, game_id: Uuid) -> bqsrv_dto::GameSummary {
        let (_, _, winner) = self.over_state();
        let v = self.view(game_id);
        bqsrv_dto::GameSummary {
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

    pub fn archived_game(&self, game_id: Uuid, finished_at: u64) -> ArchivedGame {
        let v = self.view(game_id);
        let records = self.core.history.records();
        let snapshots = self.core.history.snapshots();
        let steps = records.len();
        let mut play = bq_record::GamePlay::default();
        play.steps = steps as u32;
        for r in records {
            play.players.push(match r.player {
                banqi_core::core::env::types::Player::Red => 1,
                banqi_core::core::env::types::Player::Black => 0xFF,
            });
            play.actions
                .extend_from_slice(&(r.action as u16).to_le_bytes());
        }
        play.snapshots = snapshots.iter().map(|s| s.encode()).collect();
        let outcome = bq_record::GameOutcome {
            winner: self.over_state().2,
            termination_reason: from_server_over_reason(v.over_reason.as_deref()),
            terminal_margin: None,
            health_diff_red: None,
        };
        let mut record = bq_record::GameRecord::new(
            self.core.variant.as_str(),
            bq_record::RecordSource::SourceServer,
        );
        record.identity.as_mut().unwrap().game_id = Some(game_id.to_string());
        record.play = Some(play);
        record.outcome = Some(outcome);
        record.timing = Some(bq_record::Timing {
            created_at: self.created_at as i64,
            finished_at: finished_at as i64,
        });
        let mut step_stats = self.step_stats.clone();
        step_stats.resize(steps, None);
        ArchivedGame {
            summary: bqsrv_dto::ArchivedGameSummary {
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
            step_stats,
            record,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedGame {
    #[serde(flatten)]
    pub summary: bqsrv_dto::ArchivedGameSummary,
    pub step_stats: Vec<Option<bqsrv_dto::StepStatsDto>>,
    pub record: bq_record::GameRecord,
}

pub fn from_server_over_reason(reason: Option<&str>) -> u32 {
    use bq_record::termination::{as_u32, from_server_over_reason as contract_map};
    reason.map(contract_map).map(as_u32).unwrap_or(0)
}
pub fn strategy_slot_view(
    kind: StrategyKind,
    model_file: Option<String>,
    params: SlotParams,
) -> SlotView {
    let (b, e) = (&params.nnue, &params.engine);
    let (budget_mode, deterministic_depth, chance_outcome_depth, budget_time_ms, search_timeout_ms) =
        match kind {
            StrategyKind::ExpectimaxNnue => (
                Some(b.time.mode.as_str().to_string()),
                (b.time.mode == TimeBudgetMode::Depth).then_some(b.schema.deterministic_depth),
                (b.time.mode == TimeBudgetMode::Depth).then_some(b.schema.chance_outcome_depth),
                (b.time.mode == TimeBudgetMode::Time).then_some(b.time.time_ms),
                Some(b.time.timeout_ms),
            ),
            StrategyKind::Expectimax => (
                Some(e.time.mode.as_str().to_string()),
                (e.time.mode == TimeBudgetMode::Depth).then_some(e.schema.deterministic_depth),
                (e.time.mode == TimeBudgetMode::Depth).then_some(e.schema.chance_outcome_depth),
                (e.time.mode == TimeBudgetMode::Time).then_some(e.time.time_ms),
                Some(e.time.timeout_ms),
            ),
            _ => (None, None, None, None, None),
        };
    SlotView {
        kind: kind.as_str().to_string(),
        label: None,
        agent_id: None,
        model_file,
        mcts_sims: kind.is_mcts().then_some(params.mcts.sims),
        budget_mode,
        deterministic_depth,
        chance_outcome_depth,
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

fn labeled_slot_view(slot: &PlayerSlot, label: Option<String>) -> SlotView {
    let mut v = slot_view(slot);
    v.label = label;
    v
}
pub struct AppState {
    pub config: ServerConfig,
    pub params: RwLock<SlotParams>,
    pub models: Models,
    pub games: RwLock<HashMap<Uuid, GameSession>>,
    pub agents: RwLock<HashMap<Uuid, Agent>>,

    pub recent: Mutex<VecDeque<ArchivedGame>>,
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
        let params = SlotParams {
            mcts: config.default_mcts,
            engine: config.default_engine,
            nnue: config.default_nnue,
            roots: config.root_tuning,
        }
        .clamped();
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
        let game = s.archived_game(game_id, unix_now());
        self.persist_archive(&game);
        let mut recent = lock(&self.recent);
        recent.push_front(game);

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

    pub fn archive_session_final(&self, game_id: Uuid, s: &GameSession) {
        if self.config.recent_games == 0 {
            return;
        }
        {
            let recent = lock(&self.recent);
            if recent
                .iter()
                .any(|g| g.summary.game_id == game_id.to_string())
            {
                return;
            }
        }
        let mut game = s.archived_game(game_id, unix_now());
        if !game.summary.over {
            game.summary.over = true;
            game.summary.over_reason = Some("abandoned".to_string());
            if let Some(outcome) = game.record.outcome.as_mut() {
                outcome.termination_reason = from_server_over_reason(Some("abandoned"));
            }
        }
        self.persist_archive(&game);
        let mut recent = lock(&self.recent);
        recent.push_front(game);
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
        tracing::info!(game = %game_id, "外部对局结束，已强制归档至最近对局记录（共 {} 条）", recent.len());
    }
    fn archive_file(&self, game_id: Uuid) -> Option<PathBuf> {
        self.config
            .archive_dir
            .as_ref()
            .map(|dir| dir.join(format!("{game_id}.json")))
    }

    fn persist_archive(&self, game: &ArchivedGame) {
        let Ok(game_id) = parse_game_id(&game.summary.game_id) else {
            return;
        };
        let Some(path) = self.archive_file(game_id) else {
            return;
        };
        let write = || -> std::io::Result<()> {
            let data = serde_json::to_string_pretty(game)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

            bq_util::atomic_write(&path, data.as_bytes())
        };
        if let Err(e) = write() {
            tracing::warn!(archive = %game.summary.game_id, "归档落盘失败（仅保留内存归档）: {e}");
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

        let mut games: Vec<ArchivedGame> = bq_util::load_json_dir(dir, "json")
            .into_iter()
            .map(|(_, game)| game)
            .collect();
        games.sort_by(|a, b| b.summary.finished_at.cmp(&a.summary.finished_at));
        games.truncate(self.config.recent_games);
        let count = games.len();
        *(lock(&self.recent)) = games.into();
        if count > 0 {
            tracing::info!("已从 {dir:?} 加载 {count} 条历史归档");
        }
    }
    pub fn params(&self) -> SlotParams {
        *read_lock(&self.params)
    }
    pub fn default_sims(&self) -> usize {
        self.params().mcts.sims
    }
    pub fn default_nnue_run(&self) -> ExpectimaxRun {
        self.params().nnue
    }

    pub fn build_nnue_run(
        &self,
        mode: Option<&str>,
        depth: Option<i32>,
        chance_outcome_depth: Option<i32>,
        time_ms: Option<u64>,
        timeout_ms: Option<u64>,
        schema_json: Option<&str>,
    ) -> Result<ExpectimaxRun, ApiError> {
        Self::build_expectimax_run(
            self.default_nnue_run(),
            mode,
            depth,
            chance_outcome_depth,
            time_ms,
            timeout_ms,
            schema_json,
        )
    }

    pub fn build_engine_run(
        &self,
        mode: Option<&str>,
        depth: Option<i32>,
        chance_outcome_depth: Option<i32>,
        time_ms: Option<u64>,
        timeout_ms: Option<u64>,
        schema_json: Option<&str>,
    ) -> Result<ExpectimaxRun, ApiError> {
        Self::build_expectimax_run(
            self.params().engine,
            mode,
            depth,
            chance_outcome_depth,
            time_ms,
            timeout_ms,
            schema_json,
        )
    }

    pub fn build_expectimax_run(
        defaults: ExpectimaxRun,
        mode: Option<&str>,
        depth: Option<i32>,
        chance_outcome_depth: Option<i32>,
        time_ms: Option<u64>,
        timeout_ms: Option<u64>,
        schema_json: Option<&str>,
    ) -> Result<ExpectimaxRun, ApiError> {
        let mut schema = defaults.schema;
        if let Some(json) = schema_json {
            let partial = bqsrv_config::parse_expectimax_partial(json)
                .map_err(|e| ApiError::BadRequest(format!("非法 Expectimax schema JSON: {e}")))?;
            schema.merge_partial(partial);
        }

        schema.merge_partial(depth_partial(depth, chance_outcome_depth));
        let mut time = defaults.time;
        if let Some(s) = mode {
            time.mode = TimeBudgetMode::parse(s).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "非法预算模式: {s}（可选 depth / time，旧值 depth_nodes 视为 depth）"
                ))
            })?;
        }
        if let Some(v) = time_ms {
            time.time_ms = v;
        }
        if let Some(v) = timeout_ms {
            time.timeout_ms = v.max(1);
        }
        Ok(ExpectimaxRun { schema, time }.clamped())
    }

    pub fn build_mcts_schema(
        defaults: MctsSchema,
        mcts_sims: Option<usize>,
        schema_json: Option<&str>,
    ) -> Result<MctsSchema, ApiError> {
        let mut schema = defaults;
        if let Some(json) = schema_json {
            let partial = bqsrv_config::parse_mcts_partial(json)
                .map_err(|e| ApiError::BadRequest(format!("非法 MCTS schema JSON: {e}")))?;
            schema.merge_partial(partial);
        }
        if let Some(sims) = mcts_sims {
            schema.merge_partial(bqsrv_config::mcts_sims_partial(sims));
        }
        Ok(schema)
    }
    pub fn update_params(
        &self,
        mcts: Option<MctsSchema>,
        engine: Option<ExpectimaxRun>,
        nnue: Option<ExpectimaxRun>,
    ) -> SlotParams {
        let mut p = write_lock(&self.params);
        *p = p.updated(mcts, engine, nnue);
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
        params: Option<&bqsrv_dto::SlotParamsReq>,
    ) -> Result<SlotParams, ApiError> {
        let d = self.params();
        let Some(p) = params else {
            return Ok(d);
        };
        let nnue = self.build_nnue_run(
            p.budget.nnue_budget_mode.as_deref(),
            p.budget.nnue_deterministic_depth,
            p.budget.nnue_chance_outcome_depth,
            p.budget.nnue_time_ms,
            p.budget.search_timeout_ms,
            p.nnue_schema.as_deref(),
        )?;
        let engine = self.build_engine_run(
            p.budget.engine_budget_mode.as_deref(),
            p.budget.engine_deterministic_depth,
            p.budget.engine_chance_outcome_depth,
            p.budget.engine_time_ms,
            p.budget.search_timeout_ms,
            p.engine_schema.as_deref(),
        )?;
        let mcts = Self::build_mcts_schema(d.mcts, p.mcts_sims, p.mcts_schema.as_deref())?;
        Ok(SlotParams {
            mcts,
            engine,
            nnue,
            roots: d.roots,
        }
        .clamped())
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
    ) -> Result<Arc<QNnueEvaluator>, String> {
        let app = Arc::clone(self);
        tokio::task::spawn_blocking(move || app.models.acquire_nnue(&model_file))
            .await
            .map_err(|e| format!("模型加载任务失败: {e}"))?
    }
    pub async fn acquire_nnue_validated(
        self: &Arc<Self>,
        model_file: String,
        variant: Variant,
    ) -> Result<Arc<QNnueEvaluator>, String> {
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
    use bqsrv_config::ServerConfig;
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
        let p = app.update_params(
            Some(MctsSchema {
                sims: 0,
                ..MctsSchema::default()
            }),
            None,
            None,
        );
        assert_eq!(p.mcts.sims, 1);
        assert_eq!(app.default_sims(), 1);
        assert_eq!(app.default_variant(), Variant::ALL[0]);
        let run = app.default_nnue_run();
        assert_eq!(run.time.mode, TimeBudgetMode::Depth);
        assert_eq!(run.time.timeout_ms, 60_000);
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
    fn apply_action_with_reveal_records_mover() {
        use bqsrv_dto::piece_from_display_code;
        let mut s = GameSession::new(Variant::DarkChess4x8, Slots::all_human());
        let flip = *s
            .legal_actions()
            .iter()
            .find(|&&a| s.action_view(a).is_chance)
            .expect("初始局面必有可翻棋动作");
        s.apply_action_with_reveal(flip, None, None)
            .expect("chance 动作缺 reveal 应回退为随机抽取暗子");
        s.undo(1).expect("悔棋应成功");
        let v0 = s.view(Uuid::new_v4());
        assert!(v0.history.is_empty());
        let piece = piece_from_display_code(6).expect("6 = 红帅");
        s.apply_action_with_reveal(flip, Some(piece), Some("engine".into()))
            .expect("显式 reveal 落子应成功");
        let v = s.view(Uuid::new_v4());
        assert_eq!(v.history[0].mover.as_deref(), Some("engine"));
        assert_eq!(
            v.history[0].revealed.as_deref(),
            Some(piece.short_name()).as_deref()
        );

        s.undo(1).expect("悔棋应成功");
        assert!(s.view(Uuid::new_v4()).history.is_empty());
        assert!(s.movers.is_empty());
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
        let games: Vec<ArchivedGame> = (0..3)
            .map(|i| {
                let g = s.archived_game(Uuid::new_v4(), 1_000 + i);
                app.persist_archive(&g);
                g
            })
            .collect();

        app.load_archives();
        let recent = crate::app::sync::lock(&app.recent);
        assert_eq!(recent.len(), 2, "应按 recent_games 截断");
        assert_eq!(recent[0].summary.game_id, games[2].summary.game_id);
        assert_eq!(
            recent[0].record.steps(),
            games[2].record.steps(),
            "record 分组往返应保持步数"
        );
        assert_eq!(
            recent[0].record.play.as_ref().unwrap().snapshots.len(),
            games[2].record.play.as_ref().unwrap().snapshots.len()
        );
        drop(recent);

        let oldest: Uuid = games[0].summary.game_id.parse().expect("uuid");
        app.remove_archive_file(oldest);
        assert!(!dir.join(format!("{oldest}.json")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_disk_roundtrip_replays_without_regression() {
        let dir = std::env::temp_dir().join(format!("banqi-arch-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mk_cfg = || ServerConfig {
            models_dir: std::env::temp_dir().join("banqi-server-test-no-models"),
            archive_dir: Some(dir.clone()),
            recent_games: 8,
            ..Default::default()
        };
        let app = AppState::new(mk_cfg());
        let mut s = GameSession::new(Variant::DarkChess4x8, Slots::all_human());
        for _ in 0..6 {
            if s.over_state().0 || s.over_state().1 {
                break;
            }
            let action = *s.legal_actions().first().expect("未终局必有合法动作");
            s.apply_action(action).expect("合法动作应成功");
        }
        let game_id = Uuid::new_v4();
        let game = s.archived_game(game_id, 2_000);
        let steps = game.record.steps();
        assert_eq!(steps, s.core.history.records().len());
        assert_eq!(
            game.record.play.as_ref().unwrap().players.len(),
            steps,
            "players 应与步数一一对应"
        );
        app.persist_archive(&game);

        let app2 = AppState::new(mk_cfg());
        app2.load_archives();
        let loaded = crate::app::sync::lock(&app2.recent)[0].clone();
        assert_eq!(loaded.summary.game_id, game.summary.game_id);
        assert_eq!(loaded.record.steps(), steps);

        let d1 = crate::app::replay::to_detail(&game).expect("内存归档派生 detail 应成功");
        let d2 = crate::app::replay::to_detail(&loaded).expect("磁盘归档派生 detail 应成功");
        assert_eq!(d1.initial_fen, d2.initial_fen);
        assert_eq!(d1.final_fen, d2.final_fen);
        assert_eq!(d1.moves.len(), d2.moves.len());
        assert_eq!(d1.snapshots.len(), steps);
        let r1 = crate::app::replay::build_replay(&game).expect("内存归档回放应成功");
        let r2 = crate::app::replay::build_replay(&loaded).expect("磁盘归档回放应成功");
        assert_eq!(r1.board_source, crate::app::replay::SOURCE_SNAPSHOT);
        assert_eq!(r1.final_board, r2.final_board);
        assert_eq!(r1.moves.len(), steps);

        let mid = crate::app::replay::snapshot_at_ply(&loaded, 2).expect("中盘快照应可取");
        assert_eq!(mid.slots.len(), 32);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_archive_without_record_is_ignored() {
        let dir = std::env::temp_dir().join(format!("banqi-arch-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建归档目录应成功");
        let legacy = r#"{
            "game_id": "00000000-0000-0000-0000-000000000001",
            "variant": "dark_chess_4x8",
            "created_at": 1,
            "finished_at": 2,
            "total_steps": 0,
            "over": true,
            "winner": "red",
            "over_reason": "checkmate",
            "initial_fen": "?",
            "final_fen": "?",
            "actions": [],
            "moves": [],
            "snapshots": [],
            "final_snapshot": null,
            "step_stats": []
        }"#;
        std::fs::write(dir.join("legacy.json"), legacy).expect("写旧格式归档应成功");

        let app = AppState::new(ServerConfig {
            models_dir: std::env::temp_dir().join("banqi-server-test-no-models"),
            archive_dir: Some(dir.clone()),
            recent_games: 4,
            ..Default::default()
        });
        app.load_archives();
        assert!(
            crate::app::sync::lock(&app.recent).is_empty(),
            "旧格式归档应被忽略"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_server_over_reason_maps_all_cases() {
        assert_eq!(super::from_server_over_reason(Some("checkmate")), 1);
        assert_eq!(super::from_server_over_reason(Some("draw")), 2);
        assert_eq!(super::from_server_over_reason(Some("max_steps")), 3);
        assert_eq!(super::from_server_over_reason(Some("abandoned")), 4);
        assert_eq!(super::from_server_over_reason(None), 0);
        assert_eq!(super::from_server_over_reason(Some("unknown")), 0);
    }
}
