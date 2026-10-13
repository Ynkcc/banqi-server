//! MCTS 搜索树可视化：AI 决策后保留 `MctsArena` 供前端懒加载查询。
//!
//! 与 banqi-gui 的 `mcts_view.rs` 同构（DTO 字段一一对应，前端 types.ts 为契约真源）：
//! - 树以 trait object 形式挂在 `GameSession` 上（变体无关）；
//! - DTO 序列化为 snake_case，与 banqi-core serde 风格一致，client 适配层直传。
//!
//! 生命周期：每次 AI 回合（或手动搜索）替换整棵树；不做跨回合复用
//! （GUI 的 tree_reuse_plan 依赖具体变体的环境重放，v1 暂不迁移，见 backend-api.md）。

use banqi_core::core::env::{GameEnv, SnapshotEnv};
use banqi_core::core::mcts::MctsArena;
use serde::Serialize;

use crate::variant;

// ============================================================================
// DTO（字段与前端 types.ts 一一对应）
// ============================================================================

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
pub struct MctsSearchStats {
    pub reused: bool,
    pub budget: usize,
    pub actual_sims: u32,
    pub max_depth: usize,
    pub nodes: usize,
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

/// 节点局面视图（snake_case，前端 `GameState` 的信息子集；暗子不泄露身份）。
#[derive(Debug, Clone, Serialize)]
pub struct MctsBoardState {
    /// banqi-core `Slot` 转字符串后的棋盘（"Empty" / "Hidden" / "R_Sol" 等），
    /// 与 Tauri 后端 extract_game_state 直出的 GameState.board 同构
    /// （Slot 直接 serde 序列化是 tagged 枚举，前端无法识别，故必须转字符串）。
    pub board: Vec<String>,
    /// "Red" / "Black"
    pub current_player: String,
    pub move_counter: u32,
    pub total_step_counter: u32,
    pub hp_red: i32,
    pub hp_black: i32,
    pub variant: String,
}

/// 搜索树某节点的完整局面 + 到达着法所在格。
#[derive(Debug, Clone, Serialize)]
pub struct MctsNodeBoard {
    pub state: MctsBoardState,
    /// 翻棋/吃暗子 1 格，移动/炮击 2 格（from, to）；无则为空。
    pub move_coords: Vec<usize>,
}

fn player_label(p: banqi_core::core::env::types::Player) -> String {
    use banqi_core::core::env::types::Player::*;
    match p {
        Red => "Red".to_string(),
        Black => "Black".to_string(),
    }
}

// ============================================================================
// 泛型树查询（对齐 banqi-gui/src/mcts_view.rs）
// ============================================================================

/// 一次搜索保留的树（具体变体的环境类型实例化）。
pub struct MctsTree<G: GameEnv> {
    pub arena: MctsArena<G>,
    pub root_idx: usize,
    pub chosen_action: usize,
    pub stats: MctsSearchStats,
}

/// 收集某节点的全部子边（普通 + 机会），按 N 降序、再按先验降序。
fn mcts_edge_list<G: GameEnv>(arena: &MctsArena<G>, node_id: usize) -> Vec<MctsEdge> {
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

fn mcts_node_summary<G: GameEnv>(arena: &MctsArena<G>, node_id: usize) -> MctsNodeSummary {
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

/// 统计搜索树的最大深度与节点总数（BFS，防环）。
pub fn tree_stats<G: GameEnv>(arena: &MctsArena<G>, root_idx: usize) -> (usize, usize) {
    let mut max_depth = 0usize;
    let mut nodes = 0usize;
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![(root_idx, 0usize)];
    while let Some((idx, depth)) = stack.pop() {
        if !visited.insert(idx) {
            continue;
        }
        nodes += 1;
        max_depth = max_depth.max(depth);
        let node = arena.get(idx);
        for (_, child) in &node.children {
            stack.push((*child, depth + 1));
        }
        for (_, _, child) in &node.possible_states {
            stack.push((*child, depth + 1));
        }
    }
    (max_depth, nodes)
}

// ============================================================================
// 变体擦除：trait object 挂到 GameSession
// ============================================================================

pub type MctsTreeBox = Box<dyn MctsTreeView>;

pub trait MctsTreeView: Send + Sync {
    fn root_info(&self) -> MctsRootInfo;
    fn children(&self, node_id: usize) -> Vec<MctsEdge>;
    fn detail(&self, node_id: usize) -> Option<MctsNodeDetail>;
    fn board(&self, node_id: usize) -> Option<MctsNodeBoard>;
}

impl<G: GameEnv + SnapshotEnv> MctsTreeView for MctsTree<G> {
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
            .and_then(|a| variant::coords_for_action(snap.variant, a))
            .unwrap_or_default();
        let state = MctsBoardState {
            board: snap
                .slots
                .iter()
                .map(|slot| match slot {
                    banqi_core::core::env::types::Slot::Empty => "Empty".to_string(),
                    banqi_core::core::env::types::Slot::Hidden => "Hidden".to_string(),
                    banqi_core::core::env::types::Slot::Revealed(p) => p.short_name(),
                })
                .collect(),
            current_player: player_label(snap.current_player),
            move_counter: snap.move_counter,
            total_step_counter: snap.total_step_counter,
            hp_red: snap.scores[0],
            hp_black: snap.scores[1],
            variant: snap.variant.as_str().to_string(),
        };
        Some(MctsNodeBoard { state, move_coords })
    }
}
