use banqi_core::core::env::Variant;
use banqi_core::core::expectimax::SearchConfig;
use bq_inference::DEFAULT_NN_CACHE_ENTRIES;
use std::path::PathBuf;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetMode {
    Depth,
    Time,
}
impl BudgetMode {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetMode::Depth => "depth",
            BudgetMode::Time => "time",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_', '+', ' '], "")
            .as_str()
        {
            "depth" | "depthnodes" => Some(BudgetMode::Depth),
            "time" => Some(BudgetMode::Time),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudget {
    pub mode: BudgetMode,
    pub depth: i32,
    pub chance_outcome_depth: i32,
    pub time_ms: u64,
    pub timeout_ms: u64,
}
impl Default for SearchBudget {
    fn default() -> Self {
        Self {
            mode: BudgetMode::Depth,
            depth: 8,
            chance_outcome_depth: 0,
            time_ms: 5_000,
            timeout_ms: 60_000,
        }
    }
}
impl SearchBudget {
    pub fn clamped(mut self) -> Self {
        self.depth = self.depth.max(1);
        self.chance_outcome_depth = self.chance_outcome_depth.max(0);
        self.timeout_ms = self.timeout_ms.max(1);
        self
    }
    pub fn to_search_config(&self) -> SearchConfig {
        let timeout = self.timeout_ms.max(1);
        let mut cfg = SearchConfig::new(self.depth.max(1), self.chance_outcome_depth.max(0));
        cfg.budget.time_limit_ms = match self.mode {
            BudgetMode::Time if self.time_ms > 0 => self.time_ms.min(timeout),
            _ => timeout,
        };
        cfg
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineParams {
    pub mcts_sims: usize,

    pub engine_budget: SearchBudget,
    pub nnue_budget: SearchBudget,
}
impl EngineParams {
    pub fn clamped(mut self) -> Self {
        self.mcts_sims = self.mcts_sims.max(1);
        self.engine_budget = self.engine_budget.clamped();
        self.nnue_budget = self.nnue_budget.clamped();
        self
    }
    pub fn updated(
        self,
        mcts_sims: Option<usize>,
        engine_budget: Option<SearchBudget>,
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
            engine_budget: SearchBudget::default(),
            nnue_budget: SearchBudget::default(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub addr: String,
    pub models_dir: PathBuf,
    pub sessions: usize,
    pub device: String,
    pub default_sims: usize,
    pub default_variant: Option<Variant>,
    pub nn_cache_entries: usize,
    pub default_engine: SearchBudget,
    pub default_nnue: SearchBudget,

    pub recent_games: usize,
    pub archive_dir: Option<PathBuf>,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:8080".to_string(),
            models_dir: PathBuf::from("models"),
            sessions: 1,
            device: "cpu".to_string(),
            default_sims: 200,
            default_variant: None,
            nn_cache_entries: DEFAULT_NN_CACHE_ENTRIES,
            default_engine: SearchBudget::default(),
            default_nnue: SearchBudget::default(),
            recent_games: 20,
            archive_dir: None,
        }
    }
}
impl ServerConfig {
    pub fn from_env_and_args() -> Result<Self, String> {
        let mut cfg = Self::default();
        cfg.apply_args(std::env::args().skip(1))?;
        cfg.validate()?;
        Ok(cfg)
    }
    fn apply_args<I: Iterator<Item = String>>(&mut self, args: I) -> Result<(), String> {
        let args: Vec<String> = args.collect();
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--help" || args[i] == "-h" {
                return Err(Self::usage());
            }
            let (key, value) = match (args[i].as_str(), args.get(i + 1)) {
                (k, _) if k.starts_with("--") && k.contains('=') => {
                    let (k, v) = k.split_once('=').unwrap();
                    (k.to_string(), Some(v.to_string()))
                }
                (k, v) if k.starts_with("--") => (k.to_string(), v.cloned()),
                (k, _) => return Err(format!("未知参数: {k}")),
            };
            let value = value.ok_or_else(|| format!("参数 {key} 缺少值"))?;
            match key.as_str() {
                "--addr" => self.addr = value,
                "--models-dir" => self.models_dir = PathBuf::from(value),
                "--sessions" => {
                    self.sessions = value
                        .parse()
                        .map_err(|_| format!("--sessions 需要正整数，得到: {value}"))?
                }
                "--device" => self.device = value,
                "--nn-cache" => {
                    self.nn_cache_entries = value.parse().map_err(|_| {
                        format!("--nn-cache 需要非负整数（0 = 关闭），得到: {value}")
                    })?
                }
                "--engine-mode" => {
                    self.default_engine.mode = BudgetMode::parse(&value).ok_or_else(|| {
                        format!("--engine-mode 未知模式: {value}（可选 depth / time，旧值 depth_nodes 视为 depth）")
                    })?
                }
                "--engine-deterministic-depth" => {
                    self.default_engine.depth = value
                        .parse()
                        .map_err(|_| format!("--engine-deterministic-depth 需要正整数，得到: {value}"))?
                }
                "--engine-chance-depth" => {
                    self.default_engine.chance_outcome_depth = value
                        .parse()
                        .map_err(|_| format!("--engine-chance-depth 需要非负整数，得到: {value}"))?
                }
                "--engine-time" => {
                    self.default_engine.time_ms = value
                        .parse()
                        .map_err(|_| format!("--engine-time 需要非负整数（毫秒），得到: {value}"))?
                }
                "--nnue-mode" => {
                    self.default_nnue.mode = BudgetMode::parse(&value).ok_or_else(|| {
                        format!("--nnue-mode 未知模式: {value}（可选 depth / time，旧值 depth_nodes 视为 depth）")
                    })?
                }
                "--nnue-deterministic-depth" => {
                    self.default_nnue.depth = value
                        .parse()
                        .map_err(|_| format!("--nnue-deterministic-depth 需要正整数，得到: {value}"))?
                }
                "--nnue-chance-depth" => {
                    self.default_nnue.chance_outcome_depth = value
                        .parse()
                        .map_err(|_| format!("--nnue-chance-depth 需要非负整数，得到: {value}"))?
                }
                "--nnue-time" => {
                    self.default_nnue.time_ms = value
                        .parse()
                        .map_err(|_| format!("--nnue-time 需要非负整数（毫秒），得到: {value}"))?
                }
                "--search-timeout" => {
                    self.default_nnue.timeout_ms = value.parse().map_err(|_| {
                        format!("--search-timeout 需要正整数（毫秒），得到: {value}")
                    })?
                }
                "--recent-games" => {
                    self.recent_games = value.parse().map_err(|_| {
                        format!("--recent-games 需要非负整数（0 = 关闭归档），得到: {value}")
                    })?
                }
                "--archive-dir" => self.archive_dir = Some(PathBuf::from(value)),
                "--default-variant" => {
                    self.default_variant = Some(parse_variant(&value).ok_or_else(|| {
                        format!(
                            "--default-variant 未知变体: {value}（可用: {}）",
                            Variant::ALL
                                .iter()
                                .map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?)
                }
                other => return Err(format!("未知参数: {other}")),
            }
            i += 2;
        }
        Ok(())
    }
    fn validate(&self) -> Result<(), String> {
        if self.sessions == 0 {
            return Err("--sessions 必须至少为 1".to_string());
        }
        if self.default_sims == 0 {
            return Err("default_sims 必须至少为 1".to_string());
        }
        if self.default_engine.depth <= 0 {
            return Err("default_engine.depth 必须至少为 1".to_string());
        }
        if self.default_engine.chance_outcome_depth < 0 {
            return Err("default_engine.chance_outcome_depth 必须至少为 0".to_string());
        }
        if self.default_engine.timeout_ms == 0 {
            return Err("default_engine.timeout_ms 必须至少为 1".to_string());
        }
        if self.default_nnue.depth <= 0 {
            return Err("default_nnue.depth 必须至少为 1".to_string());
        }
        if self.default_nnue.chance_outcome_depth < 0 {
            return Err("default_nnue.chance_outcome_depth 必须至少为 0".to_string());
        }
        if self.default_nnue.timeout_ms == 0 {
            return Err("default_nnue.timeout_ms 必须至少为 1".to_string());
        }
        Ok(())
    }
    fn usage() -> String {
        "banqi-server — Banqi 暗棋对弈 HTTP 服务\n\
         \x20 用法: banqi-server [选项]（配置仅经 CLI 参数传递，不读环境变量）\n\
         \x20   --addr <ADDR>               监听地址（默认 127.0.0.1:8080）\n\
         \x20   --models-dir <DIR>          ONNX 模型目录（默认 models）\n\
         \x20   --sessions <N>              ONNX 会话池大小（默认 1）\n\
         \x20   --device <DEV>              cpu|cuda|auto（默认 cpu）\n\
         \x20   --nn-cache <N>              NN 推理缓存条目上限（默认 200000，0 = 关闭；\n\
         \x20                               每条 ≈ 1.5-2KB）\n\
         \x20   --engine-mode <MODE>        强引擎默认预算模式 depth|time（旧值 depth_nodes
         \x20                               视为 depth，默认 depth）\n\
         \x20   --engine-deterministic-depth <N>
         \x20                               强引擎默认确定性深度上限（depth 模式，默认 8）\n\
         \x20   --engine-chance-depth <N>   强引擎默认 chance outcome 展开深度（默认 0）\n\
         \x20   --engine-time <MS>          强引擎默认时间预算（time 模式，默认 5000 毫秒）\n\
         \x20   --nnue-mode <MODE>          NNUE 对手默认预算模式 depth|time（旧值
         \x20                               depth_nodes 视为 depth，默认 depth）\n\
         \x20   --nnue-deterministic-depth <N>
         \x20                               NNUE 对手默认确定性深度上限（depth 模式，默认 8）\n\
         \x20   --nnue-chance-depth <N>     NNUE 对手默认 chance outcome 展开深度（默认 0）\n\
         \x20   --nnue-time <MS>            NNUE 对手默认时间预算（time 模式，默认 5000 毫秒）\n\
         \x20   --search-timeout <MS>       搜索硬超时（全模式必选生效，默认 60000 毫秒；\n\
         \x20                               超时放弃后续搜索返回实际结果）\n\
         \x20   --recent-games <N>          最近对局归档条数上限（默认 20，0 = 关闭）\n\
         \x20   --archive-dir <DIR>         归档落盘目录（缺省仅保留内存归档，重启即失；\n\
         \x20                               指定后终局对局写 JSON 文件并在启动时加载）\n\
         \x20   --default-variant <ID>      默认对局变体（缺省时取首个有模型者，\n\
         \x20                               再退回首个变体）"
            .to_string()
    }
}
pub fn parse_variant(s: &str) -> Option<Variant> {
    s.parse().ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_mode_parse() {
        assert_eq!(BudgetMode::parse("depth"), Some(BudgetMode::Depth));
        assert_eq!(BudgetMode::parse("Depth"), Some(BudgetMode::Depth));
        assert_eq!(
            BudgetMode::parse("DepthNodes"),
            Some(BudgetMode::Depth),
            "旧值 depth_nodes 兼容为 depth"
        );
        assert_eq!(
            BudgetMode::parse("depth+nodes"),
            Some(BudgetMode::Depth),
            "旧值 depth_nodes 兼容为 depth"
        );
        assert_eq!(BudgetMode::parse("time"), Some(BudgetMode::Time));
        assert!(BudgetMode::parse("nodes").is_none(), "节点预算模式已移除");
        assert!(BudgetMode::parse("mcts").is_none());
    }
    #[test]
    fn depth_mode_sets_depths_and_timeout() {
        let b = SearchBudget {
            mode: BudgetMode::Depth,
            depth: 6,
            chance_outcome_depth: 2,
            timeout_ms: 10_000,
            ..Default::default()
        };
        let cfg = b.to_search_config();
        assert_eq!(cfg.budget.deterministic_depth, 6);
        assert_eq!(cfg.budget.expectimax_chance_outcome_depth, 2);
        assert_eq!(cfg.budget.time_limit_ms, 10_000, "硬超时兜底");
    }
    #[test]
    fn time_mode_caps_by_timeout() {
        let b = SearchBudget {
            mode: BudgetMode::Time,
            time_ms: 3_000,
            timeout_ms: 2_000,
            depth: 6,
            ..Default::default()
        };
        let cfg = b.to_search_config();
        assert_eq!(cfg.budget.time_limit_ms, 2_000, "时间预算被硬超时钳制");
        assert_eq!(cfg.budget.deterministic_depth, 6);
    }
    #[test]
    fn clamped_rejects_zero() {
        let b = SearchBudget {
            mode: BudgetMode::Depth,
            depth: 0,
            chance_outcome_depth: -3,
            time_ms: 0,
            timeout_ms: 0,
        }
        .clamped();
        assert_eq!((b.depth, b.chance_outcome_depth, b.timeout_ms), (1, 0, 1));
        assert_eq!(b.time_ms, 0, "time_ms 允许 0（= 仅超时控制）");
    }
    #[test]
    fn engine_params_partial_update_clamps() {
        let base = EngineParams::default();
        let p = base.updated(
            Some(0),
            Some(SearchBudget {
                depth: 0,
                chance_outcome_depth: -1,
                timeout_ms: 0,
                ..Default::default()
            }),
            Some(SearchBudget {
                depth: 0,
                chance_outcome_depth: -1,
                timeout_ms: 0,
                ..Default::default()
            }),
        );
        assert_eq!(p.mcts_sims, 1);
        assert_eq!(p.engine_budget.depth, 1);
        assert_eq!(p.engine_budget.chance_outcome_depth, 0);
        assert_eq!(p.nnue_budget.depth, 1);
        assert_eq!(p.nnue_budget.chance_outcome_depth, 0);
        assert_eq!(p.nnue_budget.timeout_ms, 1);
        let p = base.updated(
            None,
            Some(SearchBudget {
                mode: BudgetMode::Time,
                time_ms: 2_500,
                ..Default::default()
            }),
            None,
        );
        assert_eq!(p.engine_budget.mode, BudgetMode::Time);
        assert_eq!(p.engine_budget.time_ms, 2_500);
        assert_eq!(p.mcts_sims, base.mcts_sims);
        assert_eq!(p.nnue_budget, base.nnue_budget);
    }
}
