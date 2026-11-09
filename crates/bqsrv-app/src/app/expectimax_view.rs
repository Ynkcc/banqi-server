use crate::app::common::player_label;
use crate::app::search::NnueSearchStats;
use banqi_core::core::env::{DarkChessEnv, SnapshotEnv};
use banqi_core::core::expectimax::{BoundFlag, SearchTree, StNode, NULL_ACTION, ROOT_ACTION};
pub use bqsrv_dto::{ExNodeSummary, ExRootInfo, ExSearchStats};
use serde::Serialize;
use ts_rs::TS;
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ExEdge {
    pub child_id: usize,
    pub action: usize,
    pub value: f32,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exp_value: Option<f32>,
    pub is_chance: bool,
    pub chance_prob: f32,
    pub is_outcome: bool,

    pub is_flip: bool,
    pub depth: i32,
    pub flag: String,
    pub mark: Option<String>,

    pub pruned: bool,

    pub prune_reason: Option<String>,
    pub child_count: usize,

    pub spent_nodes: u64,
}
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ExNodeBoard {
    pub state: crate::app::view::GameStateView,
    pub move_coords: Vec<usize>,

    pub chance_outcomes: Vec<ExChanceOutcome>,
}
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ExChanceOutcome {
    pub child_id: usize,
    pub prob: f32,

    pub value: f32,

    pub revealed: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ExpectimaxTree {
    pub tree: SearchTree,
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

fn mark_label(n: &StNode) -> Option<String> {
    (n.tt_cutoffs > 0).then(|| "tt".to_string())
}

fn prune_reason_label(n: &StNode) -> Option<String> {
    n.pruned
        .then(|| n.prune_reason.to_string())
        .filter(|s| !s.is_empty())
}

fn exp_of(v: f32) -> Option<f32> {
    v.is_finite().then_some(v)
}

fn root_env(tree: &SearchTree) -> Option<&DarkChessEnv> {
    tree.root.and_then(|r| tree.nodes[r].env.as_deref())
}
fn variant_of(tree: &SearchTree) -> Option<banqi_core::core::env::Variant> {
    root_env(tree).map(|e| e.to_snapshot().variant)
}

fn materialized_env(tree: &SearchTree, id: usize) -> Option<DarkChessEnv> {
    tree.materialize(id)
}
fn node_player(tree: &SearchTree, id: usize) -> String {
    materialized_env(tree, id)
        .map(|e| player_label(e.get_current_player()))
        .unwrap_or_else(|| "Unknown".into())
}

fn chance_node_flag(tree: &SearchTree, id: usize) -> Option<BoundFlag> {
    let mut flags = tree.nodes[id].children.iter().map(|&c| tree.nodes[c].bound);
    let first = flags.next()?;
    Some(if flags.all(|f| f == first) {
        first
    } else {
        BoundFlag::Unknown
    })
}

fn display_flag(tree: &SearchTree, n: &StNode, id: usize) -> BoundFlag {
    if n.is_chance && !n.is_outcome {
        chance_node_flag(tree, id).unwrap_or(n.bound)
    } else {
        n.bound
    }
}
fn node_summary(tree: &SearchTree, id: usize) -> ExNodeSummary {
    let n = &tree.nodes[id];
    let variant = variant_of(tree);
    let is_flip = variant
        .map(|v| crate::app::envops::is_flip_action(v, n.action))
        .unwrap_or(false);
    ExNodeSummary {
        id,
        action: n.action,
        value: n.value(),
        exp_value: exp_of(n.exp_value),
        exp_lower: exp_of(n.exp_lower),
        exp_upper: exp_of(n.exp_upper),
        depth: n.depth_first_seen,
        flag: flag_label(display_flag(tree, n, id)),
        mark: mark_label(n),
        pruned: n.pruned,
        prune_reason: prune_reason_label(n),
        is_chance: n.is_chance,
        is_outcome: n.is_outcome,
        is_flip,
        chance_prob: n.chance_prob,
        is_root: n.action == ROOT_ACTION,
        is_null_probe: n.action == NULL_ACTION,
        player: node_player(tree, id),
        child_count: n.children.len(),
        spent_nodes: n.spent_total(),
    }
}
pub fn ex_root_info(t: &ExpectimaxTree) -> Option<ExRootInfo> {
    if t.tree.nodes.is_empty() {
        return None;
    }
    let root = t.tree.root?;
    Some(ExRootInfo {
        root: node_summary(&t.tree, root),
        chosen_action: t.chosen_action,
        stats: ExSearchStats::from(&t.stats),
    })
}
pub fn ex_edge_list(tree: &SearchTree, node_id: usize) -> Vec<ExEdge> {
    let variant = variant_of(tree);
    tree.nodes[node_id]
        .children
        .iter()
        .map(|&child_id| {
            let c = &tree.nodes[child_id];
            ExEdge {
                child_id,
                action: c.action,
                value: -c.value(),
                exp_value: exp_of(c.exp_value),
                is_chance: c.is_chance,
                chance_prob: c.chance_prob,
                is_outcome: c.is_outcome,
                is_flip: variant
                    .map(|v| crate::app::envops::is_flip_action(v, c.action))
                    .unwrap_or(false),
                depth: c.depth_first_seen,
                flag: flag_label(display_flag(tree, c, child_id)),
                mark: mark_label(c),
                pruned: c.pruned,
                prune_reason: prune_reason_label(c),
                child_count: c.children.len(),
                spent_nodes: c.spent_total(),
            }
        })
        .collect()
}
pub fn ex_node_summary(tree: &SearchTree, node_id: usize) -> Option<ExNodeSummary> {
    tree.nodes.get(node_id).map(|_| node_summary(tree, node_id))
}
pub fn ex_node_board(tree: &SearchTree, node_id: usize) -> Option<ExNodeBoard> {
    let n = tree.nodes.get(node_id)?;
    let env = materialized_env(tree, node_id)?;
    let snap = env.to_snapshot();

    let coord_action = if n.is_outcome {
        n.parent.and_then(|p| tree.nodes.get(p)).map(|p| p.action)
    } else if n.is_chance {
        (n.action != ROOT_ACTION && n.action != NULL_ACTION).then_some(n.action)
    } else {
        (snap.last_action >= 0).then_some(snap.last_action as usize)
    };
    let variant = snap.variant;
    let move_coords = coord_action
        .and_then(|a| crate::app::envops::coords_for_action(variant, a))
        .unwrap_or_default();
    let state = crate::app::view::game_state_view(&snap);
    let chance_outcomes = if n.is_chance {
        chance_outcomes(tree, node_id)
    } else {
        Vec::new()
    };
    Some(ExNodeBoard {
        state,
        move_coords,
        chance_outcomes,
    })
}

fn chance_outcomes(tree: &SearchTree, node_id: usize) -> Vec<ExChanceOutcome> {
    tree.nodes[node_id]
        .children
        .iter()
        .map(|&child_id| {
            let c = &tree.nodes[child_id];
            ExChanceOutcome {
                child_id,
                prob: c.chance_prob,
                value: -c.value(),
                revealed: c.revealed.map(|p| p.short_name().to_string()),
            }
        })
        .collect()
}

pub fn ex_tree_stats(tree: &SearchTree) -> (usize, i32) {
    let mut max_depth = 0;
    for n in &tree.nodes {
        let mut d = 0;
        let mut cur = n.parent;
        while let Some(p) = cur {
            d += 1;
            cur = tree.nodes[p].parent;
        }
        max_depth = max_depth.max(d);
    }
    (tree.nodes.len(), max_depth)
}
#[cfg(test)]
mod tests {
    use super::*;
    use banqi_core::core::expectimax::tree::StKind;
    use banqi_core::core::expectimax::zobrist;

    fn sample_tree() -> SearchTree {
        let mut t = SearchTree::default();
        let mut n0 = sample_node(None);
        n0.action = ROOT_ACTION;
        n0.value_q = zobrist::quant(0.3);
        n0.complete = true;
        t.nodes.push(n0);
        let mut n1 = sample_node(Some(0));
        n1.bound = BoundFlag::Lower;
        n1.tt_cutoffs = 2;
        n1.value_q = zobrist::quant(-0.3);
        n1.complete = true;
        t.nodes.push(n1);
        t.nodes[0].children.push(1);
        let mut n2 = sample_node(Some(0));
        n2.is_chance = true;
        n2.complete = true;
        t.nodes.push(n2);
        t.nodes[0].children.push(2);
        let mut n3 = sample_node(Some(2));
        n3.is_outcome = true;
        n3.chance_prob = 0.25;
        n3.value_q = zobrist::quant(0.1);
        n3.bound = BoundFlag::Upper;
        n3.complete = true;
        t.nodes.push(n3);
        t.nodes[2].children.push(3);
        let mut n4 = sample_node(Some(2));
        n4.is_outcome = true;
        n4.chance_prob = 0.5;
        n4.value_q = zobrist::quant(-0.2);
        n4.bound = BoundFlag::Upper;
        n4.complete = true;
        t.nodes.push(n4);
        t.nodes[2].children.push(4);
        let mut n5 = sample_node(Some(0));
        n5.pruned = true;
        n5.prune_reason = "beta_cut";
        t.nodes.push(n5);
        t.nodes[0].children.push(5);
        t.root = Some(0);
        t
    }
    fn sample_node(parent: Option<usize>) -> StNode {
        let id = 0;
        let mut n = StNode::new(id, 0x1234, parent, 7, StKind::Normal, 4);
        n.id = id;
        n
    }
    #[test]
    fn edge_values_are_parent_perspective() {
        let t = sample_tree();
        let edges = ex_edge_list(&t, 0);
        assert_eq!(edges.len(), 3, "节点 0 的子边 = n1 / n2 / n5");
        let tt_edge = edges.iter().find(|e| e.child_id == 1).unwrap();
        assert!(
            (tt_edge.value - 0.3).abs() < 1e-6,
            "边值 = -child.value（父视角）"
        );
        assert_eq!(tt_edge.flag, "lower");
        assert_eq!(tt_edge.mark.as_deref(), Some("tt"));
        let pruned_edge = edges.iter().find(|e| e.child_id == 5).unwrap();
        assert!(pruned_edge.pruned, "剪枝边应带 pruned 标记");
        assert_eq!(pruned_edge.prune_reason.as_deref(), Some("beta_cut"));
        assert_eq!(pruned_edge.child_count, 0);
        let outcome = ex_edge_list(&t, 2);
        assert_eq!(outcome.len(), 2);
        assert!(outcome.iter().all(|e| e.is_outcome));
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
    fn chance_node_flag_aggregates_outcome_flags() {
        let mut t = sample_tree();

        assert_eq!(node_summary(&t, 2).flag, "upper");
        assert_eq!(
            ex_edge_list(&t, 0)
                .iter()
                .find(|e| e.child_id == 2)
                .unwrap()
                .flag,
            "upper"
        );

        t.nodes[4].bound = BoundFlag::Lower;
        assert_eq!(node_summary(&t, 2).flag, "unknown");

        assert_eq!(node_summary(&t, 1).flag, "lower");
    }
    #[test]
    fn pruned_summary_carries_reason() {
        let t = sample_tree();
        let s = node_summary(&t, 5);
        assert!(s.pruned);
        assert_eq!(s.prune_reason.as_deref(), Some("beta_cut"));
        assert_eq!(s.child_count, 0);
        assert_eq!(s.mark, None, "剪枝节点无 TT 截断记录");
        let s1 = node_summary(&t, 1);
        assert!(!s1.pruned);
        assert_eq!(s1.prune_reason, None);
    }
    #[test]
    fn tree_stats_counts_depth() {
        let t = sample_tree();
        let (nodes, max_depth) = ex_tree_stats(&t);
        assert_eq!(nodes, 6);
        assert_eq!(max_depth, 2);
    }
}
