//! Expectimax 搜索轨迹树可视化（DTO 与只读查询）。
//!
//! 树由 `app::search::nnue_search_tree`（`collect_tree` 开启）产生并保留在
//! `GameSession` 中，供前端浏览引擎的搜索轨迹（Alpha-Beta 搜索轨迹语义，
//! 见 `banqi-core` `expectimax::tree` 模块文档）：
//! - 节点值为「当前节点走子方视角」；边展示给父节点视角须取负（`ExEdge.value`）；
//! - `flag` 区分精确值与界（Alpha-Beta 非 PV 分支是界不是精确值）；
//! - `mark` 标注 TT 截断 / razoring / null-move 剪枝（TT 截断子树未展开）。
//!
//! 生命周期约定：每次 NNUE 对手落子或手动搜索替换整棵树；查询均为只读投影。

use serde::Serialize;

use banqi_core::core::env::actions::action_lookup_tables;
use banqi_core::core::env::{DarkChessEnv, SnapshotEnv};
use banqi_core::core::expectimax::{BoundFlag, ExNode, ExTree};

use crate::app::common::player_label;
use crate::app::search::NnueSearchStats;

// ============================================================================
// DTO（snake_case，契约真源为 banqi-frontend/src/api/types.ts）
// ============================================================================

/// 「边」摘要：父节点到子节点的搜索轨迹边。
#[derive(Debug, Clone, Serialize)]
pub struct ExEdge {
    pub child_id: usize,
    /// 决策边 = 动作索引；机会 outcome 边 = outcome_id；根 / null 探测 = 哨兵
    pub action: usize,
    /// 子节点值换算到父节点视角（= -child.value；前端展示用）
    pub value: f32,
    pub is_chance: bool,
    pub chance_prob: f32,
    /// 机会决策节点的 outcome 子边
    pub is_outcome: bool,
    /// 子节点剩余深度
    pub depth: i32,
    /// "exact" / "lower" / "upper" / "unknown"
    pub flag: String,
    /// "tt" / "razor" / "nmp"；无标记为 None
    pub mark: Option<String>,
    /// 子节点是否有子（前端决定可否展开）
    pub child_count: usize,
}

/// 节点摘要。
#[derive(Debug, Clone, Serialize)]
pub struct ExNodeSummary {
    pub id: usize,
    pub action: usize,
    /// 自身走子方视角值（[-1,1]）
    pub value: f32,
    /// 入口窗口（flag 派生依据）
    pub alpha: f32,
    pub beta: f32,
    /// 剩余搜索深度
    pub depth: i32,
    pub flag: String,
    pub mark: Option<String>,
    pub is_chance: bool,
    pub is_outcome: bool,
    pub chance_prob: f32,
    pub is_root: bool,
    /// null-move 探测节点（虚拟跳过回合，非真实着法）
    pub is_null_probe: bool,
    /// "Red" / "Black"
    pub player: String,
    pub child_count: usize,
}

/// Expectimax 搜索统计（`NnueSearchStats` 的可序列化投影）。
#[derive(Debug, Clone, Serialize)]
pub struct ExSearchStats {
    pub mode: String,
    pub depth: i32,
    pub budget: u64,
    pub time_limit_ms: u64,
    pub nodes: u64,
    pub reached_depth: i32,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExRootInfo {
    pub root: ExNodeSummary,
    pub chosen_action: usize,
    pub stats: ExSearchStats,
}

/// 节点检视的完整局面 + 到达着法所在格（与 MCTS 节点棋盘同构）。
#[derive(Debug, Clone, Serialize)]
pub struct ExNodeBoard {
    /// `app::view::game_state_view` 直出（与状态端点共用构造器）
    pub state: crate::app::view::GameStateView,
    /// 翻棋 1 格 / 移动·炮击 2 格（from, to）；根 / null 探测为空
    pub move_coords: Vec<usize>,
}

// ============================================================================
// 树实体 + 只读查询
// ============================================================================

/// 一次 NNUE Expectimax 搜索保留的轨迹树（挂 `GameSession`）。
#[derive(Debug, Clone)]
pub struct ExpectimaxTree {
    pub tree: ExTree,
    pub chosen_action: usize,
    pub stats: NnueSearchStats,
}

pub fn flag_label(flag: BoundFlag) -> String {
    match flag {
        BoundFlag::Exact => "exact".into(),
        BoundFlag::Lower => "lower".into(),
        BoundFlag::Upper => "upper".into(),
        BoundFlag::Unknown => "unknown".into(),
    }
}

pub fn mark_label(mark: Option<banqi_core::core::expectimax::ExMark>) -> Option<String> {
    use banqi_core::core::expectimax::ExMark;
    mark.map(|m| match m {
        ExMark::Tt => "tt".into(),
        ExMark::Razor => "razor".into(),
        ExMark::Nmp => "nmp".into(),
    })
}

fn node_player(node: &ExNode) -> String {
    node.env
        .as_ref()
        .map(|e| player_label(e.get_current_player()))
        .unwrap_or_else(|| "Unknown".into())
}

fn node_summary(tree: &ExTree, id: usize) -> ExNodeSummary {
    let n = &tree.nodes[id];
    ExNodeSummary {
        id,
        action: n.action,
        value: n.value,
        alpha: n.alpha,
        beta: n.beta,
        depth: n.depth,
        flag: flag_label(n.flag),
        mark: mark_label(n.mark),
        is_chance: n.is_chance,
        is_outcome: n.is_outcome,
        chance_prob: n.chance_prob,
        is_root: n.action == banqi_core::core::expectimax::tree::ROOT_ACTION,
        is_null_probe: n.action == banqi_core::core::expectimax::tree::NULL_ACTION,
        player: node_player(n),
        child_count: tree.nodes.iter().filter(|c| c.parent == Some(id)).count(),
    }
}

/// 根信息（树为空时返回 None）。
pub fn ex_root_info(t: &ExpectimaxTree) -> Option<ExRootInfo> {
    if t.tree.nodes.is_empty() {
        return None;
    }
    Some(ExRootInfo {
        root: node_summary(&t.tree, 0),
        chosen_action: t.chosen_action,
        stats: ExSearchStats {
            mode: t.stats.mode.to_string(),
            depth: t.stats.depth,
            budget: t.stats.budget,
            time_limit_ms: t.stats.time_limit_ms,
            nodes: t.stats.nodes,
            reached_depth: t.stats.reached_depth,
            elapsed_ms: t.stats.elapsed_ms,
        },
    })
}

/// 收集某节点的全部子边（保持搜索顺序；前端按窗口宽度折叠展示）。
pub fn ex_edge_list(tree: &ExTree, node_id: usize) -> Vec<ExEdge> {
    tree.nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent == Some(node_id))
        .map(|(child_id, c)| ExEdge {
            child_id,
            action: c.action,
            value: -c.value,
            is_chance: c.is_chance,
            chance_prob: c.chance_prob,
            is_outcome: c.is_outcome,
            depth: c.depth,
            flag: flag_label(c.flag),
            mark: mark_label(c.mark),
            child_count: tree.nodes.iter().filter(|n| n.parent == Some(child_id)).count(),
        })
        .collect()
}

/// 节点摘要（越界返回 None）。
pub fn ex_node_summary(tree: &ExTree, node_id: usize) -> Option<ExNodeSummary> {
    tree.nodes.get(node_id).map(|_| node_summary(tree, node_id))
}

/// 节点棋盘检视：由节点内保存的环境副本直出局面视图。
pub fn ex_node_board(tree: &ExTree, node_id: usize) -> Option<ExNodeBoard> {
    let env: &DarkChessEnv = tree.nodes.get(node_id)?.env.as_ref()?;
    let snap = env.to_snapshot();
    let move_coords = (snap.last_action >= 0)
        .then_some(snap.last_action as usize)
        .and_then(|a| {
            action_lookup_tables(&snap.variant.config())
                .action_to_coords
                .get(a)
                .cloned()
        })
        .unwrap_or_default();
    let state = crate::app::view::game_state_view(&snap);
    Some(ExNodeBoard { state, move_coords })
}

/// 树统计（节点总数 / 最大深度，O(n) 扫描）。
pub fn ex_tree_stats(tree: &ExTree) -> (usize, usize) {
    let nodes = tree.nodes.len();
    let max_depth = tree
        .nodes
        .iter()
        .filter(|n| n.parent.is_some())
        .map(|n| {
            // 自根到该节点的链长（parent 下标恒小于子下标，逐级回溯安全）
            let mut d = 0usize;
            let mut cur = n.parent;
            while let Some(p) = cur {
                d += 1;
                cur = tree.nodes[p].parent;
            }
            d
        })
        .max()
        .unwrap_or(0);
    (nodes, max_depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> ExTree {
        use banqi_core::core::expectimax::ExMark;
        let mut t = ExTree::default();
        let mut n0 = sample_node(None);
        n0.action = banqi_core::core::expectimax::tree::ROOT_ACTION;
        n0.value = 0.3;
        t.nodes.push(n0);
        let mut n1 = sample_node(Some(0));
        n1.flag = BoundFlag::Lower;
        n1.mark = Some(ExMark::Tt);
        n1.value = -0.3;
        t.nodes.push(n1);
        let mut n2 = sample_node(Some(0));
        n2.is_chance = true;
        t.nodes.push(n2);
        let mut n3 = sample_node(Some(2));
        n3.is_outcome = true;
        n3.chance_prob = 0.25;
        n3.value = 0.1;
        t.nodes.push(n3);
        t
    }

    fn sample_node(parent: Option<usize>) -> ExNode {
        ExNode {
            parent,
            action: 7,
            is_chance: false,
            is_outcome: false,
            chance_prob: 1.0,
            depth: 4,
            value: 0.0,
            alpha: -1.0,
            beta: 1.0,
            flag: BoundFlag::Exact,
            mark: None,
            env: None,
        }
    }

    #[test]
    fn edge_values_are_parent_perspective() {
        let t = sample_tree();
        let edges = ex_edge_list(&t, 0);
        assert_eq!(edges.len(), 2, "节点 0 的子边 = n1 / n2");
        let tt_edge = edges.iter().find(|e| e.child_id == 1).unwrap();
        assert!((tt_edge.value - 0.3).abs() < 1e-6, "边值 = -child.value（父视角）");
        assert_eq!(tt_edge.flag, "lower");
        assert_eq!(tt_edge.mark.as_deref(), Some("tt"));
        // 机会节点的子边：outcome 携带概率
        let outcome = ex_edge_list(&t, 2);
        assert_eq!(outcome.len(), 1);
        assert!(outcome[0].is_outcome);
        assert!((outcome[0].chance_prob - 0.25).abs() < 1e-6);
    }

    #[test]
    fn out_of_range_returns_none() {
        let t = sample_tree();
        assert!(ex_node_summary(&t, 99).is_none());
        assert!(ex_node_board(&t, 0).is_none(), "env 未保存时棋盘查询为 None");
    }

    #[test]
    fn tree_stats_counts_depth() {
        let t = sample_tree();
        let (nodes, max_depth) = ex_tree_stats(&t);
        assert_eq!(nodes, 4);
        assert_eq!(max_depth, 2);
    }
}
