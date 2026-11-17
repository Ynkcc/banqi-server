use banqi_core::core::env::Variant;
use bq_inference::DEFAULT_NN_CACHE_ENTRIES;
use std::path::PathBuf;

pub use bq_engine_config::{ExpectimaxPartial, ExpectimaxSchema, MctsPartial, MctsSchema};

pub fn parse_mcts_partial(json: &str) -> Result<MctsPartial, String> {
    serde_json::from_str(json.trim()).map_err(|e| format!("MCTS schema JSON 解析失败: {e}"))
}

pub fn parse_expectimax_partial(json: &str) -> Result<ExpectimaxPartial, String> {
    serde_json::from_str(json.trim()).map_err(|e| format!("Expectimax schema JSON 解析失败: {e}"))
}

fn expectimax_depth_partial(
    deterministic_depth: Option<i32>,
    chance_outcome_depth: Option<i32>,
) -> ExpectimaxPartial {
    serde_json::from_value(serde_json::json!({
        "deterministic_depth": deterministic_depth,
        "chance_outcome_depth": chance_outcome_depth,
    }))
    .expect("双字段 ExpectimaxPartial 构造不应失败")
}

pub fn mcts_sims_partial(sims: usize) -> MctsPartial {
    serde_json::from_value(serde_json::json!({ "sims": sims }))
        .expect("sims MctsPartial 构造不应失败")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeBudgetMode {
    Depth,
    Time,
}
impl TimeBudgetMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TimeBudgetMode::Depth => "depth",
            TimeBudgetMode::Time => "time",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_', '+', ' '], "")
            .as_str()
        {
            "depth" | "depthnodes" => Some(TimeBudgetMode::Depth),
            "time" => Some(TimeBudgetMode::Time),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBudget {
    pub mode: TimeBudgetMode,
    pub time_ms: u64,
    pub timeout_ms: u64,
}
impl Default for TimeBudget {
    fn default() -> Self {
        Self {
            mode: TimeBudgetMode::Depth,
            time_ms: 5_000,
            timeout_ms: 60_000,
        }
    }
}
impl TimeBudget {
    pub fn time_limit_ms(&self) -> u64 {
        let timeout = self.timeout_ms.max(1);
        match self.mode {
            TimeBudgetMode::Time if self.time_ms > 0 => self.time_ms.min(timeout),
            _ => timeout,
        }
    }
    pub fn clamped(mut self) -> Self {
        self.timeout_ms = self.timeout_ms.max(1);
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ExpectimaxRun {
    pub schema: ExpectimaxSchema,
    pub time: TimeBudget,
}
impl ExpectimaxRun {
    pub fn clamped(mut self) -> Self {
        self.schema.deterministic_depth = self.schema.deterministic_depth.max(1);
        self.schema.chance_outcome_depth = self.schema.chance_outcome_depth.max(0);
        self.time = self.time.clamped();
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootTuning {
    pub aspiration: bool,
    pub chance_exp: bool,
    pub threads: usize,
}
impl Default for RootTuning {
    fn default() -> Self {
        Self {
            aspiration: false,
            chance_exp: true,
            threads: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotParams {
    pub mcts: MctsSchema,
    pub engine: ExpectimaxRun,
    pub nnue: ExpectimaxRun,

    pub roots: RootTuning,
}
impl Default for SlotParams {
    fn default() -> Self {
        Self {
            mcts: MctsSchema {
                sims: 200,
                ..MctsSchema::default()
            },
            engine: ExpectimaxRun::default(),
            nnue: ExpectimaxRun::default(),
            roots: RootTuning::default(),
        }
    }
}
impl SlotParams {
    pub fn clamped(mut self) -> Self {
        self.mcts.sims = self.mcts.sims.max(1);
        self.engine = self.engine.clamped();
        self.nnue = self.nnue.clamped();
        self
    }

    pub fn updated(
        self,
        mcts: Option<MctsSchema>,
        engine: Option<ExpectimaxRun>,
        nnue: Option<ExpectimaxRun>,
    ) -> Self {
        let mut p = self;
        if let Some(v) = mcts {
            p.mcts = v;
        }
        if let Some(v) = engine {
            p.engine = v;
        }
        if let Some(v) = nnue {
            p.nnue = v;
        }
        p.clamped()
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub addr: String,
    pub models_dir: PathBuf,
    pub sessions: usize,
    pub device: String,

    pub default_mcts: MctsSchema,
    pub default_variant: Option<Variant>,
    pub nn_cache_entries: usize,

    pub default_engine: ExpectimaxRun,

    pub default_nnue: ExpectimaxRun,

    pub root_tuning: RootTuning,
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
            default_mcts: MctsSchema {
                sims: 200,
                ..MctsSchema::default()
            },
            default_variant: None,
            nn_cache_entries: DEFAULT_NN_CACHE_ENTRIES,
            default_engine: ExpectimaxRun::default(),
            default_nnue: ExpectimaxRun::default(),
            root_tuning: RootTuning::default(),
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


                "--mcts-schema" => {
                    let partial = parse_mcts_partial(&value)?;
                    self.default_mcts.merge_partial(partial);
                }
                "--engine-schema" => {
                    let partial = parse_expectimax_partial(&value)?;
                    self.default_engine.schema.merge_partial(partial);
                }
                "--nnue-schema" => {
                    let partial = parse_expectimax_partial(&value)?;
                    self.default_nnue.schema.merge_partial(partial);
                }

                "--engine-mode" => {
                    self.default_engine.time.mode =
                        TimeBudgetMode::parse(&value).ok_or_else(|| {
                            format!("--engine-mode 未知模式: {value}（可选 depth / time，旧值 depth_nodes 视为 depth）")
                        })?
                }
                "--engine-deterministic-depth" => {
                    let v = value
                        .parse()
                        .map_err(|_| format!("--engine-deterministic-depth 需要正整数，得到: {value}"))?;
                    self.default_engine
                        .schema
                        .merge_partial(expectimax_depth_partial(Some(v), None));
                }
                "--engine-chance-depth" => {
                    let v = value
                        .parse()
                        .map_err(|_| format!("--engine-chance-depth 需要非负整数，得到: {value}"))?;
                    self.default_engine
                        .schema
                        .merge_partial(expectimax_depth_partial(None, Some(v)));
                }
                "--engine-time" => {
                    self.default_engine.time.time_ms = value
                        .parse()
                        .map_err(|_| format!("--engine-time 需要非负整数（毫秒），得到: {value}"))?
                }
                "--nnue-mode" => {
                    self.default_nnue.time.mode =
                        TimeBudgetMode::parse(&value).ok_or_else(|| {
                            format!("--nnue-mode 未知模式: {value}（可选 depth / time，旧值 depth_nodes 视为 depth）")
                        })?
                }
                "--nnue-deterministic-depth" => {
                    let v = value
                        .parse()
                        .map_err(|_| format!("--nnue-deterministic-depth 需要正整数，得到: {value}"))?;
                    self.default_nnue
                        .schema
                        .merge_partial(expectimax_depth_partial(Some(v), None));
                }
                "--nnue-chance-depth" => {
                    let v = value
                        .parse()
                        .map_err(|_| format!("--nnue-chance-depth 需要非负整数，得到: {value}"))?;
                    self.default_nnue
                        .schema
                        .merge_partial(expectimax_depth_partial(None, Some(v)));
                }
                "--nnue-time" => {
                    self.default_nnue.time.time_ms = value
                        .parse()
                        .map_err(|_| format!("--nnue-time 需要非负整数（毫秒），得到: {value}"))?
                }
                "--search-timeout" => {
                    self.default_nnue.time.timeout_ms = value.parse().map_err(|_| {
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

        self.default_mcts
            .validate()
            .map_err(|e| format!("default_mcts 非法: {e}"))?;
        self.default_engine
            .schema
            .validate()
            .map_err(|e| format!("default_engine schema 非法: {e}"))?;
        self.default_nnue
            .schema
            .validate()
            .map_err(|e| format!("default_nnue schema 非法: {e}"))?;
        if self.default_engine.time.timeout_ms == 0 {
            return Err("default_engine.timeout_ms 必须至少为 1".to_string());
        }
        if self.default_nnue.time.timeout_ms == 0 {
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
         \x20   --mcts-schema <JSON>        MCTS schema Partial JSON（覆盖默认；字段\n\
         \x20                               sims/max_considered_actions/eta/ucb_c/crn_enabled/\n\
         \x20                               health{...} 等，旧名 mcts_sims 等 alias 兼容）\n\
         \x20   --engine-schema <JSON>      强引擎 Expectimax schema Partial JSON\n\
         \x20                               （deterministic_depth/chance_outcome_depth/\n\
         \x20                               fullwidth/expl{...}，旧名 expectimax_* 兼容）\n\
         \x20   --nnue-schema <JSON>        NNUE 对手 Expectimax schema Partial JSON\n\
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

    fn depth_partial(depth: i32, chance: i32) -> ExpectimaxPartial {
        serde_json::from_value(serde_json::json!({
            "deterministic_depth": depth,
            "chance_outcome_depth": chance,
        }))
        .unwrap()
    }

    #[test]
    fn time_budget_mode_parse() {
        assert_eq!(TimeBudgetMode::parse("depth"), Some(TimeBudgetMode::Depth));
        assert_eq!(TimeBudgetMode::parse("Depth"), Some(TimeBudgetMode::Depth));
        assert_eq!(
            TimeBudgetMode::parse("DepthNodes"),
            Some(TimeBudgetMode::Depth),
            "旧值 depth_nodes 兼容为 depth"
        );
        assert_eq!(
            TimeBudgetMode::parse("depth+nodes"),
            Some(TimeBudgetMode::Depth),
            "旧值 depth_nodes 兼容为 depth"
        );
        assert_eq!(TimeBudgetMode::parse("time"), Some(TimeBudgetMode::Time));
        assert!(
            TimeBudgetMode::parse("nodes").is_none(),
            "节点预算模式已移除"
        );
        assert!(TimeBudgetMode::parse("mcts").is_none());
    }

    #[test]
    fn time_limit_ms_semantics_preserved() {
        let b = TimeBudget {
            mode: TimeBudgetMode::Depth,
            time_ms: 5_000,
            timeout_ms: 10_000,
        };
        assert_eq!(b.time_limit_ms(), 10_000, "硬超时兜底");

        let b = TimeBudget {
            mode: TimeBudgetMode::Time,
            time_ms: 3_000,
            timeout_ms: 2_000,
        };
        assert_eq!(b.time_limit_ms(), 2_000, "时间预算被硬超时钳制");
    }

    #[test]
    fn clamped_rejects_zero() {
        let mut schema = ExpectimaxSchema::default();
        schema.merge_partial(depth_partial(0, -3));
        let run = ExpectimaxRun {
            schema,
            time: TimeBudget {
                mode: TimeBudgetMode::Depth,
                time_ms: 0,
                timeout_ms: 0,
            },
        }
        .clamped();
        assert_eq!(run.schema.deterministic_depth, 1);
        assert_eq!(run.schema.chance_outcome_depth, 0);
        assert_eq!(run.time.timeout_ms, 1);
        assert_eq!(run.time.time_ms, 0, "time_ms 允许 0（= 仅超时控制）");
    }

    #[test]
    fn slot_params_partial_update_clamps() {
        let base = SlotParams::default();
        let bad = ExpectimaxRun {
            schema: {
                let mut s = ExpectimaxSchema::default();
                s.merge_partial(depth_partial(0, -1));
                s
            },
            time: TimeBudget {
                timeout_ms: 0,
                ..Default::default()
            },
        };
        let p = base.updated(
            Some(MctsSchema {
                sims: 0,
                ..MctsSchema::default()
            }),
            Some(bad),
            Some(bad),
        );
        assert_eq!(p.mcts.sims, 1);
        assert_eq!(p.engine.schema.deterministic_depth, 1);
        assert_eq!(p.engine.schema.chance_outcome_depth, 0);
        assert_eq!(p.nnue.schema.deterministic_depth, 1);
        assert_eq!(p.nnue.schema.chance_outcome_depth, 0);
        assert_eq!(p.nnue.time.timeout_ms, 1);

        let p = base.updated(
            None,
            Some(ExpectimaxRun {
                time: TimeBudget {
                    mode: TimeBudgetMode::Time,
                    time_ms: 2_500,
                    ..Default::default()
                },
                ..Default::default()
            }),
            None,
        );
        assert_eq!(p.engine.time.mode, TimeBudgetMode::Time);
        assert_eq!(p.engine.time.time_ms, 2_500);
        assert_eq!(p.mcts, base.mcts);
        assert_eq!(p.nnue, base.nnue);
    }

    #[test]
    fn default_slot_params_match_legacy_defaults() {
        let p = SlotParams::default();

        assert_eq!(p.mcts.sims, 200);
        assert_eq!(p.engine.schema.deterministic_depth, 8);
        assert_eq!(p.engine.schema.chance_outcome_depth, 0);
        assert_eq!(p.engine.time.mode, TimeBudgetMode::Depth);
        assert_eq!(p.engine.time.timeout_ms, 60_000);
        assert_eq!(p.nnue, p.engine);

        assert_eq!(
            p.roots,
            RootTuning {
                aspiration: false,
                chance_exp: true
            }
        );
    }

    #[test]
    fn schema_json_args_merge_onto_defaults() {
        let mut cfg = ServerConfig::default();
        cfg.apply_args(
            [
                "--mcts-schema",
                r#"{"sims": 96, "ucb_c": 0.3}"#,
                "--engine-schema",
                r#"{"deterministic_depth": 6, "expl_enabled": true, "expl_topk": 4}"#,
                "--nnue-schema",
                r#"{"expectimax_chance_outcome_depth": 1}"#,
                "--engine-chance-depth",
                "2",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();
        assert_eq!(cfg.default_mcts.sims, 96);
        assert_eq!(cfg.default_mcts.ucb_c, 0.3);
        assert_eq!(cfg.default_engine.schema.deterministic_depth, 6);

        assert_eq!(cfg.default_engine.schema.expl.as_ref().unwrap().topk, 4);

        assert_eq!(cfg.default_engine.schema.chance_outcome_depth, 2);
        assert_eq!(cfg.default_nnue.schema.chance_outcome_depth, 1);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn schema_json_args_reject_unknown_fields() {
        let mut cfg = ServerConfig::default();
        assert!(
            cfg.apply_args(
                ["--mcts-schema", r#"{"nope": 1}"#]
                    .into_iter()
                    .map(String::from)
            )
            .is_err()
        );
        assert!(
            cfg.apply_args(
                ["--engine-schema", r#"{"foo": true}"#]
                    .into_iter()
                    .map(String::from)
            )
            .is_err()
        );
    }

    #[test]
    fn validate_rejects_bad_schema_json_values() {
        let mut cfg = ServerConfig::default();
        cfg.apply_args(
            ["--mcts-schema", r#"{"sims": 0}"#]
                .into_iter()
                .map(String::from),
        )
        .unwrap();
        assert!(cfg.validate().is_err(), "sims=0 应被 schema validate 拒绝");
    }
}
