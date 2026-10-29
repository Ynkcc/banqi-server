use crate::app::common::player_label;
use crate::app::search::NnueSearchStats;
use banqi_core::core::env::actions::action_lookup_tables;
use banqi_core::core::env::{DarkChessEnv, SnapshotEnv};
use banqi_core::core::expectimax::{BoundFlag, ExNode, ExTree};
use serde::Serialize;
pub use bqsrv_dto::{ExNodeSummary, ExRootInfo, ExSearchStats};
#[derive(Debug, Clone, Serialize)]
pub struct ExEdge {
    pub child_id: usize,
    pub action: usize,
    pub value: f32,
    pub is_chance: bool,
    pub chance_prob: f32,
    pub is_outcome: bool,
    pub depth: i32,
    pub flag: String,
    pub mark: Option<String>,
    pub child_count: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct ExNodeBoard {
    pub state: crate::app::view::GameStateView,
    pub move_coords: Vec<usize>,
}
#[derive(Debug, Clone)]
pub struct ExpectimaxTree {
    pub tree: ExTree,
    pub chosen_action: usize,
    pub stats: NnueSearchStats,

    pub root_value: f32,
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
pub fn ex_root_info(t: &ExpectimaxTree) -> Option<ExRootInfo> {
    if t.tree.nodes.is_empty() {
        return None;
    }
    Some(ExRootInfo {
        root: node_summary(&t.tree, 0),
        chosen_action: t.chosen_action,
        stats: ExSearchStats::from(&t.stats),
    })
}
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
            child_count: tree
                .nodes
                .iter()
                .filter(|n| n.parent == Some(child_id))
                .count(),
        })
        .collect()
}
pub fn ex_node_summary(tree: &ExTree, node_id: usize) -> Option<ExNodeSummary> {
    tree.nodes.get(node_id).map(|_| node_summary(tree, node_id))
}
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
pub use banqi_core::core::expectimax::ex_tree_stats;
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
        assert!(
            (tt_edge.value - 0.3).abs() < 1e-6,
            "边值 = -child.value（父视角）"
        );
        assert_eq!(tt_edge.flag, "lower");
        assert_eq!(tt_edge.mark.as_deref(), Some("tt"));
        let outcome = ex_edge_list(&t, 2);
        assert_eq!(outcome.len(), 1);
        assert!(outcome[0].is_outcome);
        assert!((outcome[0].chance_prob - 0.25).abs() < 1e-6);
    }
    #[test]
    fn out_of_range_returns_none() {
        let t = sample_tree();
        assert!(ex_node_summary(&t, 99).is_none());
        assert!(
            ex_node_board(&t, 0).is_none(),
            "env 未保存时棋盘查询为 None"
        );
    }
    #[test]
    fn tree_stats_counts_depth() {
        let t = sample_tree();
        let (nodes, max_depth) = ex_tree_stats(&t);
        assert_eq!(nodes, 4);
        assert_eq!(max_depth, 2);
    }
}
