use banqi_core::core::env::Variant;
use banqi_core::core::expectimax::SearchConfig;
use std::path::PathBuf;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetMode {
    Nodes,
    DepthNodes,
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
    pub fn parse(s: &str) -> Option<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_', '+', ' '], "")
            .as_str()
        {
            "nodes" => Some(BudgetMode::Nodes),
            "depthnodes" => Some(BudgetMode::DepthNodes),
            "time" => Some(BudgetMode::Time),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudget {
    pub mode: BudgetMode,
    pub nodes: u64,
    pub depth: i32,
    pub time_ms: u64,
    pub timeout_ms: u64,
}
impl Default for SearchBudget {
    fn default() -> Self {
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
    pub fn clamped(mut self) -> Self {
        self.nodes = self.nodes.max(1);
        self.depth = self.depth.max(1);
        self.timeout_ms = self.timeout_ms.max(1);
        self
    }
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
            _ => 64,
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
            engine_budget: SearchBudget {
                mode: BudgetMode::Nodes,
                nodes: 300_000,
                ..SearchBudget::default()
            },
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
    pub default_engine_budget: u64,
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
            nn_cache_entries: 200_000,
            default_engine_budget: 300_000,
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
                "--nnue-mode" => {
                    self.default_nnue.mode = BudgetMode::parse(&value).ok_or_else(|| {
                        format!(
                            "--nnue-mode 未知模式: {value}（可选 nodes / depth_nodes / time）"
                        )
                    })?
                }
                "--nnue-depth" => {
                    self.default_nnue.depth = value
                        .parse()
                        .map_err(|_| format!("--nnue-depth 需要正整数，得到: {value}"))?
                }
                "--nnue-budget" => {
                    self.default_nnue.nodes = value
                        .parse()
                        .map_err(|_| format!("--nnue-budget 需要正整数，得到: {value}"))?
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
        if self.default_engine_budget == 0 {
            return Err("default_engine_budget 必须至少为 1".to_string());
        }
        if self.default_nnue.nodes == 0 {
            return Err("default_nnue.nodes 必须至少为 1".to_string());
        }
        if self.default_nnue.depth <= 0 {
            return Err("default_nnue.depth 必须至少为 1".to_string());
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
         \x20   --engine-budget <N>         强引擎默认节点预算（默认 300000）\n\
         \x20   --nnue-mode <MODE>          NNUE 对手默认预算模式 nodes|depth_nodes|time\n\
         \x20                               （默认 depth_nodes）\n\
         \x20   --nnue-depth <N>            NNUE 对手默认深度上限（depth_nodes 模式，默认 8）\n\
         \x20   --nnue-budget <N>           NNUE 对手默认节点预算（nodes/depth_nodes 模式，默认 200000）\n\
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
    Variant::ALL
        .iter()
        .copied()
        .find(|v| v.as_str().eq_ignore_ascii_case(s))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_mode_parse() {
        assert_eq!(BudgetMode::parse("nodes"), Some(BudgetMode::Nodes));
        assert_eq!(
            BudgetMode::parse("DepthNodes"),
            Some(BudgetMode::DepthNodes)
        );
        assert_eq!(
            BudgetMode::parse("depth+nodes"),
            Some(BudgetMode::DepthNodes)
        );
        assert_eq!(BudgetMode::parse("time"), Some(BudgetMode::Time));
        assert!(BudgetMode::parse("mcts").is_none());
    }
    #[test]
    fn nodes_mode_uses_timeout_as_time_limit() {
        let b = SearchBudget {
            mode: BudgetMode::Nodes,
            nodes: 5_000,
            ..Default::default()
        };
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
    #[test]
    fn engine_params_partial_update_clamps() {
        let base = EngineParams::default();
        let p = base.updated(
            Some(0),
            Some(SearchBudget {
                nodes: 0,
                depth: 0,
                timeout_ms: 0,
                ..Default::default()
            }),
            Some(SearchBudget {
                nodes: 0,
                depth: 0,
                timeout_ms: 0,
                ..Default::default()
            }),
        );
        assert_eq!(p.mcts_sims, 1);
        assert_eq!(p.engine_budget.nodes, 1);
        assert_eq!(p.nnue_budget.nodes, 1);
        assert_eq!(p.nnue_budget.depth, 1);
        assert_eq!(p.nnue_budget.timeout_ms, 1);
        let p = base.updated(
            None,
            Some(SearchBudget {
                mode: BudgetMode::Nodes,
                nodes: 500_000,
                ..Default::default()
            }),
            None,
        );
        assert_eq!(p.engine_budget.nodes, 500_000);
        assert_eq!(p.mcts_sims, base.mcts_sims);
        assert_eq!(p.nnue_budget, base.nnue_budget);
    }
}
