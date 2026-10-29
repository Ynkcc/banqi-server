use crate::app::common::player_label;
use banqi_core::core::env::{GameEnv, SnapshotEnv};
use banqi_core::core::mcts::MctsArena;
pub use banqi_engine::engine::gumbel::{MctsSearchStats, MctsTree};
use serde::Serialize;
#[derive(Debug, Clone, Serialize)]
pub struct MctsEdge {
    pub child_id: usize,
    pub action: usize,
    pub prior: f32,
    pub logit: f32,
    pub n: u32,
    pub q: f32,
    pub health_q: f32,
    pub is_chance: bool,
    pub chance_prob: f32,
}
#[derive(Debug, Clone, Serialize)]
pub struct MctsNodeSummary {
    pub id: usize,
    pub n: u32,
    pub q: f32,
    pub health_q: f32,
    pub player: String,
    pub is_chance: bool,
    pub is_terminal: bool,
    pub is_expanded: bool,
    pub edge_count: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct MctsRootInfo {
    pub root: MctsNodeSummary,
    pub chosen_action: usize,
    pub stats: MctsSearchStats,
}
#[derive(Debug, Clone, Serialize)]
pub struct MctsNodeDetail {
    pub id: usize,
    pub prior: f32,
    pub logit: f32,
    pub n: u32,
    pub q: f32,
    pub health_q: f32,
    pub initial_value: f32,
    pub player: String,
    pub is_chance: bool,
    pub is_terminal: bool,
    pub is_expanded: bool,
    pub child_count: usize,
    pub outcome_count: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct MctsNodeBoard {
    pub state: crate::app::view::GameStateView,
    pub move_coords: Vec<usize>,
}
pub fn mcts_edge_list<G: GameEnv>(arena: &MctsArena<G>, node_id: usize) -> Vec<MctsEdge> {
    let node = arena.get(node_id);
    let mut edges: Vec<MctsEdge> = node
        .children
        .iter()
        .map(|(action, idx)| {
            let c = arena.get(*idx);
            MctsEdge {
                child_id: *idx,
                action: *action,
                prior: c.prior,
                logit: c.logit,
                n: c.visit_count,
                q: c.q_value(),
                health_q: c.q_health_value(),
                is_chance: false,
                chance_prob: 1.0,
            }
        })
        .collect();
    edges.extend(node.possible_states.iter().map(|(outcome_id, prob, idx)| {
        let c = arena.get(*idx);
        MctsEdge {
            child_id: *idx,
            action: *outcome_id,
            prior: c.prior,
            logit: c.logit,
            n: c.visit_count,
            q: c.q_value(),
            health_q: c.q_health_value(),
            is_chance: true,
            chance_prob: *prob,
        }
    }));
    edges.sort_by(|a, b| {
        b.n.cmp(&a.n).then(
            b.prior
                .partial_cmp(&a.prior)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    edges
}
pub fn mcts_node_summary<G: GameEnv>(arena: &MctsArena<G>, node_id: usize) -> MctsNodeSummary {
    let node = arena.get(node_id);
    MctsNodeSummary {
        id: node_id,
        n: node.visit_count,
        q: node.q_value(),
        health_q: node.q_health_value(),
        player: player_label(node.player),
        is_chance: node.is_chance_node,
        is_terminal: node.is_terminal,
        is_expanded: node.is_expanded,
        edge_count: node.children.len() + node.possible_states.len(),
    }
}
pub type MctsTreeBox = Box<dyn MctsTreeView>;
pub fn mcts_stats_from_session(tree: Option<&MctsTreeBox>) -> Option<bqsrv_dto::MctsStatsDto> {
    Some(bqsrv_dto::MctsStatsDto::from(&tree?.root_info().stats))
}
pub trait MctsTreeView: Send + Sync {
    fn root_info(&self) -> MctsRootInfo;
    fn children(&self, node_id: usize) -> Vec<MctsEdge>;
    fn detail(&self, node_id: usize) -> Option<MctsNodeDetail>;
    fn board(&self, node_id: usize) -> Option<MctsNodeBoard>;
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any>;
}
impl<G: GameEnv + SnapshotEnv + 'static> MctsTreeView for MctsTree<G> {
    fn root_info(&self) -> MctsRootInfo {
        MctsRootInfo {
            root: mcts_node_summary(&self.arena, self.root_idx),
            chosen_action: self.chosen_action,
            stats: self.stats.clone(),
        }
    }
    fn children(&self, node_id: usize) -> Vec<MctsEdge> {
        mcts_edge_list(&self.arena, node_id)
    }
    fn detail(&self, node_id: usize) -> Option<MctsNodeDetail> {
        let node = self.arena.get(node_id);
        Some(MctsNodeDetail {
            id: node_id,
            prior: node.prior,
            logit: node.logit,
            n: node.visit_count,
            q: node.q_value(),
            health_q: node.q_health_value(),
            initial_value: node.initial_value,
            player: player_label(node.player),
            is_chance: node.is_chance_node,
            is_terminal: node.is_terminal,
            is_expanded: node.is_expanded,
            child_count: node.children.len(),
            outcome_count: node.possible_states.len(),
        })
    }
    fn board(&self, node_id: usize) -> Option<MctsNodeBoard> {
        let node = self.arena.get(node_id);
        let env = node.env.as_ref()?;
        let action = if node.is_chance_node {
            node.possible_states
                .first()
                .and_then(|(_, _, idx)| self.arena.get(*idx).env.as_ref())
                .map(SnapshotEnv::to_snapshot)
                .map(|s| s.last_action)
                .filter(|&a| a >= 0)
                .or_else(|| {
                    let s = env.to_snapshot();
                    (s.last_action >= 0).then_some(s.last_action)
                })
        } else {
            let s = env.to_snapshot();
            (s.last_action >= 0).then_some(s.last_action)
        };
        let action = action.map(|a| a as usize);
        let snap = env.to_snapshot();
        let move_coords = action
            .and_then(|a| crate::app::envops::coords_for_action(snap.variant, a))
            .unwrap_or_default();
        let state = crate::app::view::game_state_view(&snap);
        Some(MctsNodeBoard { state, move_coords })
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        Box::new(*self)
    }
}
