//! MCTS 搜索树可视化（共享）：DTO、只读查询、统计与变体擦除。
//!
//! 由 banqi-server/src/mcts.rs 与 banqi-gui/src/mcts_view.rs 收敛而来：
//! - DTO 序列化为 snake_case，两侧前端契约一致；
//! - 树以泛型 [`MctsTree`]（`G: GameEnv`）承载，经 [`MctsTreeView`] trait object
//!   （[`MctsTreeBox`]）挂在会话上，变体无关；
//! - gui 的节点检视返回完整 `GameState`（含位板 / 动作掩码等），由 gui 侧
//!   自行组装；此处 [`MctsNodeBoard`] 为 server 契约的精简局面视图。
//!
//! 生命周期约定：每次搜索替换整棵树；查询均为只读投影。

use banqi_core::core::env::actions::action_lookup_tables;
use banqi_core::core::env::{GameEnv, SnapshotEnv};
use banqi_core::core::mcts::MctsArena;
use serde::Serialize;

use crate::app::common::player_label;

// 搜索结果树（真源在 banqi-engine::engine::gumbel，server 仅 re-export 保持路径稳定）
pub use banqi_engine::engine::gumbel::{MctsSearchStats, MctsTree, tree_stats};

// DTO（字段与两侧前端 types.ts 一一对应，snake_case）

/// 统一的"边"摘要：普通节点取 children，机会节点取 possible_states。
#[derive(Debug, Clone, Serialize)]
pub struct MctsEdge {
    pub child_id: usize,
    /// 普通边 = 动作索引；机会边 = outcome_id
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
    /// "Red" / "Black"
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

/// 搜索树某节点的完整局面 + 到达着法所在格。
///
/// `state` 为前端契约的完整 `GameState` 形状（[`crate::app::view::GameStateView`]），
/// 与状态端点零差异直传；暗子不泄露身份（快照即信息状态视角）。
#[derive(Debug, Clone, Serialize)]
pub struct MctsNodeBoard {
    pub state: crate::app::view::GameStateView,
    /// 翻棋/吃暗子 1 格，移动/炮击 2 格（from, to）；无则为空。
    pub move_coords: Vec<usize>,
}

// 泛型树查询

/// 收集某节点的全部子边（普通 + 机会），按 N 降序、再按先验降序。
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
        b.n.cmp(&a.n)
            .then(b.prior.partial_cmp(&a.prior).unwrap_or(std::cmp::Ordering::Equal))
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

// 变体擦除：trait object 挂到会话

pub type MctsTreeBox = Box<dyn MctsTreeView>;

pub trait MctsTreeView: Send + Sync {
    fn root_info(&self) -> MctsRootInfo;
    fn children(&self, node_id: usize) -> Vec<MctsEdge>;
    fn detail(&self, node_id: usize) -> Option<MctsNodeDetail>;
    fn board(&self, node_id: usize) -> Option<MctsNodeBoard>;
    /// 变体特化下沉：供树复用从 `MctsTreeBox` 还原具体
    /// `MctsTree<DarkChessEnv>`（`tree_reuse_plan` 依赖具体环境类型重放）。
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
        // 机会节点保存的是「翻棋前」局面，其 last_action 仍指向父节点的着法；
        // 故改从任一机会结果子节点取回真正的翻棋动作。
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
            .and_then(|a| {
                action_lookup_tables(&snap.variant.config())
                    .action_to_coords
                    .get(a)
                    .cloned()
            })
            .unwrap_or_default();
        let state = crate::app::view::game_state_view(&snap);
        Some(MctsNodeBoard { state, move_coords })
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        Box::new(*self)
    }
}
