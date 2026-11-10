use banqi_core::core::env::DarkChessEnv;
use banqi_core::core::expectimax::nnue::NnueEvaluate;
use banqi_core::core::expectimax::{FEAT_ASP, SearchConfig, SearchResult};
use banqi_engine::nnue::NnueEvaluator;
use std::sync::Arc;
use std::time::Instant;
pub use bqsrv_config::{BudgetMode, SearchBudget};
pub use bqsrv_dto::NnueSearchStats;
fn nnue_stats_from(
    engine: &'static str,
    budget: &SearchBudget,
    cfg: &SearchConfig,
    nodes: u64,
    reached_depth: i32,
    elapsed_ms: u64,
) -> NnueSearchStats {
    NnueSearchStats {
        engine,
        mode: budget.mode.as_str(),
        depth: if budget.mode == BudgetMode::DepthNodes {
            cfg.budget.max_depth
        } else {
            0
        },
        budget: if budget.mode != BudgetMode::Time {
            cfg.budget.node_budget
        } else {
            0
        },
        time_limit_ms: cfg.budget.time_limit_ms,
        nodes,
        reached_depth,
        elapsed_ms,
    }
}

pub fn expectimax_search_tree(
    env: &DarkChessEnv,
    evaluator: Arc<dyn NnueEvaluate>,
    budget: SearchBudget,
    engine_label: &'static str,
) -> Result<(usize, crate::app::expectimax_view::ExpectimaxTree), String> {
    let budget = budget.clamped();
    let mut cfg = budget.to_search_config();
    cfg.res.nnue_evaluator = Some(evaluator);
    cfg.budget.threads = 1;
    cfg.features &= !FEAT_ASP;
    let started = Instant::now();
    let SearchResult {
        action,
        value,
        depth: reached_depth,
        nodes,
        tree,
        ..
    } = banqi_core::core::expectimax::search(env, &cfg)
        .ok_or_else(|| "Expectimax 未返回动作（无合法动作）".to_string())?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let kept = crate::app::expectimax_view::ExpectimaxTree {
        tree: tree.unwrap_or_default(),
        chosen_action: action,
        stats: nnue_stats_from(
            engine_label,
            &budget,
            &cfg,
            nodes,
            reached_depth,
            elapsed_ms,
        ),
        root_value: value,
    };
    Ok((action, kept))
}

pub fn nnue_search_tree(
    env: &DarkChessEnv,
    evaluator: Arc<NnueEvaluator>,
    budget: SearchBudget,
) -> Result<(usize, crate::app::expectimax_view::ExpectimaxTree), String> {
    evaluator
        .validate_layout(&env.config)
        .map_err(|e| format!(".nnue 权重与当前变体布局不匹配: {e}"))?;
    expectimax_search_tree(env, evaluator, budget, "nnue")
}
