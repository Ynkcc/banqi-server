//! 搜索执行（server）：Expectimax 搜索预算与 NNUE 轨迹树搜索。
//!
//! Gumbel MCTS 编排（`run_gumbel` / `tree_reuse_plan` / `mcts_search_impl` /
//! `mcts_search_torch` / `mcts_search_onnx`）已下沉 `banqi-engine::engine::gumbel`
//! （变体无关单一真源），本模块仅保留 server 侧的预算定义与 NNUE 搜索：
//! - 全部为同步（blocking）函数，异步包装（`spawn_blocking`）由调用方实现；
//! - [`nnue_search_tree`] 的可视化约束（threads = 1 / 关 ASP / 树捕获）在此单点施加。

use std::sync::Arc;
use std::time::Instant;

use banqi_core::core::env::DarkChessEnv;
use banqi_core::core::expectimax::{SearchConfig, SearchResult};
use banqi_engine::nnue::NnueEvaluator;

// Expectimax 搜索预算（NNUE 对手 / 纯计算引擎共用）

/// 预算模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetMode {
    /// 纯节点预算（深度不限，由节点预算截断）
    Nodes,
    /// 深度 + 节点预算（双重约束，先到为准）
    DepthNodes,
    /// 用时（毫秒；节点与深度不限）
    Time,
}

impl BudgetMode {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetMode::Nodes => "nodes",
            BudgetMode::DepthNodes => "depth_nodes",
            BudgetMode::Time => "time",
        }
    }

    /// 解析预算模式字符串（大小写不敏感，接受 `depth_nodes` / `depth-nodes` /
    /// `depth+nodes` / `DepthNodes` 等写法）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace(['-', '_', '+', ' '], "").as_str() {
            "nodes" => Some(BudgetMode::Nodes),
            "depthnodes" => Some(BudgetMode::DepthNodes),
            "time" => Some(BudgetMode::Time),
            _ => None,
        }
    }
}

/// Expectimax 搜索预算：三种模式 + 硬超时。
/// `timeout_ms` 为硬超时（必选，默认 60s），对全部模式生效：超时放弃后续搜索，
/// 返回最近完成迭代层的实际结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudget {
    pub mode: BudgetMode,
    /// 节点预算（Nodes / DepthNodes 模式生效）
    pub nodes: u64,
    /// 深度上限（DepthNodes 模式生效）
    pub depth: i32,
    /// 时间预算毫秒（Time 模式生效；0 = 视为仅超时控制）
    pub time_ms: u64,
    /// 硬超时毫秒（全模式必选生效；默认 60_000）
    pub timeout_ms: u64,
}

impl Default for SearchBudget {
    fn default() -> Self {
        // 与历史默认行为一致：深度 8 + 节点 20 万双重约束（gui/server 原语义）
        Self {
            mode: BudgetMode::DepthNodes,
            nodes: 200_000,
            depth: 8,
            time_ms: 5_000,
            timeout_ms: 60_000,
        }
    }
}

impl SearchBudget {
    /// 全字段钳制到合法区间（timeout ≥ 1，其余按模式语义 ≥ 1 / ≥ 0）。
    pub fn clamped(mut self) -> Self {
        self.nodes = self.nodes.max(1);
        self.depth = self.depth.max(1);
        self.timeout_ms = self.timeout_ms.max(1);
        self
    }

    /// 转为 core `SearchConfig`（预算/时间/深度约束在此单点换算）：
    /// - 时间上限 = min(模式时间, timeout)；Nodes/DepthNodes 模式 = timeout；
    /// - 节点预算 = Time 模式不限（`u64::MAX`），其余 = nodes；
    /// - 深度上限 = DepthNodes 模式 = depth，其余 64（实际由预算/超时截断）。
    pub fn to_search_config(&self) -> SearchConfig {
        let mut cfg = SearchConfig::default();
        let timeout = self.timeout_ms.max(1);
        cfg.time_limit_ms = match self.mode {
            BudgetMode::Time if self.time_ms > 0 => self.time_ms.min(timeout),
            _ => timeout,
        };
        cfg.node_budget = match self.mode {
            BudgetMode::Time => u64::MAX,
            _ => self.nodes.max(1),
        };
        cfg.max_depth = match self.mode {
            BudgetMode::DepthNodes => self.depth.max(1),
            // 「不限」深度取 64：远超预算/超时可达深度，仅作迭代循环上界
            _ => 64,
        };
        cfg
    }
}

/// 一次 Expectimax（NNUE / 规则叶评估）搜索的统计（不含后端标识；DTO 由调用方组装）。
#[derive(Debug, Clone)]
pub struct NnueSearchStats {
    /// 生效的预算模式（`BudgetMode::as_str()`）
    pub mode: &'static str,
    /// 请求的迭代加深深度上限（0 = 不限，由预算/超时控制）
    pub depth: i32,
    /// 请求的节点预算（0 = 不限）
    pub budget: u64,
    /// 生效的时间上限毫秒（模式时间与硬超时的较小者；恒 ≥ 1）
    pub time_limit_ms: u64,
    /// 实际消耗的节点数
    pub nodes: u64,
    /// 完成的最深迭代层数
    pub reached_depth: i32,
    /// 搜索本体耗时（毫秒）
    pub elapsed_ms: u64,
}

/// 由预算/生效配置/搜索结果组装统计（`nnue_search_tree` 单点使用；
/// 此前与已删除的 `nnue_search` 逐字重复 9 行）。
fn nnue_stats_from(
    budget: &SearchBudget,
    cfg: &SearchConfig,
    nodes: u64,
    reached_depth: i32,
    elapsed_ms: u64,
) -> NnueSearchStats {
    NnueSearchStats {
        mode: budget.mode.as_str(),
        depth: if budget.mode == BudgetMode::DepthNodes { cfg.max_depth } else { 0 },
        budget: if budget.mode != BudgetMode::Time { cfg.node_budget } else { 0 },
        time_limit_ms: cfg.time_limit_ms,
        nodes,
        reached_depth,
        elapsed_ms,
    }
}

/// Expectimax + NNUE 搜索一次并捕获搜索轨迹树（可视化；同步）。
/// 搜索入口校验 `.nnue` 头部布局（跨变体权重硬失败）；预算约束由
/// [`SearchBudget`] 单点换算。可视化约束：`threads = 1`（多线程搜索碎片
/// 无法连贯成树）、剔除 `FEAT_ASP`（fail 重搜污染同层树）、`collect_tree = true`。
/// 返回 `(选择的动作, ExpectimaxTree)`；预算耗尽时树为最近完成层。
pub fn nnue_search_tree(
    env: &DarkChessEnv,
    evaluator: Arc<NnueEvaluator>,
    budget: SearchBudget,
) -> Result<(usize, crate::app::expectimax_view::ExpectimaxTree), String> {
    use banqi_core::core::expectimax::FEAT_ASP;

    let budget = budget.clamped();
    evaluator
        .validate_layout(&env.config)
        .map_err(|e| format!(".nnue 权重与当前变体布局不匹配: {e}"))?;
    let mut cfg = budget.to_search_config();
    cfg.nnue_evaluator = Some(evaluator);
    // 可视化约束：单线程 + 关闭 aspiration + 开启树捕获
    cfg.threads = 1;
    cfg.features &= !FEAT_ASP;
    cfg.collect_tree = true;
    let started = Instant::now();
    let SearchResult { action, depth: reached_depth, nodes, tree, .. } =
        banqi_core::core::expectimax::search(env, &cfg)
            .ok_or_else(|| "Expectimax 未返回动作（无合法动作）".to_string())?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    Ok((
        action,
        crate::app::expectimax_view::ExpectimaxTree {
            tree: tree.unwrap_or_default(),
            chosen_action: action,
            stats: nnue_stats_from(&budget, &cfg, nodes, reached_depth, elapsed_ms),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_mode_parse() {
        assert_eq!(BudgetMode::parse("nodes"), Some(BudgetMode::Nodes));
        assert_eq!(BudgetMode::parse("DepthNodes"), Some(BudgetMode::DepthNodes));
        assert_eq!(BudgetMode::parse("depth+nodes"), Some(BudgetMode::DepthNodes));
        assert_eq!(BudgetMode::parse("time"), Some(BudgetMode::Time));
        assert!(BudgetMode::parse("mcts").is_none());
    }

    #[test]
    fn nodes_mode_uses_timeout_as_time_limit() {
        let b = SearchBudget { mode: BudgetMode::Nodes, nodes: 5_000, ..Default::default() };
        let cfg = b.to_search_config();
        assert_eq!(cfg.node_budget, 5_000);
        assert_eq!(cfg.max_depth, 64, "纯节点预算深度不限");
        assert_eq!(cfg.time_limit_ms, 60_000, "硬超时兜底");
    }

    #[test]
    fn depth_nodes_mode_applies_both() {
        let b = SearchBudget {
            mode: BudgetMode::DepthNodes,
            nodes: 300_000,
            depth: 6,
            timeout_ms: 10_000,
            ..Default::default()
        };
        let cfg = b.to_search_config();
        assert_eq!(cfg.node_budget, 300_000);
        assert_eq!(cfg.max_depth, 6);
        assert_eq!(cfg.time_limit_ms, 10_000, "硬超时仍生效");
    }

    #[test]
    fn time_mode_unlimits_nodes_and_caps_by_timeout() {
        let b = SearchBudget {
            mode: BudgetMode::Time,
            time_ms: 3_000,
            timeout_ms: 2_000,
            ..Default::default()
        };
        let cfg = b.to_search_config();
        assert_eq!(cfg.node_budget, u64::MAX, "用时模式节点不限");
        assert_eq!(cfg.time_limit_ms, 2_000, "时间预算被硬超时钳制");
        assert_eq!(cfg.max_depth, 64);
    }

    #[test]
    fn clamped_rejects_zero() {
        let b = SearchBudget {
            mode: BudgetMode::DepthNodes,
            nodes: 0,
            depth: 0,
            time_ms: 0,
            timeout_ms: 0,
        }
        .clamped();
        assert_eq!((b.nodes, b.depth, b.timeout_ms), (1, 1, 1));
        assert_eq!(b.time_ms, 0, "time_ms 允许 0（= 仅超时控制）");
    }
}
