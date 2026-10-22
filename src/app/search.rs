//! 搜索执行（共享）：Gumbel MCTS（含树复用）与 NNUE Expectimax。
//!
//! 由 banqi-gui/src/mcts_view.rs（torch/onnx 搜索 + 树复用）与
//! banqi-server/src/ai.rs（onnx MCTS / NNUE Expectimax）的搜索核心收敛而来：
//! - 全部为同步（blocking）函数，异步包装（`spawn_blocking`）由调用方实现；
//! - [`run_gumbel`] 为泛型核心：「建树（或按复用计划推进根）+ run + 统计」，
//!   torch / onnx 分支仅在评估器构造处不同，由特征门控的薄包装调用同一核心；
//! - 树复用（[`tree_reuse_plan`]，沿「AI 上手 → 对手上手」推进两步 + 特征比对）
//!   依赖 `DarkChessEnv` 的固有方法（resnet 特征 / last_revealed_piece 等），
//!   保留具体类型签名。

use std::sync::Arc;
use std::time::Instant;

use banqi_core::core::env::{DarkChessEnv, GameEnv, SnapshotEnv};
use banqi_core::core::expectimax::{SearchConfig, SearchResult};
use banqi_core::core::mcts::{Evaluator, GumbelConfig, GumbelMCTS, MctsArena};
use banqi_engine::nnue::NnueEvaluator;

use crate::app::mcts_view::{MctsSearchStats, MctsTree};

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

/// 树复用计划：上一棵树沿「AI 上手 → 对手上手」推进两步后可续用
/// （root 下移，保留已积累的访问 / Q 统计）。
pub struct ReusePlan<G: GameEnv> {
    pub arena: MctsArena<G>,
    pub root_idx: usize,
    /// AI 上手后的中间局面（root 推进的第 1 步）
    pub ai_env: G,
    pub ai_action: usize,
    pub opp_action: usize,
}

/// 泛型核心：建树（或按复用计划推进根）+ run + 统计。
///
/// `reuse` 为 `Some` 时尝试树复用（对不上时由调用方回退 `None` 全新建树）。
pub fn run_gumbel<G, E>(
    env: &G,
    evaluator: &E,
    config: GumbelConfig,
    reuse: Option<ReusePlan<G>>,
) -> Result<MctsTree<G>, String>
where
    G: GameEnv + Copy,
    E: Evaluator<G>,
{
    let reused = reuse.is_some();
    let budget = config.num_simulations.max(1);
    let mut mcts = match reuse {
        Some(p) => {
            let mut m = GumbelMCTS::from_arena(env, evaluator, config, p.arena, p.root_idx);
            m.step_next(&p.ai_env, p.ai_action);
            m.step_next(env, p.opp_action);
            m
        }
        None => GumbelMCTS::new(env, evaluator, config),
    };
    let pre_visits = mcts.arena.get(mcts.root_idx).visit_count;
    let started = Instant::now();
    let result = mcts
        .run()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "MCTS 搜索无合法动作".to_string())?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let (max_depth, nodes) = crate::app::mcts_view::tree_stats(&mcts.arena, mcts.root_idx);
    Ok(MctsTree {
        arena: mcts.arena,
        root_idx: mcts.root_idx,
        chosen_action: result.action,
        stats: MctsSearchStats {
            reused,
            budget,
            actual_sims: result.root_visit_count.saturating_sub(pre_visits),
            max_depth,
            nodes,
            elapsed_ms,
        },
    })
}

/// 比较两个局面是否一致（按 ResNet 观测特征逐位比较）。
fn env_state_eq<G: GameEnv>(a: &G, b: &G) -> bool {
    let (a_obs, b_obs) = (a.get_resnet_state(), b.get_resnet_state());
    a_obs.board.shape() == b_obs.board.shape()
        && a_obs.board.iter().zip(b_obs.board.iter()).all(|(x, y)| x == y)
        && a_obs.scalars == b_obs.scalars
}

/// 计算搜索树复用计划：把上一棵树的根沿「AI 上手 → 对手上手」推进两步。
/// 机会动作用 `chance_outcomes` 枚举全部候选结果 + 特征校验（确定性重放）；
/// 任一环节对不上（树过期、变体切换等）返回 None，调用方回退全新建树。
pub fn tree_reuse_plan<G>(prev: MctsTree<G>, cur: &G) -> Option<ReusePlan<G>>
where
    G: GameEnv + Copy + SnapshotEnv,
{
    let env0 = prev.arena.get(prev.root_idx).env?;
    let opp_action: usize = {
        let snap = cur.to_snapshot();
        usize::try_from(snap.last_action).ok()?
    };

    /// 单步推进的候选局面：机会动作枚举全部结果，普通动作唯一
    /// （非法动作 / 步进失败时返回 None，整体回退全新建树）。
    fn advance_candidates<G: GameEnv + Copy>(env: G, action: usize) -> Option<Vec<G>> {
        if env.is_chance_action(action) {
            Some(
                env.chance_outcomes(action)
                    .into_iter()
                    .map(|(_, _, e)| e)
                    .collect(),
            )
        } else {
            let mut e = env;
            e.step(action).ok()?;
            Some(vec![e])
        }
    }

    // 第 1 步：AI 上手后的局面
    for env1 in advance_candidates(env0, prev.chosen_action)? {
        // 第 2 步：对手上手后的局面（翻棋/吃暗子按全部翻子结果枚举校验）
        for check in advance_candidates(env1, opp_action)? {
            if env_state_eq(&check, cur) {
                return Some(ReusePlan {
                    arena: prev.arena,
                    root_idx: prev.root_idx,
                    ai_env: env1,
                    ai_action: prev.chosen_action,
                    opp_action,
                });
            }
        }
    }
    None
}

/// Gumbel MCTS + TorchScript（需 torch feature）：含树复用。
/// 泛型支持全部变体环境（调用方经 `with_env!` 以具体环境类型实例化）。
#[cfg(feature = "torch")]
pub fn mcts_search_torch<G>(
    env: &G,
    sims: usize,
    model: Option<Arc<banqi_engine::engine::ModelWrapper>>,
    prev_tree: Option<MctsTree<G>>,
) -> Result<MctsTree<G>, String>
where
    G: GameEnv + Copy + SnapshotEnv + 'static,
{
    let model = model.ok_or("未加载 .pt 模型，无法执行 MCTS+DL 搜索")?;
    let reuse = prev_tree.and_then(|t| tree_reuse_plan(t, env));
    let evaluator = banqi_engine::engine::TchEvaluator::<G>::new(model);
    run_gumbel(env, &evaluator, GumbelConfig::for_play(sims), reuse)
}

/// Gumbel MCTS + ONNX（需 onnx feature）：含树复用。
/// 泛型支持全部变体环境（调用方经 `with_env!` 以具体环境类型实例化）。
#[cfg(feature = "onnx")]
pub fn mcts_search_onnx<G>(
    env: &G,
    sims: usize,
    model: Option<Arc<banqi_engine::inference::onnx::OnnxModel>>,
    prev_tree: Option<MctsTree<G>>,
) -> Result<MctsTree<G>, String>
where
    G: GameEnv + Copy + SnapshotEnv + 'static,
{
    let model = model.ok_or("未加载 .onnx 模型，无法执行 MCTS+ONNX 搜索")?;
    let reuse = prev_tree.and_then(|t| tree_reuse_plan(t, env));
    let evaluator = banqi_engine::inference::onnx::OnnxEvaluator::<G>::new(model);
    run_gumbel(env, &evaluator, GumbelConfig::for_play(sims), reuse)
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

/// Expectimax 搜索一次（NNUE 叶评估；同步；调用方自行 spawn_blocking）。
/// 搜索入口校验 `.nnue` 头部布局（跨变体权重硬失败）；
/// 预算约束由 [`SearchBudget`] 单点换算。返回 `(选择的动作, 搜索统计)`。
pub fn nnue_search(
    env: &DarkChessEnv,
    evaluator: Arc<NnueEvaluator>,
    budget: SearchBudget,
) -> Result<(usize, NnueSearchStats), String> {
    let budget = budget.clamped();
    evaluator
        .validate_layout(&env.config)
        .map_err(|e| format!(".nnue 权重与当前变体布局不匹配: {e}"))?;
    let mut cfg = budget.to_search_config();
    cfg.nnue_evaluator = Some(evaluator);
    let started = Instant::now();
    let SearchResult { action, depth: reached_depth, nodes, .. } = banqi_core::core::expectimax::search(env, &cfg)
        .ok_or_else(|| "Expectimax 未返回动作（无合法动作）".to_string())?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    Ok((
        action,
        NnueSearchStats {
            mode: budget.mode.as_str(),
            depth: if budget.mode == BudgetMode::DepthNodes { cfg.max_depth } else { 0 },
            budget: if budget.mode != BudgetMode::Time { cfg.node_budget } else { 0 },
            time_limit_ms: cfg.time_limit_ms,
            nodes,
            reached_depth,
            elapsed_ms,
        },
    ))
}

/// Expectimax + NNUE 搜索一次并捕获搜索轨迹树（可视化；同步）。
/// 在 [`nnue_search`] 基础上追加可视化约束：`threads = 1`（多线程搜索碎片
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
            stats: NnueSearchStats {
                mode: budget.mode.as_str(),
                depth: if budget.mode == BudgetMode::DepthNodes { cfg.max_depth } else { 0 },
                budget: if budget.mode != BudgetMode::Time { cfg.node_budget } else { 0 },
                time_limit_ms: cfg.time_limit_ms,
                nodes,
                reached_depth,
                elapsed_ms,
            },
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
