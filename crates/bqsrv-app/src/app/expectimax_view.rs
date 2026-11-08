use crate::app::common::player_label;
use crate::app::search::NnueSearchStats;
use banqi_core::core::env::{DarkChessEnv, SnapshotEnv};
use banqi_core::core::expectimax::{BoundFlag, ExNode, ExTree};
pub use bqsrv_dto::{ExNodeSummary, ExRootInfo, ExSearchStats};
use serde::Serialize;
use ts_rs::TS;
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ExEdge {
    pub child_id: usize,
    pub action: usize,
    pub value: f32,
    pub is_chance: bool,
    pub chance_prob: f32,
    pub is_outcome: bool,

    pub is_flip: bool,
    pub depth: i32,
    pub flag: String,
    pub mark: Option<String>,
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
    let is_flip = n
        .env
        .as_ref()
        .map(|e| crate::app::envops::is_flip_action(e.to_snapshot().variant, n.action))
        .unwrap_or(false);
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
        is_flip,
        chance_prob: n.chance_prob,
        is_root: n.action == banqi_core::core::expectimax::tree::ROOT_ACTION,
        is_null_probe: n.action == banqi_core::core::expectimax::tree::NULL_ACTION,
        player: node_player(n),
        child_count: tree.nodes.iter().filter(|c| c.parent == Some(id)).count(),
        spent_nodes: n.spent_nodes,
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
    let variant = tree
        .nodes
        .get(node_id)
        .and_then(|n| n.env.as_ref())
        .or_else(|| tree.nodes.iter().find_map(|n| n.env.as_ref()))
        .map(|e| e.to_snapshot().variant);
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
            is_flip: variant
                .map(|v| crate::app::envops::is_flip_action(v, c.action))
                .unwrap_or(false),
            depth: c.depth,
            flag: flag_label(c.flag),
            mark: mark_label(c.mark),
            child_count: tree
                .nodes
                .iter()
                .filter(|n| n.parent == Some(child_id))
                .count(),
            spent_nodes: c.spent_nodes,
        })
        .collect()
}
pub fn ex_node_summary(tree: &ExTree, node_id: usize) -> Option<ExNodeSummary> {
    tree.nodes.get(node_id).map(|_| node_summary(tree, node_id))
}
pub fn ex_node_board(tree: &ExTree, node_id: usize) -> Option<ExNodeBoard> {
    let n = tree.nodes.get(node_id)?;
    let env: &DarkChessEnv = n.env.as_ref()?;
    let snap = env.to_snapshot();

    let move_coords = if n.is_chance {
        (n.action != banqi_core::core::expectimax::tree::ROOT_ACTION
            && n.action != banqi_core::core::expectimax::tree::NULL_ACTION)
            .then(|| crate::app::envops::coords_for_action(snap.variant, n.action))
            .flatten()
            .unwrap_or_default()
    } else {
        (snap.last_action >= 0)
            .then_some(snap.last_action as usize)
            .and_then(|a| crate::app::envops::coords_for_action(snap.variant, a))
            .unwrap_or_default()
    };
    let state = crate::app::view::game_state_view(&snap);
    let chance_outcomes = if n.is_chance {
        chance_outcomes(tree, node_id, &state.board, &move_coords)
    } else {
        Vec::new()
    };
    Some(ExNodeBoard {
        state,
        move_coords,
        chance_outcomes,
    })
}

fn chance_outcomes(
    tree: &ExTree,
    node_id: usize,
    board: &[String],
    coords: &[usize],
) -> Vec<ExChanceOutcome> {
    tree.nodes
        .iter()
        .enumerate()
        .filter(|(_, c)| c.parent == Some(node_id))
        .map(|(child_id, c)| {
            let revealed = c.env.as_ref().and_then(|cenv| {
                let csnap = cenv.to_snapshot();
                let cboard = crate::app::view::game_state_view(&csnap).board;
                coords.iter().rev().find_map(|sq| {
                    let old = board.get(*sq)?;
                    let new = cboard.get(*sq)?;
                    (old != new && new != "Hidden").then(|| new.clone())
                })
            });
            ExChanceOutcome {
                child_id,
                prob: c.chance_prob,
                value: -c.value,
                revealed,
            }
        })
        .collect()
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
            spent_nodes: 0,
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
