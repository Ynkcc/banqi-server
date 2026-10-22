//! 对手抽象（共享）：规则对手 / 纯计算引擎的同步决策 + 引擎参数快照。
//!
//! 收敛 banqi-gui/src/game_cmds.rs（`bot_move` 的规则对手与 Engine 分支）与
//! banqi-server/src/ai.rs（对手配置）中重复的「对手 → 动作」分派：
//!
//! - [`RuleKind`] / [`SimpleKind`] / [`OpponentKind`]：对手种类与前端
//!   `Opponent` 字符串契约一一对应（"Random" / "RevealFirst" / "CaptureFirst" /
//!   "Engine" / "Nnue" / "MctsOnnx"）；
//! - [`choose_simple`]：无模型对手的同步决策入口（规则对手廉价可同步调用；
//!   Engine 分支计算量大，调用方自行 `spawn_blocking`）；
//! - [`EngineParams`]：gui `EngineSettings` 与 server `/api/settings` 的
//!   引擎参数单一真源（钳制规则与部分更新语义在此统一）。
//!
//! 设计约束：纯同步逻辑，不依赖 tauri / axum / tokio；模型类对手
//! （Nnue / MctsOnnx）的搜索执行仍在 [`crate::search`]，由调用方组装。

use std::sync::Arc;

use banqi_core::core::env::{DarkChessEnv, PositionSnapshot, SnapshotEnv};
use banqi_core::core::expectimax::rule_eval::RuleMaterialEval;
use banqi_core::core::expectimax;
use banqi_engine::engine::{CaptureFirstPolicy, Policy, RandomPolicy, RevealFirstPolicy};

use crate::app::models::EngineKind;
use crate::app::search::SearchBudget;

// 对手种类

/// 规则对手种类（轻量基线 / 陪练）。
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

    /// 解析前端对手字符串（大小写不敏感，接受 camelCase / snake_case 别名）。
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

/// 无模型对手：规则对手或纯计算强引擎（αβ + Star1 + 置换表 + 迭代加深）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleKind {
    Rule(RuleKind),
    Engine,
}

impl SimpleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SimpleKind::Rule(r) => r.as_str(),
            SimpleKind::Engine => "Engine",
        }
    }

    /// 解析前端对手字符串（规则对手 + "Engine"；"PvP" 不属于对手种类）。
    pub fn parse(s: &str) -> Option<Self> {
        if s.trim().eq_ignore_ascii_case("engine") {
            return Some(SimpleKind::Engine);
        }
        RuleKind::parse(s).map(SimpleKind::Rule)
    }
}

/// 对手种类全集（含模型后端；字符串契约与前端 `Opponent` 枚举一致，
/// 含 Tauri 侧的 torch 路径 "MctsDL"，需 torch feature 支持实际执行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpponentKind {
    Rule(RuleKind),
    Engine,
    Nnue,
    MctsOnnx,
    /// Gumbel MCTS + TorchScript（.pt，需 torch feature）
    MctsDL,
}

impl OpponentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OpponentKind::Rule(r) => r.as_str(),
            OpponentKind::Engine => "Engine",
            OpponentKind::Nnue => "Nnue",
            OpponentKind::MctsOnnx => "MctsOnnx",
            OpponentKind::MctsDL => "MctsDL",
        }
    }

    /// 解析前端对手字符串（模型类对手按名解析；模型文件仍需单独给出）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "nnue" => return Some(OpponentKind::Nnue),
            "mctsonnx" => return Some(OpponentKind::MctsOnnx),
            "mctsdl" => return Some(OpponentKind::MctsDL),
            _ => {}
        }
        SimpleKind::parse(s).map(|k| match k {
            SimpleKind::Rule(r) => OpponentKind::Rule(r),
            SimpleKind::Engine => OpponentKind::Engine,
        })
    }

    /// 是否需要绑定模型文件（建局 / 走子前须已加载）。
    pub fn needs_model(self) -> bool {
        matches!(self, OpponentKind::Nnue | OpponentKind::MctsOnnx | OpponentKind::MctsDL)
    }

    /// 模型类对手对应的推理后端（规则 / Engine 对手返回 None）。
    /// 引擎字符串下发（`EngineKind::as_str`）与模型预加载均由此单点换算。
    pub fn engine_kind(self) -> Option<EngineKind> {
        match self {
            OpponentKind::Nnue => Some(EngineKind::Nnue),
            OpponentKind::MctsOnnx => Some(EngineKind::MctsOnnx),
            OpponentKind::MctsDL => Some(EngineKind::MctsDL),
            _ => None,
        }
    }

    /// 由推理后端构造对应的模型类对手（传统路径推断 / 换绑模型时切换对手种类）。
    pub fn from_engine_kind(engine: EngineKind) -> Self {
        match engine {
            EngineKind::Nnue => OpponentKind::Nnue,
            EngineKind::MctsOnnx => OpponentKind::MctsOnnx,
            EngineKind::MctsDL => OpponentKind::MctsDL,
        }
    }
}

// 无模型对手的同步决策

/// 规则对手 / 纯计算引擎的同步决策：给定快照返回一个合法动作。
/// 规则对手廉价，可直接同步调用（忽略 `budget`）；Engine 计算量大，
/// 调用方须放入 blocking 线程池执行；模型类对手不经此函数。
pub fn choose_simple(
    kind: SimpleKind,
    snap: &PositionSnapshot,
    budget: &SearchBudget,
) -> Result<usize, String> {
    let env = DarkChessEnv::from_snapshot(snap).map_err(|e| format!("重建环境失败: {e}"))?;
    let action = match kind {
        SimpleKind::Rule(r) => match r {
            RuleKind::Random => RandomPolicy::choose_action(&env),
            RuleKind::RevealFirst => RevealFirstPolicy::choose_action(&env),
            RuleKind::CaptureFirst => CaptureFirstPolicy::choose_action(&env),
        },
        SimpleKind::Engine => {
            // 纯计算引擎：无模型，注入 core 的纯规则子力差基线叶评估器
            let mut cfg = budget.clamped().to_search_config();
            cfg.nnue_evaluator = Some(Arc::new(RuleMaterialEval::default()));
            expectimax::search(&env, &cfg).map(|r| r.action)
        }
    };
    action.ok_or_else(|| "无合法动作".to_string())
}

// 引擎参数快照（server /api/settings 的单一真源）

/// 引擎搜索参数快照：全量字段 + 钳制规则（必须 ≥ 1）+ 部分更新语义的单一真源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineParams {
    /// Gumbel MCTS 模拟次数（MctsOnnx 对手 / 手动搜索）
    pub mcts_sims: usize,
    /// Expectimax 强引擎节点预算（Engine 对手）
    pub engine_budget: u64,
    /// Expectimax 搜索预算（NNUE 对手：模式 + 节点/深度/时间 + 硬超时；
    /// Engine 对手取其节点预算与硬超时）
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
        // 与 banqi-gui 初始值一致（gui main.rs / server ServerConfig）
        Self {
            mcts_sims: 200,
            engine_budget: 300_000,
            nnue_budget: SearchBudget::default(),
        }
    }
}

// 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::envops;
    use crate::app::search::BudgetMode;
    use banqi_core::core::env::Variant;

    fn initial_snap() -> PositionSnapshot {
        envops::initial_snapshot(Variant::DarkChess4x8)
    }

    #[test]
    fn parses_opponent_strings_case_insensitive() {
        assert_eq!(OpponentKind::parse("Random"), Some(OpponentKind::Rule(RuleKind::Random)));
        assert_eq!(
            OpponentKind::parse("reveal_first"),
            Some(OpponentKind::Rule(RuleKind::RevealFirst))
        );
        assert_eq!(
            OpponentKind::parse("CaptureFirst"),
            Some(OpponentKind::Rule(RuleKind::CaptureFirst))
        );
        assert_eq!(OpponentKind::parse("engine"), Some(OpponentKind::Engine));
        assert_eq!(OpponentKind::parse("Nnue"), Some(OpponentKind::Nnue));
        assert_eq!(OpponentKind::parse("mcts-onnx"), Some(OpponentKind::MctsOnnx));
        assert_eq!(OpponentKind::parse("mcts_dl"), Some(OpponentKind::MctsDL));
        assert!(OpponentKind::parse("pvp").is_none());
        assert_eq!(
            OpponentKind::parse("Random").expect("规则对手").as_str(),
            "Random"
        );
    }

    #[test]
    fn kind_strings_match_frontend_contract() {
        assert_eq!(OpponentKind::Rule(RuleKind::RevealFirst).as_str(), "RevealFirst");
        assert_eq!(OpponentKind::Engine.as_str(), "Engine");
        assert_eq!(OpponentKind::Nnue.as_str(), "Nnue");
        assert_eq!(OpponentKind::MctsOnnx.as_str(), "MctsOnnx");
        assert_eq!(OpponentKind::MctsDL.as_str(), "MctsDL");
        assert!(!OpponentKind::Rule(RuleKind::Random).needs_model());
        assert!(OpponentKind::Nnue.needs_model());
        assert!(OpponentKind::MctsOnnx.needs_model());
        assert!(OpponentKind::MctsDL.needs_model());
    }

    #[test]
    fn choose_simple_rule_returns_legal_action() {
        let snap = initial_snap();
        for kind in [
            SimpleKind::Rule(RuleKind::Random),
            SimpleKind::Rule(RuleKind::RevealFirst),
            SimpleKind::Rule(RuleKind::CaptureFirst),
        ] {
            let action = choose_simple(kind, &snap, &SearchBudget::default())
                .expect("初始局面必有合法动作");
            let legal = envops::legal_actions(
                &DarkChessEnv::from_snapshot(&snap).expect("重建不应失败"),
            );
            assert!(legal.contains(&action), "{kind:?} 选出的动作必须在合法集合内");
        }
    }

    #[test]
    fn choose_simple_engine_returns_legal_action() {
        // 纯计算引擎：规则叶评估器驱动，无任何模型依赖（节点预算 + 超时约束）
        let snap = initial_snap();
        let budget = SearchBudget {
            mode: BudgetMode::Nodes,
            nodes: 1_000,
            ..Default::default()
        };
        let action = choose_simple(SimpleKind::Engine, &snap, &budget).expect("小预算搜索应返回动作");
        let legal = envops::legal_actions(
            &DarkChessEnv::from_snapshot(&snap).expect("重建不应失败"),
        );
        assert!(legal.contains(&action));
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
}
