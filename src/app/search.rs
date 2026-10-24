use banqi_core::core::env::DarkChessEnv;
use banqi_core::core::expectimax::{SearchConfig, SearchResult};
use banqi_engine::nnue::NnueEvaluator;
use std::sync::Arc;
use std::time::Instant;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetMode {
    Nodes,
    DepthNodes,
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
    pub fn parse(s: &str) -> Option<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_', '+', ' '], "")
            .as_str()
        {
            "nodes" => Some(BudgetMode::Nodes),
            "depthnodes" => Some(BudgetMode::DepthNodes),
            "time" => Some(BudgetMode::Time),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudget {
    pub mode: BudgetMode,
    pub nodes: u64,
    pub depth: i32,
    pub time_ms: u64,
    pub timeout_ms: u64,
}
impl Default for SearchBudget {
    fn default() -> Self {
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
    pub fn clamped(mut self) -> Self {
        self.nodes = self.nodes.max(1);
        self.depth = self.depth.max(1);
        self.timeout_ms = self.timeout_ms.max(1);
        self
    }
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
            _ => 64,
        };
        cfg
    }
}
#[derive(Debug, Clone)]
pub struct NnueSearchStats {
    pub mode: &'static str,
    pub depth: i32,
    pub budget: u64,
    pub time_limit_ms: u64,
    pub nodes: u64,
    pub reached_depth: i32,
    pub elapsed_ms: u64,
}
fn nnue_stats_from(
    budget: &SearchBudget,
    cfg: &SearchConfig,
    nodes: u64,
    reached_depth: i32,
    elapsed_ms: u64,
) -> NnueSearchStats {
    NnueSearchStats {
        mode: budget.mode.as_str(),
        depth: if budget.mode == BudgetMode::DepthNodes {
            cfg.max_depth
        } else {
            0
        },
        budget: if budget.mode != BudgetMode::Time {
            cfg.node_budget
        } else {
            0
        },
        time_limit_ms: cfg.time_limit_ms,
        nodes,
        reached_depth,
        elapsed_ms,
    }
}
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
    cfg.threads = 1;
    cfg.features &= !FEAT_ASP;
    cfg.collect_tree = true;
    let started = Instant::now();
    let SearchResult {
        action,
        depth: reached_depth,
        nodes,
        tree,
        ..
    } = banqi_core::core::expectimax::search(env, &cfg)
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
        assert_eq!(
            BudgetMode::parse("DepthNodes"),
            Some(BudgetMode::DepthNodes)
        );
        assert_eq!(
            BudgetMode::parse("depth+nodes"),
            Some(BudgetMode::DepthNodes)
        );
        assert_eq!(BudgetMode::parse("time"), Some(BudgetMode::Time));
        assert!(BudgetMode::parse("mcts").is_none());
    }
    #[test]
    fn nodes_mode_uses_timeout_as_time_limit() {
        let b = SearchBudget {
            mode: BudgetMode::Nodes,
            nodes: 5_000,
            ..Default::default()
        };
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
