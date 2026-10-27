use crate::app::mcts_view::{MctsTree, MctsTreeBox};
use crate::app::models::{EngineKind, ModelPool};
use crate::app::search::SearchBudget;
use crate::dto::{MctsStatsDto, NnueStatsDto};
use banqi_core::core::env::{DarkChessEnv, PositionSnapshot, SnapshotEnv};
use banqi_core::core::expectimax;
use banqi_core::core::expectimax::rule_eval::RuleMaterialEval;
use banqi_engine::engine::{CaptureFirstPolicy, Policy, RandomPolicy, RevealFirstPolicy};
use std::sync::Arc;
use uuid::Uuid;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Random,
    RevealFirst,
    CaptureFirst,
}
impl RuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleKind::Random => "Random",
            RuleKind::RevealFirst => "RevealFirst",
            RuleKind::CaptureFirst => "CaptureFirst",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        let norm = s.trim().to_ascii_lowercase().replace(['-', '_'], "");
        match norm.as_str() {
            "random" => Some(RuleKind::Random),
            "revealfirst" => Some(RuleKind::RevealFirst),
            "capturefirst" => Some(RuleKind::CaptureFirst),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyKind {
    Random,
    RevealFirst,
    CaptureFirst,
    Expectimax,
    ExpectimaxNnue,
    MctsOnnx,
    MctsDL,
}
impl StrategyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StrategyKind::Random => "Random",
            StrategyKind::RevealFirst => "RevealFirst",
            StrategyKind::CaptureFirst => "CaptureFirst",
            StrategyKind::Expectimax => "Expectimax",
            StrategyKind::ExpectimaxNnue => "ExpectimaxNnue",
            StrategyKind::MctsOnnx => "MctsOnnx",
            StrategyKind::MctsDL => "MctsDL",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        let norm = s.trim().to_ascii_lowercase().replace(['-', '_'], "");
        match norm.as_str() {
            "random" => Some(StrategyKind::Random),
            "revealfirst" => Some(StrategyKind::RevealFirst),
            "capturefirst" => Some(StrategyKind::CaptureFirst),
            "expectimax" | "engine" => Some(StrategyKind::Expectimax),
            "expectimaxnnue" | "nnue" => Some(StrategyKind::ExpectimaxNnue),
            "mctsonnx" => Some(StrategyKind::MctsOnnx),
            "mctsdl" => Some(StrategyKind::MctsDL),
            _ => None,
        }
    }
    pub fn needs_model(self) -> bool {
        matches!(
            self,
            StrategyKind::ExpectimaxNnue | StrategyKind::MctsOnnx | StrategyKind::MctsDL
        )
    }
    pub fn is_mcts(self) -> bool {
        matches!(self, StrategyKind::MctsOnnx | StrategyKind::MctsDL)
    }
    pub fn engine_kind(self) -> Option<EngineKind> {
        match self {
            StrategyKind::ExpectimaxNnue => Some(EngineKind::Nnue),
            StrategyKind::MctsOnnx => Some(EngineKind::MctsOnnx),
            StrategyKind::MctsDL => Some(EngineKind::MctsDL),
            _ => None,
        }
    }
    pub fn from_engine_kind(engine: EngineKind) -> Self {
        match engine {
            EngineKind::Nnue => StrategyKind::ExpectimaxNnue,
            EngineKind::MctsOnnx => StrategyKind::MctsOnnx,
            EngineKind::MctsDL => StrategyKind::MctsDL,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineParams {
    pub mcts_sims: usize,
    pub engine_budget: u64,
    pub nnue_budget: SearchBudget,
}
impl EngineParams {
    pub fn clamped(mut self) -> Self {
        self.mcts_sims = self.mcts_sims.max(1);
        self.engine_budget = self.engine_budget.max(1);
        self.nnue_budget = self.nnue_budget.clamped();
        self
    }
    pub fn updated(
        self,
        mcts_sims: Option<usize>,
        engine_budget: Option<u64>,
        nnue_budget: Option<SearchBudget>,
    ) -> Self {
        let mut p = self;
        if let Some(v) = mcts_sims {
            p.mcts_sims = v;
        }
        if let Some(v) = engine_budget {
            p.engine_budget = v;
        }
        if let Some(v) = nnue_budget {
            p.nnue_budget = v;
        }
        p.clamped()
    }
}
impl Default for EngineParams {
    fn default() -> Self {
        Self {
            mcts_sims: 200,
            engine_budget: 300_000,
            nnue_budget: SearchBudget::default(),
        }
    }
}
#[derive(Debug, Clone)]
pub enum PlayerSlot {
    Human,
    Strategy {
        kind: StrategyKind,
        model_file: Option<String>,
        params: EngineParams,
    },
    Agent(Uuid),
}
impl PlayerSlot {
    pub fn strategy(kind: StrategyKind, model_file: Option<String>, params: EngineParams) -> Self {
        PlayerSlot::Strategy {
            kind,
            model_file,
            params,
        }
    }
    pub fn is_human(&self) -> bool {
        matches!(self, PlayerSlot::Human)
    }
    pub fn kind(&self) -> Option<StrategyKind> {
        match self {
            PlayerSlot::Human | PlayerSlot::Agent(_) => None,
            PlayerSlot::Strategy { kind, .. } => Some(*kind),
        }
    }
    pub fn agent_id(&self) -> Option<Uuid> {
        match self {
            PlayerSlot::Agent(id) => Some(*id),
            _ => None,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Slots {
    pub red: PlayerSlot,
    pub black: PlayerSlot,
}
impl Slots {
    pub fn new(red: PlayerSlot, black: PlayerSlot) -> Self {
        Self { red, black }
    }
    pub fn all_human() -> Self {
        Self::new(PlayerSlot::Human, PlayerSlot::Human)
    }
    pub fn side(&self, player: banqi_core::core::env::types::Player) -> &PlayerSlot {
        match player {
            banqi_core::core::env::types::Player::Red => &self.red,
            banqi_core::core::env::types::Player::Black => &self.black,
        }
    }
    pub fn side_mut(&mut self, player: banqi_core::core::env::types::Player) -> &mut PlayerSlot {
        match player {
            banqi_core::core::env::types::Player::Red => &mut self.red,
            banqi_core::core::env::types::Player::Black => &mut self.black,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Agent {
    pub id: Uuid,
    pub variant: banqi_core::core::env::Variant,
    pub kind: StrategyKind,
    pub model_file: Option<String>,
    pub params: EngineParams,
}
pub struct Decision {
    pub action: usize,
    pub mcts_tree: Option<MctsTreeBox>,
    pub ex_tree: Option<crate::app::expectimax_view::ExpectimaxTree>,
    pub mcts_stats: Option<MctsStatsDto>,
    pub nnue_stats: Option<NnueStatsDto>,
    pub score: Option<f32>,
}
impl Decision {
    fn plain(action: usize) -> Self {
        Self {
            action,
            mcts_tree: None,
            ex_tree: None,
            mcts_stats: None,
            nnue_stats: None,
            score: None,
        }
    }
}
pub fn decide(
    snap: &PositionSnapshot,
    slot: &PlayerSlot,
    prev_tree: Option<MctsTree<DarkChessEnv>>,
    pool: &ModelPool,
) -> Result<Decision, String> {
    let (kind, model_file, params) = match slot {
        PlayerSlot::Human => {
            return Err("当前行棋方由用户控制，无策略可执行".into());
        }
        PlayerSlot::Agent(_) => {
            return Err("Agent 槽位未解析为策略槽位".into());
        }
        PlayerSlot::Strategy {
            kind,
            model_file,
            params,
        } => (*kind, model_file.clone(), *params),
    };
    let env = DarkChessEnv::from_snapshot(snap).map_err(|e| format!("重建环境失败: {e}"))?;
    match kind {
        StrategyKind::Random => {
            let action = RandomPolicy::choose_action(&env).ok_or("无合法动作")?;
            Ok(Decision::plain(action))
        }
        StrategyKind::RevealFirst => {
            let action = RevealFirstPolicy::choose_action(&env).ok_or("无合法动作")?;
            Ok(Decision::plain(action))
        }
        StrategyKind::CaptureFirst => {
            let action = CaptureFirstPolicy::choose_action(&env).ok_or("无合法动作")?;
            Ok(Decision::plain(action))
        }
        StrategyKind::Expectimax => {
            let budget = SearchBudget {
                mode: crate::app::search::BudgetMode::Nodes,
                nodes: params.engine_budget,
                timeout_ms: params.nnue_budget.timeout_ms,
                ..Default::default()
            };
            let mut cfg = budget.clamped().to_search_config();
            cfg.nnue_evaluator = Some(Arc::new(RuleMaterialEval::default()));
            let result = expectimax::search(&env, &cfg).ok_or("无合法动作")?;
            let mut d = Decision::plain(result.action);
            d.score = Some(result.value);
            Ok(d)
        }
        StrategyKind::ExpectimaxNnue => {
            let file = model_file.ok_or("ExpectimaxNnue 策略未绑定 .nnue 模型文件")?;
            let evaluator = pool.acquire_nnue(&file)?;
            let (action, kept) =
                crate::app::search::nnue_search_tree(&env, evaluator, params.nnue_budget)?;
            let stats = kept.stats.clone();
            Ok(Decision {
                action,
                mcts_tree: None,
                ex_tree: Some(kept),
                mcts_stats: None,
                nnue_stats: Some(NnueStatsDto::from(&stats)),
                score: None,
            })
        }
        StrategyKind::MctsOnnx => {
            #[cfg(feature = "onnx")]
            {
                mcts_branch(
                    model_file,
                    "MctsOnnx 策略未绑定 .onnx 模型文件",
                    &env,
                    params.mcts_sims,
                    prev_tree,
                    |f| pool.acquire_onnx(f),
                    banqi_engine::engine::gumbel::mcts_search_onnx::<DarkChessEnv>,
                )
            }
            #[cfg(not(feature = "onnx"))]
            {
                let _ = (model_file, prev_tree);
                Err(
                    "服务编译未启用 onnx 推理，MCTS+ONNX 策略不可用（可改用 ExpectimaxNnue）"
                        .into(),
                )
            }
        }
        StrategyKind::MctsDL => {
            #[cfg(feature = "torch")]
            {
                mcts_branch(
                    model_file,
                    "MctsDL 策略未绑定 .pt 模型文件",
                    &env,
                    params.mcts_sims,
                    prev_tree,
                    |f| pool.acquire_torch(f),
                    banqi_engine::engine::gumbel::mcts_search_torch::<DarkChessEnv>,
                )
            }
            #[cfg(not(feature = "torch"))]
            {
                let _ = (model_file, prev_tree);
                Err("服务编译未启用 torch 推理，MCTS+DL 策略不可用（可改用 MctsOnnx / ExpectimaxNnue）".into())
            }
        }
    }
}
#[cfg(any(feature = "onnx", feature = "torch"))]
fn mcts_branch<M>(
    file: Option<String>,
    missing: &str,
    env: &DarkChessEnv,
    sims: usize,
    prev_tree: Option<MctsTree<DarkChessEnv>>,
    acquire: impl FnOnce(&str) -> Result<Arc<M>, String>,
    search: impl FnOnce(
        &DarkChessEnv,
        usize,
        Option<Arc<M>>,
        Option<MctsTree<DarkChessEnv>>,
    ) -> Result<MctsTree<DarkChessEnv>, String>,
) -> Result<Decision, String> {
    let file = file.ok_or(missing)?;
    let model = acquire(&file)?;
    let tree = search(env, sims, Some(model), prev_tree)?;
    Ok(mcts_decision(tree))
}
fn mcts_decision(tree: MctsTree<DarkChessEnv>) -> Decision {
    let action = tree.chosen_action;
    let score = {
        let root = tree.arena.get(tree.root_idx);
        let n = root.visit_count.max(1);
        Some(root.value_sum / n as f32)
    };
    let boxed: MctsTreeBox = Box::new(tree);
    let mcts_stats = MctsStatsDto::from_session(Some(&boxed));
    Decision {
        action,
        mcts_tree: Some(boxed),
        ex_tree: None,
        mcts_stats,
        nnue_stats: None,
        score,
    }
}
pub fn dry_run(
    snap: &PositionSnapshot,
    slot: &PlayerSlot,
    pool: &ModelPool,
) -> Result<Decision, String> {
    decide(snap, slot, None, pool)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::envops;
    use banqi_core::core::env::Variant;
    fn initial_snap() -> PositionSnapshot {
        envops::initial_snapshot(Variant::DarkChess4x8)
    }
    fn human_vs(kind: StrategyKind) -> Slots {
        Slots::new(
            PlayerSlot::Human,
            PlayerSlot::strategy(kind, None, EngineParams::default()),
        )
    }
    #[test]
    fn parses_strategy_strings_with_legacy_aliases() {
        assert_eq!(StrategyKind::parse("Random"), Some(StrategyKind::Random));
        assert_eq!(
            StrategyKind::parse("reveal_first"),
            Some(StrategyKind::RevealFirst)
        );
        assert_eq!(
            StrategyKind::parse("CaptureFirst"),
            Some(StrategyKind::CaptureFirst)
        );
        assert_eq!(
            StrategyKind::parse("engine"),
            Some(StrategyKind::Expectimax)
        );
        assert_eq!(
            StrategyKind::parse("Expectimax"),
            Some(StrategyKind::Expectimax)
        );
        assert_eq!(
            StrategyKind::parse("nnue"),
            Some(StrategyKind::ExpectimaxNnue)
        );
        assert_eq!(
            StrategyKind::parse("expectimax-nnue"),
            Some(StrategyKind::ExpectimaxNnue)
        );
        assert_eq!(
            StrategyKind::parse("mcts-onnx"),
            Some(StrategyKind::MctsOnnx)
        );
        assert_eq!(StrategyKind::parse("mcts_dl"), Some(StrategyKind::MctsDL));
        assert!(StrategyKind::parse("pvp").is_none());
        assert!(StrategyKind::parse("human").is_none());
    }
    #[test]
    fn kind_strings_match_frontend_contract() {
        assert_eq!(StrategyKind::RevealFirst.as_str(), "RevealFirst");
        assert_eq!(StrategyKind::Expectimax.as_str(), "Expectimax");
        assert_eq!(StrategyKind::ExpectimaxNnue.as_str(), "ExpectimaxNnue");
        assert_eq!(StrategyKind::MctsOnnx.as_str(), "MctsOnnx");
        assert_eq!(StrategyKind::MctsDL.as_str(), "MctsDL");
        assert!(!StrategyKind::Random.needs_model());
        assert!(!StrategyKind::Expectimax.needs_model());
        assert!(StrategyKind::ExpectimaxNnue.needs_model());
        assert!(StrategyKind::MctsOnnx.needs_model());
        assert!(StrategyKind::MctsDL.needs_model());
        assert!(StrategyKind::MctsOnnx.is_mcts());
        assert!(!StrategyKind::Expectimax.is_mcts());
    }
    #[test]
    fn slots_side_access() {
        use banqi_core::core::env::types::Player;
        let slots = human_vs(StrategyKind::Expectimax);
        assert!(slots.side(Player::Red).is_human());
        assert!(!slots.side(Player::Black).is_human());
        assert_eq!(
            slots.side(Player::Black).kind(),
            Some(StrategyKind::Expectimax)
        );
    }
    #[test]
    fn rule_strategies_return_legal_action() {
        let snap = initial_snap();
        for kind in [
            StrategyKind::Random,
            StrategyKind::RevealFirst,
            StrategyKind::CaptureFirst,
        ] {
            let slot = PlayerSlot::strategy(kind, None, EngineParams::default());
            let d = decide(&snap, &slot, None, &dummy_pool()).expect("初始局面必有合法动作");
            let legal =
                envops::legal_actions(&DarkChessEnv::from_snapshot(&snap).expect("重建不应失败"));
            assert!(
                legal.contains(&d.action),
                "{kind:?} 选出的动作必须在合法集合内"
            );
            assert!(d.mcts_tree.is_none() && d.ex_tree.is_none());
        }
    }
    #[test]
    fn expectimax_returns_legal_action_with_score() {
        let snap = initial_snap();
        let slot = PlayerSlot::strategy(
            StrategyKind::Expectimax,
            None,
            EngineParams {
                engine_budget: 1_000,
                ..EngineParams::default()
            },
        );
        let d = decide(&snap, &slot, None, &dummy_pool()).expect("小预算搜索应返回动作");
        let legal =
            envops::legal_actions(&DarkChessEnv::from_snapshot(&snap).expect("重建不应失败"));
        assert!(legal.contains(&d.action));
        assert!(d.score.is_some(), "Expectimax 决策应携带根评估值");
    }
    #[test]
    fn human_slot_decision_errors() {
        let snap = initial_snap();
        assert!(decide(&snap, &PlayerSlot::Human, None, &dummy_pool()).is_err());
    }
    #[test]
    fn engine_params_partial_update_clamps() {
        let base = EngineParams::default();
        let p = base.updated(
            Some(0),
            Some(0),
            Some(SearchBudget {
                nodes: 0,
                depth: 0,
                timeout_ms: 0,
                ..Default::default()
            }),
        );
        assert_eq!(p.mcts_sims, 1);
        assert_eq!(p.engine_budget, 1);
        assert_eq!(p.nnue_budget.nodes, 1);
        assert_eq!(p.nnue_budget.depth, 1);
        assert_eq!(p.nnue_budget.timeout_ms, 1);
        let p = base.updated(None, Some(500_000), None);
        assert_eq!(p.engine_budget, 500_000);
        assert_eq!(p.mcts_sims, base.mcts_sims);
        assert_eq!(p.nnue_budget, base.nnue_budget);
    }
    fn dummy_pool() -> ModelPool {
        ModelPool::new(
            &std::env::temp_dir().join("banqi-player-test-no-models"),
            1,
            "cpu",
            0,
        )
    }
}
