//! 应用状态：模型池（按变体，随用随加载）、对局存储与会话（对手配置 + 版本号）。
//!
//! 会话核心（变体 / 局面快照 / 悔棋 / 棋谱不变量）为 `crate::app::session::SnapshotSession`
//! （单一真源，经 `with_env!` 重建泛型环境执行——server 侧不假设变体种类）；
//! 本 crate 的 `GameSession` 在其上叠加 server 特有的对手配置、绑定模型与搜索树。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{RwLock, Arc};

use uuid::Uuid;

use crate::app::player::{Agent, EngineParams, PlayerSlot, Slots};
use crate::app::search::{BudgetMode, SearchBudget};
use crate::app::session::SnapshotSession;
use crate::app::sync::{read_lock, write_lock};
use banqi_core::core::env::{PositionSnapshot, Variant};
use banqi_engine::nnue::NnueEvaluator;

use crate::config::ServerConfig;
use crate::dto::{winner_str, ActionView, GameStateResp, HistoryMoveView, SlotView, SlotsView};
use crate::error::ApiError;
use crate::variant;

/// 树复用的具体树类型（本 server 的 MCTS 搜索统一以 `DarkChessEnv` 实例化，
/// `DarkChessEnv::from_snapshot` 按快照自带变体配置重建，所有变体通用）。
use crate::app::mcts_view::MctsTree;
use banqi_core::core::env::DarkChessEnv;


// 模型池与推理后端分派：实现收敛到 banqi-app（单一真源），此处仅别名/re-export

pub use crate::app::models::EngineKind;

/// 模型池：启动只**扫描**模型目录得到文件清单（不加载权重）；
/// 按文件路径随用随加载并**常驻缓存**，供所有对局复用。
pub type Models = crate::app::models::ModelPool;

// 对局会话

/// 单局状态：快照式会话核心 + 红/黑选手槽位 + 搜索树。
pub struct GameSession {
    /// 对局核心（变体/当前快照/悔棋历史/版本号；走子不变量在 banqi-app 单点维护）
    pub core: SnapshotSession,
    /// 红 / 黑双方选手槽位（Human 或策略托管；双方 Human 即原本地双人）
    pub slots: Slots,
    /// 最近一次 MCTS 搜索保留的树（策略回合或手动搜索时替换；仅查询用）
    pub mcts_tree: Option<crate::mcts::MctsTreeBox>,
    /// 最近一次 Expectimax+NNUE 搜索保留的轨迹树（ExpectimaxNnue 策略回合或
    /// 手动搜索时替换；仅查询用，不做跨回合复用）
    pub expectimax_tree: Option<crate::app::expectimax_view::ExpectimaxTree>,
}

impl GameSession {
    /// 按变体新建（标准初始局面；建局现统一走 [`Self::new_with_scenario`]，
    /// 本方法供测试与扩展使用）。
    #[allow(dead_code)]
    pub fn new(variant: Variant, slots: Slots) -> Self {
        Self {
            core: SnapshotSession::new(variant),
            slots,
            mcts_tree: None,
            expectimax_tree: None,
        }
    }

    /// 按变体 + 可选开局场景新建（场景参数非法 / 变体不支持时报 400）。
    /// `scenario = None` 时行为与 [`Self::new`] 完全一致。
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

    /// 由既有快照构建会话（BFEN 粘贴重建等场景；走子历史从空开始）。
    pub fn from_snapshot(variant: Variant, snapshot: PositionSnapshot, slots: Slots) -> Self {
        Self {
            core: SnapshotSession::from_snapshot(variant, snapshot),
            slots,
            mcts_tree: None,
            expectimax_tree: None,
        }
    }

    /// `(terminated, truncated, winner)`。
    pub fn over_state(&self) -> (bool, bool, Option<i32>) {
        self.core.over_state()
    }

    /// 应用动作（校验终局与合法性；快照栈/决策历史/版本号由核心维护）。
    pub fn apply_action(
        &mut self,
        action: usize,
    ) -> Result<variant::StepOutcome, ApiError> {
        self.core.apply_action(action).map_err(ApiError::from)
    }

    /// 悔棋：按快照逐帧回退 `steps` 步（对局结束后不可悔棋）。
    pub fn undo(&mut self, steps: usize) -> Result<(), ApiError> {
        self.core.undo_steps(steps).map_err(ApiError::from)
    }

    /// 悔棋到当前行棋方上次行动前：回退快照直至回到「当前行棋方即将行动」的局面。
    /// AI 对局中会一并回退 AI 与人类各一步；当前玩家尚未行动过时返回错误。
    pub fn undo_to_current_player(&mut self) -> Result<(), ApiError> {
        self.core.undo_to_current_player().map_err(ApiError::from)
    }

    /// 当前合法动作列表（状态视图经 `game_view` 产出；此方法供测试与扩展使用）。
    #[allow(dead_code)]
    pub fn legal_actions(&self) -> Vec<usize> {
        self.core.legal_actions()
    }

    /// 单个动作的结构化视图（坐标 + 机会标记）。
    pub fn action_view(&self, action: usize) -> ActionView {
        let coords = variant::coords_for_action(self.core.variant, action).unwrap_or_default();
        ActionView {
            action,
            from: coords.first().copied().unwrap_or(action),
            to: coords.last().copied().unwrap_or(action),
            is_chance: self.core.is_chance_action(action),
        }
    }

    /// 当前是否轮到策略行棋（Human 槽位恒 false；对局结束恒 false）。
    pub fn strategy_to_move(&self) -> bool {
        let (terminated, truncated, _) = self.over_state();
        !terminated
            && !truncated
            && !self.slots.side(self.core.current.current_player).is_human()
    }

    /// 当前是否可悔棋：对局未结束，且历史中存在「当前行棋方上次行动前」的快照
    ///（悔棋总是回到当前行棋方自己上次行动前，可连续触发以反悔所有自己的行动）。
    pub fn undoable(&self) -> bool {
        self.core.undoable()
    }

    /// 取出会话保留的上一棵搜索树并还原为具体类型（树复用用）；
    /// 搜索树以 `DarkChessEnv` 实例化（所有变体通用），还原恒成功。
    pub fn take_reusable_mcts_tree(&mut self) -> Option<MctsTree<DarkChessEnv>> {
        let tree = self.mcts_tree.take()?;
        tree.into_any()
            .downcast::<MctsTree<DarkChessEnv>>()
            .ok()
            .map(|b| *b)
    }

    /// 当前完整状态视图：前端 `GameState` 契约视图（`game_state_view`，单一真源）
    /// + server 专属扩展字段。
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

        // 决策历史（棋谱；棋子为前端短码 "R_Sol" 等）
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

    /// 摘要视图（列表用）。
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

/// 单个选手槽位的状态视图（Human 只下发 kind；策略槽位附参数摘要；
/// Agent 槽位下发 agent_id，实际策略由 Agent 实时持有）。
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
        } => {
            let b = &params.nnue_budget;
            let (budget_mode, nnue_depth, nnue_budget, nnue_time_ms, search_timeout_ms) =
                match kind {
                    crate::app::player::StrategyKind::ExpectimaxNnue => (
                        Some(b.mode.as_str().to_string()),
                        (b.mode == BudgetMode::DepthNodes).then_some(b.depth),
                        (b.mode != BudgetMode::Time).then_some(b.nodes),
                        (b.mode == BudgetMode::Time).then_some(b.time_ms),
                        Some(b.timeout_ms),
                    ),
                    crate::app::player::StrategyKind::Expectimax => (
                        None,
                        None,
                        None,
                        None,
                        Some(b.timeout_ms),
                    ),
                    _ => (None, None, None, None, None),
                };
            SlotView {
                kind: kind.as_str().to_string(),
                agent_id: None,
                model_file: model_file.clone(),
                mcts_sims: kind.is_mcts().then_some(params.mcts_sims),
                engine_budget: (matches!(
                    kind,
                    crate::app::player::StrategyKind::Expectimax
                ))
                .then_some(params.engine_budget),
                budget_mode,
                nnue_depth,
                nnue_budget,
                nnue_time_ms,
                search_timeout_ms,
            }
        }
    }
}

// 应用状态

/// axum 共享状态。
pub struct AppState {
    pub config: ServerConfig,
    /// 运行时可调的默认引擎参数快照（MCTS 模拟次数 / 强引擎节点预算 /
    /// NNUE 搜索预算，含模式与硬超时）；初始值取 `config`，经
    /// `PUT /api/settings` 在线修改。仅作为创建对局时未显式指定字段的
    /// 取值；已建对局的参数在建局时快照固定。
    pub params: RwLock<EngineParams>,
    pub models: Models,
    /// 对局存储（std RwLock 即可：所有持锁段均为同步 CPU 操作，不跨 await）
    pub games: RwLock<HashMap<Uuid, GameSession>>,
    /// Agent（玩家实体）注册表：变体 + 策略种类 + 模型 + 可实时调整的搜索参数。
    /// 绑定它的对局决策时实时读取（搜索配置调整立即生效）。
    pub agents: RwLock<HashMap<Uuid, Agent>>,
}

/// 路由层共享的 `Arc<AppState>` 别名（统一在此定义，避免各 handler 重复声明）。
pub type SharedState = std::sync::Arc<AppState>;

// ============================================================================
// 路由层公共辅助：对局 ID 解析 / 会话存取 / 搜索树读取（api 各文件共用）
// ============================================================================

/// 解析路径中的对局 ID（非法时 400）。
pub fn parse_game_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| ApiError::BadRequest(format!("非法对局 ID: {raw}")))
}

/// 从对局表读取会话（不存在时 404；`label` 用于错误消息，通常为路径原始 ID）。
pub fn get_session<'a>(
    games: &'a HashMap<Uuid, GameSession>,
    label: &str,
    game_id: Uuid,
) -> Result<&'a GameSession, ApiError> {
    games
        .get(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(label.to_string()))
}

/// [`get_session`] 的可变版本（走子 / 悔棋 / 换绑等写路径共用）。
pub fn get_mut_session<'a>(
    games: &'a mut HashMap<Uuid, GameSession>,
    label: &str,
    game_id: Uuid,
) -> Result<&'a mut GameSession, ApiError> {
    games
        .get_mut(&game_id)
        .ok_or_else(|| ApiError::GameNotFound(label.to_string()))
}

/// 取会话保留的 MCTS 搜索树（无树时 404 MCTS_TREE_MISSING；mcts 查询端点共用）。
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

    /// 当前默认引擎参数快照（读锁内拷贝，钳制规则由 `EngineParams` 维护）。
    pub fn params(&self) -> EngineParams {
        *read_lock(&self.params)
    }

    /// 当前默认 MCTS 搜索预算。
    pub fn default_sims(&self) -> usize {
        self.params().mcts_sims
    }

    /// 当前默认 NNUE 搜索预算（含模式与硬超时）。
    pub fn default_nnue_budget(&self) -> SearchBudget {
        self.params().nnue_budget
    }

    /// 由请求字段 + 服务器默认值构建搜索预算：未显式指定的字段取当前默认值，
    /// 模式字符串非法时报 400（建局 / 引擎查询 / settings 共用）。
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

    /// 运行时部分更新默认引擎参数：只覆盖 `Some` 字段，返回钳制后的生效快照。
    /// 只影响之后创建的对局（已建对局的参数在建局时快照固定）。
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

    /// 注册 Agent（模型类策略已由调用方预加载校验后才调用）。
    pub fn register_agent(&self, agent: Agent) -> Agent {
        write_lock(&self.agents).insert(agent.id, agent.clone());
        agent
    }

    /// 读取 Agent（不存在时 404）。
    pub fn get_agent(&self, id: Uuid) -> Result<Agent, ApiError> {
        read_lock(&self.agents)
            .get(&id)
            .cloned()
            .ok_or(ApiError::AgentNotFound(id.to_string()))
    }

    /// 读取 Agent 并校验变体一致（绑定槽位 / dry-run / 单步共用）。
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

    /// 删除 Agent。
    pub fn remove_agent(&self, id: Uuid) -> Result<Agent, ApiError> {
        write_lock(&self.agents)
            .remove(&id)
            .ok_or(ApiError::AgentNotFound(id.to_string()))
    }

    /// 实时解析 Agent 槽位为策略槽位（决策时调用；搜索配置取 Agent 当前值）。
    pub fn resolve_agent_slot(
        &self,
        id: Uuid,
        variant: Variant,
    ) -> Result<PlayerSlot, ApiError> {
        let agent = self.get_agent_for_variant(id, variant)?;
        Ok(PlayerSlot::Strategy {
            kind: agent.kind,
            model_file: agent.model_file,
            params: agent.params,
        })
    }

    /// 由请求可空槽位参数 + 服务器默认值构建引擎参数快照
    /// （建局 / 改槽位 / 建 Agent / Agent 设置共用）。
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

    /// 创建对局的默认变体：--default-variant > 首个变体。
    pub fn default_variant(&self) -> Variant {
        self.config.default_variant.unwrap_or(Variant::ALL[0])
    }

    /// 由请求可空的模型文件字段解析实际绑定的模型文件：空串视为未指定；
    /// 未指定时按变体 ID 自动匹配唯一候选（建局 / engine 查询共用）。
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

    /// 未显式指定模型时的自动匹配：文件名主干 == 变体 ID 的唯一候选
    /// （`.onnx` / `.nnue` 均参与；模型目录约定 `<variant_id>.onnx|.nnue`）。
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

    /// 随用随加载：按对局绑定的模型文件取 ONNX 模型；加载在 blocking 线程池执行，
    /// 加载后常驻缓存供所有对局复用。
    #[cfg(feature = "onnx")]
    pub async fn acquire_model(self: &Arc<Self>, model_file: String) -> Result<Arc<banqi_engine::inference::onnx::OnnxModel>, String> {
        let app = Arc::clone(self);
        tokio::task::spawn_blocking(move || app.models.acquire_onnx(&model_file))
            .await
            .map_err(|e| format!("模型加载任务失败: {e}"))?
    }

    /// 随用随加载：按对局绑定的模型文件取 TorchScript 模型（需 torch feature）。
    /// 加载在 blocking 线程池执行，加载后常驻缓存，供所有对局复用。
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

    /// 随用随加载：按对局绑定的模型文件取 NNUE 评估器（解析自解释头部）。
    pub async fn acquire_nnue_model(
        self: &Arc<Self>,
        model_file: String,
    ) -> Result<Arc<NnueEvaluator>, String> {
        let app = Arc::clone(self);
        tokio::task::spawn_blocking(move || app.models.acquire_nnue(&model_file))
            .await
            .map_err(|e| format!("模型加载任务失败: {e}"))?
    }

    /// 随用随加载 + 布局校验：取 NNUE 评估器并按变体配置显式校验
    /// `.nnue` 权重布局（坏模型 / 跨变体权重在建局/绑定时报错而非对局中途）。
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

    /// 按推理后端预加载并校验模型（建局 / 对局中改槽位共用的单一真源：
    /// 按后端 cfg 门控的「加载 + NNUE 布局校验」序列收敛于此）。
    /// 坏文件 / 跨变体权重在此报错而非对局中途。
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

// 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::sync::{read_lock, write_lock};
    use crate::config::ServerConfig;

    fn test_app() -> AppState {
        // models_dir 指向不存在的临时路径：模型池为空（仅告警），测试不加载 ONNX
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
        // 默认变体不依赖模型扫描
        assert_eq!(app.default_variant(), Variant::ALL[0]);
        // NNUE 默认预算：模式 + 硬超时均取默认值（DepthNodes / 60s）
        let b = app.default_nnue_budget();
        assert_eq!(b.mode, crate::app::search::BudgetMode::DepthNodes);
        assert_eq!(b.timeout_ms, 60_000);
    }

    #[test]
    fn session_delegates_apply_and_undo() {
        let mut s = GameSession::new(Variant::DarkChess4x8, Slots::all_human());
        assert!(!s.undoable());
        // 非法动作：合法集合之外必存在
        let illegal = (0..).find(|&a| !s.legal_actions().contains(&a)).expect("总有非法动作");
        assert!(matches!(
            s.apply_action(illegal),
            Err(ApiError::IllegalAction { .. })
        ));
        // 红翻棋一步（换黑行棋，黑尚未行动 → 不可悔）
        let flip = *s
            .legal_actions()
            .iter()
            .find(|&&a| s.action_view(a).is_chance)
            .expect("初始局面必有可翻棋动作");
        s.apply_action(flip).expect("翻棋应成功");
        assert!(!s.undoable());
        // 黑再翻一步后轮回红方：可悔到红方上次行动前
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
        write_lock(&games).insert(Uuid::new_v4(), GameSession::new(Variant::DarkChess4x8, Slots::all_human()));
        assert_eq!(read_lock(&games).len(), 1);
    }
}
