//! 服务器配置：CLI 参数优先，最后取默认值（不读环境变量，全部经 CLI 显式传递）。

use std::path::PathBuf;

use banqi_app::search::SearchBudget;
use banqi_core::core::env::Variant;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// 监听地址，如 `127.0.0.1:8080`
    pub addr: String,
    /// ONNX 模型目录（文件名即变体 ID：`<variant_id>.onnx`）
    pub models_dir: PathBuf,
    /// ONNX 会话池大小（并发推理通道数）
    pub sessions: usize,
    /// 推理设备："cpu" / "cuda" / "auto"（cuda 需 onnx-cuda feature）
    pub device: String,
    /// 默认 MCTS 模拟次数（创建对局未指定时使用）
    pub default_sims: usize,
    /// 默认对局变体（创建对局未指定时使用；None = 取首个有模型者，再退回首个变体）
    pub default_variant: Option<Variant>,
    /// NN 推理结果缓存条目上限（0 = 关闭）。条目 ≈ 1.5-2KB，
    /// 20 万条 ≈ 300-400MB；收益来自跨局/跨树的同观测重复评估。
    pub nn_cache_entries: usize,
    /// Expectimax 强引擎默认节点预算（创建对局未指定时使用）
    pub default_engine_budget: u64,
    /// NNUE 对手默认搜索预算（模式 + 节点/深度/时间 + 硬超时；创建对局未指定时使用）
    pub default_nnue: SearchBudget,
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
        }
    }
}

impl ServerConfig {
    /// 解析配置：CLI 参数 > 默认值（不读环境变量，全部经 CLI 显式传递）。
    pub fn from_env_and_args() -> Result<Self, String> {
        let mut cfg = Self::default();
        cfg.apply_args(std::env::args().skip(1))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// CLI 参数：--addr / --models-dir / --sessions / --device
    /// （默认模拟次数不再由 CLI 指定，仅运行时经 settings 接口调整）
    fn apply_args<I: Iterator<Item = String>>(&mut self, args: I) -> Result<(), String> {
        let args: Vec<String> = args.collect();
        let mut i = 0;
        while i < args.len() {
            // 无值选项：--help / -h（在取值前处理，避免「缺少值」误报）
            if args[i] == "--help" || args[i] == "-h" {
                return Err(Self::usage());
            }
            let (key, value) = match (args[i].as_str(), args.get(i + 1)) {
                // --key=value 形式
                (k, _) if k.starts_with("--") && k.contains('=') => {
                    let (k, v) = k.split_once('=').unwrap();
                    (k.to_string(), Some(v.to_string()))
                }
                // --key value 形式
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
                    self.nn_cache_entries = value
                        .parse()
                        .map_err(|_| format!("--nn-cache 需要非负整数（0 = 关闭），得到: {value}"))?
                }
                "--nnue-mode" => {
                    self.default_nnue.mode = banqi_app::search::BudgetMode::parse(&value)
                        .ok_or_else(|| {
                            format!("--nnue-mode 未知模式: {value}（可选 nodes / depth_nodes / time）")
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
                    self.default_nnue.timeout_ms = value
                        .parse()
                        .map_err(|_| format!("--search-timeout 需要正整数（毫秒），得到: {value}"))?
                }
                "--default-variant" => {
                    self.default_variant = Some(parse_variant(&value).ok_or_else(|| {
                        format!(
                            "--default-variant 未知变体: {value}（可用: {}）",
                            Variant::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
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
         \x20   --default-variant <ID>      默认对局变体（缺省时取首个有模型者，\n\
         \x20                               再退回首个变体）"
            .to_string()
    }
}

/// 由变体字符串标识解析 `Variant`（不认识返回 None）。
///
/// 直接遍历 `Variant::ALL` 匹配 `as_str()`，无任何硬编码变体清单。
pub fn parse_variant(s: &str) -> Option<Variant> {
    Variant::ALL
        .iter()
        .copied()
        .find(|v| v.as_str().eq_ignore_ascii_case(s))
}
