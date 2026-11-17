use banqi_core::core::env::DarkChessEnv;
use banqi_core::core::expectimax::nnue::NnueEvaluate;
use banqi_core::core::expectimax::{SearchConfig, SearchResult};
use banqi_engine::nnue::QNnueEvaluator;
pub use bqsrv_config::{ExpectimaxRun, RootTuning, TimeBudgetMode};
pub use bqsrv_dto::NnueSearchStats;
use std::sync::Arc;
use std::time::Instant;
fn nnue_stats_from(
    engine: &'static str,
    run: &ExpectimaxRun,
    cfg: &SearchConfig,
    nodes: u64,
    reached_depth: i32,
    elapsed_ms: u64,
) -> NnueSearchStats {
    NnueSearchStats {
        engine,
        mode: run.time.mode.as_str(),
        depth: if run.time.mode == TimeBudgetMode::Depth {
            cfg.budget.deterministic_depth
        } else {
            0
        },
        time_limit_ms: cfg.budget.time_limit_ms,
        nodes,
        reached_depth,
        elapsed_ms,
    }
}

fn build_search_config(run: &ExpectimaxRun, roots: RootTuning) -> SearchConfig {
    let mut cfg = SearchConfig::from(&run.schema);
    cfg.budget.time_limit_ms = run.time.time_limit_ms();
    cfg.root.aspiration = roots.aspiration;
    cfg.root.chance_exp = roots.chance_exp;
    cfg
}

pub fn expectimax_search_tree(
    env: &DarkChessEnv,
    evaluator: Arc<dyn NnueEvaluate>,
    run: ExpectimaxRun,
    roots: RootTuning,
    engine_label: &'static str,
) -> Result<(usize, crate::app::expectimax_view::ExpectimaxTree), String> {
    let run = run.clamped();
    let mut cfg = build_search_config(&run, roots);
    cfg.res.nnue_evaluator = Some(evaluator);
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
        stats: nnue_stats_from(engine_label, &run, &cfg, nodes, reached_depth, elapsed_ms),
        root_value: value,
    };
    Ok((action, kept))
}

pub fn nnue_search_tree(
    env: &DarkChessEnv,
    evaluator: Arc<QNnueEvaluator>,
    run: ExpectimaxRun,
    roots: RootTuning,
) -> Result<(usize, crate::app::expectimax_view::ExpectimaxTree), String> {
    evaluator
        .validate_layout(&env.config)
        .map_err(|e| format!(".nnue 权重与当前变体布局不匹配: {e}"))?;
    expectimax_search_tree(env, evaluator, run, roots, "nnue")
}
