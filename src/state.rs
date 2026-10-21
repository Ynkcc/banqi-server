//! 应用状态：模型池（按变体，随用随加载）、对局存储与会话（对手配置 + 版本号）。
//!
//! 会话核心（变体 / 局面快照 / 悔棋 / 棋谱不变量）为 `banqi_app::session::SnapshotSession`
//! （单一真源，经 `with_env!` 重建泛型环境执行——server 侧不假设变体种类）；
//! 本 crate 的 `GameSession` 在其上叠加 server 特有的对手配置、绑定模型与搜索树。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{RwLock, Arc};

use uuid::Uuid;

use banqi_app::opponent::{EngineParams, OpponentKind};
use banqi_app::search::{BudgetMode, SearchBudget};
use banqi_app::session::SnapshotSession;
use banqi_app::sync::{read_lock, write_lock};
use banqi_core::core::env::types::Player;
use banqi_core::core::env::{PositionSnapshot, Variant};
use banqi_engine::nnue::NnueEvaluator;

use crate::config::ServerConfig;
use crate::dto::{
    piece_type_str, player_str, winner_str, ActionView, AiInfoView, CellView, GameStateResp,
    HistoryMoveView, PieceView,
};
use crate::error::ApiError;
use crate::variant;

// ============================================================================
// 模型池与推理后端分派：实现收敛到 banqi-app（单一真源），此处仅别名/re-export
// ============================================================================

pub use banqi_app::models::EngineKind;

/// 模型池：启动只**扫描**模型目录得到文件清单（不加载权重）；
/// 按文件路径随用随加载并**常驻缓存**，供所有对局复用。
pub type Models = banqi_app::models::ModelPool;

// ============================================================================
// 对局会话
// ============================================================================

/// 对手配置（PvP 时 `GameSession.opponent` 为 None）。
#[derive(Clone, Copy, Debug)]
pub struct OpponentConfig {
    /// 对手执子方
    pub player: Player,
    /// 对手种类（规则 / Engine / Nnue / MctsOnnx；模型类对手须绑定模型文件）
    pub kind: OpponentKind,
    /// MCTS 模拟次数（MctsOnnx 后端；创建对局时快照固定，
    /// 未显式指定时取服务器当前默认值）
    pub mcts_sims: usize,
    /// Expectimax 强引擎节点预算（Engine 对手；创建对局时快照固定，
    /// 未显式指定时取服务器当前默认值）
    pub engine_budget: u64,
    /// Expectimax 搜索预算（Engine：节点预算 + 硬超时；Nnue：模式 +
    /// 节点/深度/时间 + 硬超时。创建对局时快照固定，未显式指定字段
    /// 取服务器当前默认值）
    pub search_budget: SearchBudget,
}

/// 单局状态：快照式会话核心 + 对手配置 + 绑定模型 + 搜索树。
pub struct GameSession {
    /// 对局核心（变体/当前快照/悔棋历史/版本号；走子不变量在 banqi-app 单点维护）
    pub core: SnapshotSession,
    /// 对手配置（None = 本地双人 PvP）
    pub opponent: Option<OpponentConfig>,
    /// 与对局绑定的模型文件（仅模型类对手需要；手动搜索亦使用该文件，
    /// 随用随加载，常驻缓存）。
    pub model_file: Option<String>,
    /// 最近一次 MCTS 搜索保留的树（对手回合或手动搜索时替换；仅查询用）
    pub mcts_tree: Option<crate::mcts::MctsTreeBox>,
}

impl GameSession {
    pub fn new(
        variant: Variant,
        opponent: Option<OpponentConfig>,
        model_file: Option<String>,
    ) -> Self {
        Self {
            core: SnapshotSession::new(variant),
            opponent,
            model_file,
            mcts_tree: None,
        }
    }

    /// 由既有快照构建会话（BFEN 粘贴重建等场景；走子历史从空开始）。
    pub fn from_snapshot(
        variant: Variant,
        snapshot: PositionSnapshot,
        opponent: Option<OpponentConfig>,
        model_file: Option<String>,
    ) -> Self {
        Self {
            core: SnapshotSession::from_snapshot(variant, snapshot),
            opponent,
            model_file,
            mcts_tree: None,
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
    ///
    /// AI 对局中（AI 走完后轮到人类）会一并回退 AI 与人类各一步；
    /// PvP 中即回退最近一步。当前玩家尚未行动过时返回错误。
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

    /// 当前是否轮到对手行棋（PvP 恒 false）。
    pub fn opponent_to_move(&self) -> bool {
        match self.opponent {
            Some(opp) => {
                let (terminated, truncated, _) = self.over_state();
                !terminated && !truncated && self.core.current.current_player == opp.player
            }
            None => false,
        }
    }

    /// 当前是否可悔棋：对局未结束，且历史中存在「当前行棋方上次行动前」的快照
    ///（悔棋总是回到当前行棋方自己上次行动前，可连续触发以反悔所有自己的行动）。
    pub fn undoable(&self) -> bool {
        self.core.undoable()
    }

    /// 当前完整状态视图：中立视图 `banqi_app::view::game_view`（单一真源）→ DTO 映射。
    pub fn view(&self, game_id: Uuid) -> GameStateResp {
        let v = banqi_app::view::game_view(&self.core);
        let s = &v.scalars;
        let over = s.terminated || s.truncated;

        // 棋盘视图（行优先；暗子不泄露真实身份）
        let board: Vec<CellView> = v
            .cells
            .iter()
            .map(|c| {
                use banqi_app::view::CellState::*;
                let (state, piece) = match c.state {
                    Empty => ("empty", None),
                    Hidden => ("hidden", None),
                    Revealed => (
                        "revealed",
                        c.piece.map(|p| PieceView {
                            piece_type: piece_type_str(p.piece_type),
                            side: player_str(p.player),
                        }),
                    ),
                };
                CellView {
                    index: c.index,
                    row: c.row,
                    col: c.col,
                    state: state.to_string(),
                    piece,
                }
            })
            .collect();

        // 合法动作视图
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

        // 阵亡 / 隐藏棋子列表（按型展开；hidden 计数可由公开信息推导，不泄露暗子身份）
        let dead_red: Vec<String> = v.dead[0].iter().map(|t| piece_type_str(*t)).collect();
        let dead_black: Vec<String> = v.dead[1].iter().map(|t| piece_type_str(*t)).collect();
        let hidden_red: Vec<String> = v.hidden[0]
            .iter()
            .map(|p| piece_type_str(p.piece_type))
            .collect();
        let hidden_black: Vec<String> = v.hidden[1]
            .iter()
            .map(|p| piece_type_str(p.piece_type))
            .collect();

        let ai_view = match self.opponent {
            Some(opp) => AiInfoView {
                enabled: true,
                kind: opp.kind.as_str().to_string(),
                player: Some(player_str(opp.player)),
                engine: match opp.kind {
                    OpponentKind::Nnue => Some("nnue".to_string()),
                    OpponentKind::MctsOnnx => Some("mcts-onnx".to_string()),
                    _ => None,
                },
                mcts_sims: opp.mcts_sims,
                engine_budget: (opp.kind == OpponentKind::Engine).then_some(opp.engine_budget),
                budget_mode: (opp.kind == OpponentKind::Nnue)
                    .then(|| opp.search_budget.mode.as_str().to_string()),
                nnue_depth: (opp.kind == OpponentKind::Nnue
                    && opp.search_budget.mode == BudgetMode::DepthNodes)
                    .then_some(opp.search_budget.depth),
                nnue_budget: (opp.kind == OpponentKind::Nnue
                    && opp.search_budget.mode != BudgetMode::Time)
                    .then_some(opp.search_budget.nodes),
                nnue_time_ms: (opp.kind == OpponentKind::Nnue
                    && opp.search_budget.mode == BudgetMode::Time)
                    .then_some(opp.search_budget.time_ms),
                search_timeout_ms: (opp.kind == OpponentKind::Nnue)
                    .then_some(opp.search_budget.timeout_ms),
            },
            None => AiInfoView {
                enabled: false,
                kind: "PvP".to_string(),
                player: None,
                engine: None,
                mcts_sims: 0,
                engine_budget: None,
                budget_mode: None,
                nnue_depth: None,
                nnue_budget: None,
                nnue_time_ms: None,
                search_timeout_ms: None,
            },
        };

        // 决策历史（棋谱）
        let history: Vec<HistoryMoveView> = v
            .history
            .iter()
            .map(|e| HistoryMoveView {
                ply: e.ply,
                player: player_str(e.player),
                action: e.action,
                from: e.from,
                to: e.to,
                is_flip: e.is_flip,
                is_chance: e.is_chance,
                is_capture: e.is_capture,
                revealed: e.revealed.map(|p| PieceView {
                    piece_type: piece_type_str(p.piece_type),
                    side: player_str(p.player),
                }),
                captured: e.captured.map(|p| PieceView {
                    piece_type: piece_type_str(p.piece_type),
                    side: player_str(p.player),
                }),
            })
            .collect();

        GameStateResp {
            game_id: game_id.to_string(),
            variant: self.core.variant.as_str().to_string(),
            fen: s.fen.clone(),
            version: self.core.version,
            current_player: player_str(s.current_player),
            move_count: s.move_count,
            total_steps: s.total_steps,
            max_steps: s.max_steps,
            red_score: s.scores[0],
            black_score: s.scores[1],
            red_hp: s.scores[0],
            black_hp: s.scores[1],
            over,
            terminated: s.terminated,
            truncated: s.truncated,
            over_reason: s.over_reason.map(|r| r.as_str().to_string()),
            winner: winner_str(s.winner),
            last_action: s.last_action,
            board,
            dead_red,
            dead_black,
            hidden_red,
            hidden_black,
            legal_actions,
            history,
            ai: ai_view,
            ai_to_move: self.opponent_to_move(),
            undoable: self.undoable(),
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
            current_player: v.current_player,
            total_steps: v.total_steps,
            over: v.over,
            winner: winner_str(winner),
            ai: v.ai,
        }
    }
}

// ============================================================================
// 应用状态
// ============================================================================

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
}

/// 路由层共享的 `Arc<AppState>` 别名（统一在此定义，避免各 handler 重复声明）。
pub type SharedState = std::sync::Arc<AppState>;

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

    /// 当前默认 Expectimax 强引擎节点预算。
    pub fn default_engine_budget(&self) -> u64 {
        self.params().engine_budget
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

    /// 创建对局的默认变体：--default-variant > 首个变体。
    pub fn default_variant(&self) -> Variant {
        self.config.default_variant.unwrap_or(Variant::ALL[0])
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

    /// 随用随加载：按对局绑定的模型文件取 ONNX 模型（缓存命中直接返回，
    /// 首次使用时加载权重）。加载（构建 ort 会话）在 blocking 线程池执行，
    /// 避免阻塞异步运行时；加载后常驻缓存，供所有对局复用。
    #[cfg(feature = "onnx")]
    pub async fn acquire_model(self: &Arc<Self>, model_file: String) -> Result<Arc<banqi_engine::inference::onnx::OnnxModel>, String> {
        let app = Arc::clone(self);
        tokio::task::spawn_blocking(move || app.models.acquire_onnx(&model_file))
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
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use banqi_app::sync::{read_lock, write_lock};
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
        assert_eq!(b.mode, banqi_app::search::BudgetMode::DepthNodes);
        assert_eq!(b.timeout_ms, 60_000);
    }

    #[test]
    fn session_delegates_apply_and_undo() {
        let mut s = GameSession::new(Variant::DarkChess4x8, None, None);
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
        write_lock(&games).insert(Uuid::new_v4(), GameSession::new(Variant::DarkChess4x8, None, None));
        assert_eq!(read_lock(&games).len(), 1);
    }
}
