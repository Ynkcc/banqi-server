//! 统一选手层（解耦核心）：选手槽位 + 策略决策的单一真源。
//!
//! 「对局模式」（PvP / 单一对手）概念由此废除：每局红、黑各一个独立槽位
//! [`PlayerSlot`]，可随时设为 `Human`（用户本人操作）或任意一种 [`StrategyKind`]
//! 托管；双方均为 `Human` 即原本地双人。
//!
//! - [`StrategyKind`]：策略种类与前端字符串契约一一对应
//!   （"Random" / "RevealFirst" / "CaptureFirst" / "Expectimax" / "ExpectimaxNnue" /
//!   "MctsOnnx" / "MctsDL"；兼容旧别名 "Engine" / "Nnue"）；
//! - [`decide`]：全部策略的同步决策入口（收敛原 `choose_simple` 与
//!   ai.rs 按 OpponentKind 的大 match 分派；规则策略廉价可锁内直调，
//!   其余计算量大，调用方须放入 blocking 线程池）；
//! - [`dry_run`]：非破坏性决策（不落子），供 dry-run 高亮建议走法；
//! - [`EngineParams`]：引擎参数快照单一真源（钳制规则与部分更新语义在此统一）。
//!
//! 设计约束：纯同步逻辑，不依赖 axum / tokio；模型经 [`ModelPool`]
//! 随用随加载（同步加载，调用方在 blocking 线程池内调用）。

use std::sync::Arc;

use uuid::Uuid;

use banqi_core::core::env::{DarkChessEnv, PositionSnapshot, SnapshotEnv};
use banqi_core::core::expectimax::rule_eval::RuleMaterialEval;
use banqi_core::core::expectimax;
use banqi_engine::engine::{CaptureFirstPolicy, Policy, RandomPolicy, RevealFirstPolicy};

use crate::app::models::{EngineKind, ModelPool};
use crate::app::mcts_view::{MctsTree, MctsTreeBox};
use crate::app::search::SearchBudget;
use crate::dto::{MctsStatsDto, NnueStatsDto};

// 策略种类

/// 规则策略种类（轻量基线 / 陪练）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Random,
    RevealFirst,
    CaptureFirst,
}

impl RuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleKind::Random => "Random",
            RuleKind::RevealFirst => "RevealFirst",
            RuleKind::CaptureFirst => "CaptureFirst",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let norm = s.trim().to_ascii_lowercase().replace(['-', '_'], "");
        match norm.as_str() {
            "random" => Some(RuleKind::Random),
            "revealfirst" => Some(RuleKind::RevealFirst),
            "capturefirst" => Some(RuleKind::CaptureFirst),
            _ => None,
        }
    }
}

/// 策略种类全集（字符串契约与前端 `StrategyKind` 一致）。
///
/// 旧名兼容："Engine" → [`StrategyKind::Expectimax`]、"Nnue" →
/// [`StrategyKind::ExpectimaxNnue`]；`MctsDL` 需 torch feature 才能实际执行
/// （未启用时决策期报错，与 MctsOnnx 无 onnx feature 时一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyKind {
    Random,
    RevealFirst,
    CaptureFirst,
    /// Expectimax 强引擎（αβ + Star1 + 置换表 + 迭代加深，纯规则叶评估，无模型）
    Expectimax,
    /// Expectimax + NNUE 叶评估（.nnue 模型）
    ExpectimaxNnue,
    /// Gumbel MCTS + ONNX 策略价值网络（.onnx，需 onnx feature）
    MctsOnnx,
    /// Gumbel MCTS + TorchScript 策略价值网络（.pt，需 torch feature）
    MctsDL,
}

impl StrategyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StrategyKind::Random => "Random",
            StrategyKind::RevealFirst => "RevealFirst",
            StrategyKind::CaptureFirst => "CaptureFirst",
            StrategyKind::Expectimax => "Expectimax",
            StrategyKind::ExpectimaxNnue => "ExpectimaxNnue",
            StrategyKind::MctsOnnx => "MctsOnnx",
            StrategyKind::MctsDL => "MctsDL",
        }
    }

    /// 解析策略字符串（大小写不敏感，接受 camelCase / snake_case 别名与旧名）。
    pub fn parse(s: &str) -> Option<Self> {
        let norm = s.trim().to_ascii_lowercase().replace(['-', '_'], "");
        match norm.as_str() {
            "random" => Some(StrategyKind::Random),
            "revealfirst" => Some(StrategyKind::RevealFirst),
            "capturefirst" => Some(StrategyKind::CaptureFirst),
            "expectimax" | "engine" => Some(StrategyKind::Expectimax),
            "expectimaxnnue" | "nnue" => Some(StrategyKind::ExpectimaxNnue),
            "mctsonnx" => Some(StrategyKind::MctsOnnx),
            "mctsdl" => Some(StrategyKind::MctsDL),
            _ => None,
        }
    }

    /// 是否需要绑定模型文件（建局 / 决策前须已解析到模型）。
    pub fn needs_model(self) -> bool {
        matches!(
            self,
            StrategyKind::ExpectimaxNnue | StrategyKind::MctsOnnx | StrategyKind::MctsDL
        )
    }

    /// 是否为 Gumbel MCTS 类策略（决策保留整棵搜索树，支持跨回合复用）。
    pub fn is_mcts(self) -> bool {
        matches!(self, StrategyKind::MctsOnnx | StrategyKind::MctsDL)
    }

    /// 模型类策略对应的推理后端（其余返回 None）。
    /// 引擎字符串下发（`EngineKind::as_str`）与模型预加载均由此单点换算。
    pub fn engine_kind(self) -> Option<EngineKind> {
        match self {
            StrategyKind::ExpectimaxNnue => Some(EngineKind::Nnue),
            StrategyKind::MctsOnnx => Some(EngineKind::MctsOnnx),
            StrategyKind::MctsDL => Some(EngineKind::MctsDL),
            _ => None,
        }
    }

    /// 由推理后端构造对应策略（按模型扩展名推断时使用）。
    pub fn from_engine_kind(engine: EngineKind) -> Self {
        match engine {
            EngineKind::Nnue => StrategyKind::ExpectimaxNnue,
            EngineKind::MctsOnnx => StrategyKind::MctsOnnx,
            EngineKind::MctsDL => StrategyKind::MctsDL,
        }
    }
}

// 选手槽位

/// 引擎搜索参数快照：全量字段 + 钳制规则（必须 ≥ 1）+ 部分更新语义的单一真源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineParams {
    /// Gumbel MCTS 模拟次数（Mcts 类策略 / 手动搜索）
    pub mcts_sims: usize,
    /// Expectimax 强引擎节点预算（Expectimax 策略）
    pub engine_budget: u64,
    /// Expectimax 搜索预算（ExpectimaxNnue 全量；Expectimax 取节点预算与硬超时）
    pub nnue_budget: SearchBudget,
}

impl EngineParams {
    /// 全字段钳制到合法区间。
    pub fn clamped(mut self) -> Self {
        self.mcts_sims = self.mcts_sims.max(1);
        self.engine_budget = self.engine_budget.max(1);
        self.nnue_budget = self.nnue_budget.clamped();
        self
    }

    /// 部分更新：只覆盖 `Some` 字段，返回钳制后的生效快照。
    pub fn updated(
        self,
        mcts_sims: Option<usize>,
        engine_budget: Option<u64>,
        nnue_budget: Option<SearchBudget>,
    ) -> Self {
        let mut p = self;
        if let Some(v) = mcts_sims {
            p.mcts_sims = v;
        }
        if let Some(v) = engine_budget {
            p.engine_budget = v;
        }
        if let Some(v) = nnue_budget {
            p.nnue_budget = v;
        }
        p.clamped()
    }
}

impl Default for EngineParams {
    fn default() -> Self {
        Self {
            mcts_sims: 200,
            engine_budget: 300_000,
            nnue_budget: SearchBudget::default(),
        }
    }
}

/// 单个选手槽位：用户本人操作、交由某种策略托管，或绑定一个 Agent 实体。
#[derive(Debug, Clone)]
pub enum PlayerSlot {
    Human,
    Strategy {
        kind: StrategyKind,
        /// 模型类策略绑定的模型文件（其余为 None）
        model_file: Option<String>,
        /// 引擎参数快照（建局/改槽位时固定）
        params: EngineParams,
    },
    /// 绑定 Agent 实体（策略种类 / 模型 / 搜索配置由 Agent 持有，
    /// 决策时实时读取，搜索配置可在线调整后立即对已绑定对局生效）。
    Agent(Uuid),
}

impl PlayerSlot {
    /// 构造策略槽位。
    pub fn strategy(kind: StrategyKind, model_file: Option<String>, params: EngineParams) -> Self {
        PlayerSlot::Strategy {
            kind,
            model_file,
            params,
        }
    }

    pub fn is_human(&self) -> bool {
        matches!(self, PlayerSlot::Human)
    }

    /// 策略种类（Human / Agent 引用返回 None——Agent 的种类决策时解析）。
    pub fn kind(&self) -> Option<StrategyKind> {
        match self {
            PlayerSlot::Human | PlayerSlot::Agent(_) => None,
            PlayerSlot::Strategy { kind, .. } => Some(*kind),
        }
    }

    /// 绑定的 Agent ID（仅 Agent 槽位）。
    pub fn agent_id(&self) -> Option<Uuid> {
        match self {
            PlayerSlot::Agent(id) => Some(*id),
            _ => None,
        }
    }
}

/// 红 / 黑双方槽位（对局与玩家解耦后的配置载体；双方 Human 即原本地双人）。
#[derive(Debug, Clone)]
pub struct Slots {
    pub red: PlayerSlot,
    pub black: PlayerSlot,
}

impl Slots {
    pub fn new(red: PlayerSlot, black: PlayerSlot) -> Self {
        Self { red, black }
    }

    /// 双方均为 Human（原本地双人 PvP）。
    pub fn all_human() -> Self {
        Self::new(PlayerSlot::Human, PlayerSlot::Human)
    }

    pub fn side(&self, player: banqi_core::core::env::types::Player) -> &PlayerSlot {
        match player {
            banqi_core::core::env::types::Player::Red => &self.red,
            banqi_core::core::env::types::Player::Black => &self.black,
        }
    }

    pub fn side_mut(&mut self, player: banqi_core::core::env::types::Player) -> &mut PlayerSlot {
        match player {
            banqi_core::core::env::types::Player::Red => &mut self.red,
            banqi_core::core::env::types::Player::Black => &mut self.black,
        }
    }
}

// Agent 实体

/// Agent（玩家实体）：一个命名的策略配置——变体 + 策略种类 + 模型文件 +
/// 搜索参数。创建后只读，仅搜索参数可实时调整（对所有绑定它的对局立即生效：
/// 决策时从注册表实时读取，不做建局时快照）。
#[derive(Debug, Clone)]
pub struct Agent {
    pub id: Uuid,
    /// 适用的棋盘变体（对局变体不一致时拒绝参与该对局）
    pub variant: banqi_core::core::env::Variant,
    pub kind: StrategyKind,
    /// 模型类策略绑定的模型文件（其余为 None；创建时已预加载校验）
    pub model_file: Option<String>,
    /// 搜索参数（可经 `PUT /api/agents/{id}/settings` 实时调整）
    pub params: EngineParams,
}

// 统一决策入口

/// 一次策略决策的完整产出（供 run / dry-run / 手动搜索共用）。
pub struct Decision {
    /// 选择的动作（动作空间索引）
    pub action: usize,
    /// MCTS 类策略保留的搜索树（其余为 None；供前端浏览与跨回合复用）
    pub mcts_tree: Option<MctsTreeBox>,
    /// Expectimax+NNUE 搜索轨迹树（可视化用）
    pub ex_tree: Option<crate::app::expectimax_view::ExpectimaxTree>,
    pub mcts_stats: Option<MctsStatsDto>,
    pub nnue_stats: Option<NnueStatsDto>,
    /// 根评估值（行棋方视角；Expectimax 类为搜索值，MCTS 类为根 q 均值）
    pub score: Option<f32>,
}

impl Decision {
    fn plain(action: usize) -> Self {
        Self {
            action,
            mcts_tree: None,
            ex_tree: None,
            mcts_stats: None,
            nnue_stats: None,
            score: None,
        }
    }
}

/// 统一决策入口：给定局面快照与选手槽位，返回一个合法动作及搜索产出。
///
/// 全部策略共用此单一分派（原 `choose_simple` + ai.rs 大 match 的收敛点）：
/// - 规则策略（Random / RevealFirst / CaptureFirst）：廉价，可锁内同步直调；
/// - Expectimax / ExpectimaxNnue / Mcts 类：计算量大 / 需加载模型，
///   调用方须放入 blocking 线程池执行；
/// - `prev_tree`：MCTS 类策略的上一棵搜索树（跨回合复用；复用尝试会消费它，
///   对不上自动回退全新建树）；其余策略忽略。
pub fn decide(
    snap: &PositionSnapshot,
    slot: &PlayerSlot,
    prev_tree: Option<MctsTree<DarkChessEnv>>,
    pool: &ModelPool,
) -> Result<Decision, String> {
    let (kind, model_file, params) = match slot {
        PlayerSlot::Human => {
            return Err("当前行棋方由用户控制，无策略可执行".into());
        }
        // Agent 引用须先由调用方实时解析（resolve_agent_slot）
        PlayerSlot::Agent(_) => {
            return Err("Agent 槽位未解析为策略槽位".into());
        }
        PlayerSlot::Strategy {
            kind,
            model_file,
            params,
        } => (*kind, model_file.clone(), *params),
    };

    let env = DarkChessEnv::from_snapshot(snap).map_err(|e| format!("重建环境失败: {e}"))?;

    match kind {
        // 规则策略：无模型，直调 engine Policy
        StrategyKind::Random => {
            let action = RandomPolicy::choose_action(&env).ok_or("无合法动作")?;
            Ok(Decision::plain(action))
        }
        StrategyKind::RevealFirst => {
            let action = RevealFirstPolicy::choose_action(&env).ok_or("无合法动作")?;
            Ok(Decision::plain(action))
        }
        StrategyKind::CaptureFirst => {
            let action = CaptureFirstPolicy::choose_action(&env).ok_or("无合法动作")?;
            Ok(Decision::plain(action))
        }
        // Expectimax 强引擎：纯规则子力差基线叶评估，无模型
        StrategyKind::Expectimax => {
            let budget = SearchBudget {
                mode: crate::app::search::BudgetMode::Nodes,
                nodes: params.engine_budget,
                timeout_ms: params.nnue_budget.timeout_ms,
                ..Default::default()
            };
            let mut cfg = budget.clamped().to_search_config();
            cfg.nnue_evaluator = Some(Arc::new(RuleMaterialEval::default()));
            let result = expectimax::search(&env, &cfg).ok_or("无合法动作")?;
            let mut d = Decision::plain(result.action);
            d.score = Some(result.value);
            Ok(d)
        }
        // Expectimax + NNUE：.nnue 叶评估 + 搜索轨迹树捕获（可视化约束）
        StrategyKind::ExpectimaxNnue => {
            let file = model_file.ok_or("ExpectimaxNnue 策略未绑定 .nnue 模型文件")?;
            let evaluator = pool.acquire_nnue(&file)?;
            let (action, kept) = crate::app::search::nnue_search_tree(&env, evaluator, params.nnue_budget)?;
            let stats = kept.stats.clone();
            Ok(Decision {
                action,
                mcts_tree: None,
                ex_tree: Some(kept),
                mcts_stats: None,
                nnue_stats: Some(NnueStatsDto {
                    engine: "nnue".to_string(),
                    mode: stats.mode.to_string(),
                    depth: stats.depth,
                    budget: stats.budget,
                    time_limit_ms: stats.time_limit_ms,
                    nodes: stats.nodes,
                    reached_depth: stats.reached_depth,
                    elapsed_ms: stats.elapsed_ms,
                }),
                score: None,
            })
        }
        // Gumbel MCTS 类：推理后端按策略种类分派（feature 门控在此单点维护）
        StrategyKind::MctsOnnx => {
            #[cfg(feature = "onnx")]
            {
                mcts_branch(
                    model_file,
                    "MctsOnnx 策略未绑定 .onnx 模型文件",
                    &env,
                    params.mcts_sims,
                    prev_tree,
                    |f| pool.acquire_onnx(f),
                    banqi_engine::engine::gumbel::mcts_search_onnx::<DarkChessEnv>,
                )
            }
            #[cfg(not(feature = "onnx"))]
            {
                let _ = (model_file, prev_tree);
                Err("服务编译未启用 onnx 推理，MCTS+ONNX 策略不可用（可改用 ExpectimaxNnue）".into())
            }
        }
        StrategyKind::MctsDL => {
            #[cfg(feature = "torch")]
            {
                mcts_branch(
                    model_file,
                    "MctsDL 策略未绑定 .pt 模型文件",
                    &env,
                    params.mcts_sims,
                    prev_tree,
                    |f| pool.acquire_torch(f),
                    banqi_engine::engine::gumbel::mcts_search_torch::<DarkChessEnv>,
                )
            }
            #[cfg(not(feature = "torch"))]
            {
                let _ = (model_file, prev_tree);
                Err("服务编译未启用 torch 推理，MCTS+DL 策略不可用（可改用 MctsOnnx / ExpectimaxNnue）".into())
            }
        }
    }
}

/// MCTS 类策略分支的公共装配（torch / onnx 两臂同构收口）：
/// 模型文件校验 → 随用随加载 → 搜索（含树复用）→ 决策组装。
#[cfg(any(feature = "onnx", feature = "torch"))]
fn mcts_branch<M>(
    file: Option<String>,
    missing: &str,
    env: &DarkChessEnv,
    sims: usize,
    prev_tree: Option<MctsTree<DarkChessEnv>>,
    acquire: impl FnOnce(&str) -> Result<Arc<M>, String>,
    search: impl FnOnce(
        &DarkChessEnv,
        usize,
        Option<Arc<M>>,
        Option<MctsTree<DarkChessEnv>>,
    ) -> Result<MctsTree<DarkChessEnv>, String>,
) -> Result<Decision, String> {
    let file = file.ok_or(missing)?;
    let model = acquire(&file)?;
    let tree = search(env, sims, Some(model), prev_tree)?;
    Ok(mcts_decision(tree))
}

/// MCTS 决策产出组装：动作 + 整树 + 统计 + 根 q 均值。
fn mcts_decision(tree: MctsTree<DarkChessEnv>) -> Decision {
    let action = tree.chosen_action;
    let score = {
        let root = tree.arena.get(tree.root_idx);
        let n = root.visit_count.max(1);
        Some(root.value_sum / n as f32)
    };
    let boxed: MctsTreeBox = Box::new(tree);
    let mcts_stats = MctsStatsDto::from_session(Some(&boxed));
    Decision {
        action,
        mcts_tree: Some(boxed),
        ex_tree: None,
        mcts_stats,
        nnue_stats: None,
        score,
    }
}

/// 非破坏性决策（dry-run）：与 [`decide`] 共用全部分派逻辑，但不消费会话树
/// （全新搜索，不尝试跨回合复用），不落子。返回的树 / 统计由调用方决定
/// 是否入会话（供前端浏览与后续 run 复用）。
pub fn dry_run(snap: &PositionSnapshot, slot: &PlayerSlot, pool: &ModelPool) -> Result<Decision, String> {
    decide(snap, slot, None, pool)
}

// 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::envops;
    use banqi_core::core::env::Variant;

    fn initial_snap() -> PositionSnapshot {
        envops::initial_snapshot(Variant::DarkChess4x8)
    }

    fn human_vs(kind: StrategyKind) -> Slots {
        Slots::new(
            PlayerSlot::Human,
            PlayerSlot::strategy(kind, None, EngineParams::default()),
        )
    }

    #[test]
    fn parses_strategy_strings_with_legacy_aliases() {
        assert_eq!(StrategyKind::parse("Random"), Some(StrategyKind::Random));
        assert_eq!(StrategyKind::parse("reveal_first"), Some(StrategyKind::RevealFirst));
        assert_eq!(StrategyKind::parse("CaptureFirst"), Some(StrategyKind::CaptureFirst));
        assert_eq!(StrategyKind::parse("engine"), Some(StrategyKind::Expectimax));
        assert_eq!(StrategyKind::parse("Expectimax"), Some(StrategyKind::Expectimax));
        assert_eq!(StrategyKind::parse("nnue"), Some(StrategyKind::ExpectimaxNnue));
        assert_eq!(
            StrategyKind::parse("expectimax-nnue"),
            Some(StrategyKind::ExpectimaxNnue)
        );
        assert_eq!(StrategyKind::parse("mcts-onnx"), Some(StrategyKind::MctsOnnx));
        assert_eq!(StrategyKind::parse("mcts_dl"), Some(StrategyKind::MctsDL));
        assert!(StrategyKind::parse("pvp").is_none());
        assert!(StrategyKind::parse("human").is_none());
    }

    #[test]
    fn kind_strings_match_frontend_contract() {
        assert_eq!(StrategyKind::RevealFirst.as_str(), "RevealFirst");
        assert_eq!(StrategyKind::Expectimax.as_str(), "Expectimax");
        assert_eq!(StrategyKind::ExpectimaxNnue.as_str(), "ExpectimaxNnue");
        assert_eq!(StrategyKind::MctsOnnx.as_str(), "MctsOnnx");
        assert_eq!(StrategyKind::MctsDL.as_str(), "MctsDL");
        assert!(!StrategyKind::Random.needs_model());
        assert!(!StrategyKind::Expectimax.needs_model());
        assert!(StrategyKind::ExpectimaxNnue.needs_model());
        assert!(StrategyKind::MctsOnnx.needs_model());
        assert!(StrategyKind::MctsDL.needs_model());
        assert!(StrategyKind::MctsOnnx.is_mcts());
        assert!(!StrategyKind::Expectimax.is_mcts());
    }

    #[test]
    fn slots_side_access() {
        use banqi_core::core::env::types::Player;
        let slots = human_vs(StrategyKind::Expectimax);
        assert!(slots.side(Player::Red).is_human());
        assert!(!slots.side(Player::Black).is_human());
        assert_eq!(slots.side(Player::Black).kind(), Some(StrategyKind::Expectimax));
    }

    #[test]
    fn rule_strategies_return_legal_action() {
        let snap = initial_snap();
        for kind in [
            StrategyKind::Random,
            StrategyKind::RevealFirst,
            StrategyKind::CaptureFirst,
        ] {
            let slot = PlayerSlot::strategy(kind, None, EngineParams::default());
            let d = decide(&snap, &slot, None, &dummy_pool())
                .expect("初始局面必有合法动作");
            let legal = envops::legal_actions(
                &DarkChessEnv::from_snapshot(&snap).expect("重建不应失败"),
            );
            assert!(legal.contains(&d.action), "{kind:?} 选出的动作必须在合法集合内");
            assert!(d.mcts_tree.is_none() && d.ex_tree.is_none());
        }
    }

    #[test]
    fn expectimax_returns_legal_action_with_score() {
        let snap = initial_snap();
        let slot = PlayerSlot::strategy(
            StrategyKind::Expectimax,
            None,
            EngineParams {
                engine_budget: 1_000,
                ..EngineParams::default()
            },
        );
        let d = decide(&snap, &slot, None, &dummy_pool()).expect("小预算搜索应返回动作");
        let legal = envops::legal_actions(
            &DarkChessEnv::from_snapshot(&snap).expect("重建不应失败"),
        );
        assert!(legal.contains(&d.action));
        assert!(d.score.is_some(), "Expectimax 决策应携带根评估值");
    }

    #[test]
    fn human_slot_decision_errors() {
        let snap = initial_snap();
        assert!(decide(&snap, &PlayerSlot::Human, None, &dummy_pool()).is_err());
    }

    #[test]
    fn engine_params_partial_update_clamps() {
        let base = EngineParams::default();
        let p = base.updated(
            Some(0),
            Some(0),
            Some(SearchBudget { nodes: 0, depth: 0, timeout_ms: 0, ..Default::default() }),
        );
        assert_eq!(p.mcts_sims, 1);
        assert_eq!(p.engine_budget, 1);
        assert_eq!(p.nnue_budget.nodes, 1);
        assert_eq!(p.nnue_budget.depth, 1);
        assert_eq!(p.nnue_budget.timeout_ms, 1);
        let p = base.updated(None, Some(500_000), None);
        assert_eq!(p.engine_budget, 500_000);
        assert_eq!(p.mcts_sims, base.mcts_sims);
        assert_eq!(p.nnue_budget, base.nnue_budget);
    }

    /// 测试用空模型池（模型目录不存在；规则 / Expectimax 策略不加载模型）。
    fn dummy_pool() -> ModelPool {
        ModelPool::new(
            &std::env::temp_dir().join("banqi-player-test-no-models"),
            1,
            "cpu",
            0,
        )
    }
}
