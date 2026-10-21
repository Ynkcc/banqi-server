//! banqi-cli — banqi-server 的命令行客户端（LLM / 脚本测试友好，替代 curl）。
//!
//! 仅是 REST API 的薄封装：不内嵌引擎、不持有状态，直连运行中的 banqi-server，
//! 所有响应以 pretty JSON 打印到 stdout，错误打到 stderr 并以非零码退出。
//! 服务器地址优先级：`--url` > 环境变量 `BANQI_URL` > 默认 `http://127.0.0.1:8080`。
//!
//! 常用示例：
//! ```text
//! banqi-cli health
//! banqi-cli new --opponent nnue                  # 创建对局，返回含 gameId
//! banqi-cli move <id> 42                          # 按 action 索引走子
//! banqi-cli move <id> 3:7                         # 按 from:to 自动匹配合法动作
//! banqi-cli ai-move <id>                          # 强制 AI 走子
//! banqi-cli undo <id>                             # 悔棋到当前行棋方上次行动前
//! banqi-cli engine "<BFEN>" --model dark_chess_4x8.nnue   # 无状态引擎查询
//! banqi-cli mcts root <id>                        # 查看搜索树根节点
//! ```

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

// ============================================================================
// CLI 定义
// ============================================================================

#[derive(Parser)]
#[command(
    name = "banqi-cli",
    version,
    about = "banqi-server 命令行客户端：REST API 的薄封装（LLM / 脚本测试用，替代 curl）"
)]
struct Cli {
    /// 服务器基础地址（优先级：--url > 环境变量 BANQI_URL > 默认值）
    #[arg(
        long,
        global = true,
        env = "BANQI_URL",
        value_name = "URL",
        default_value = "http://127.0.0.1:8080"
    )]
    url: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 健康检查（GET /api/health，含模型文件清单）
    Health,
    /// 列出全部棋盘变体（GET /api/variants）
    Variants,
    /// 读取 / 修改服务器运行时设置（GET / PUT /api/settings）
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    /// 列出进行中的对局（GET /api/games）
    Games,
    /// 创建对局（POST /api/games），返回含 gameId 的完整状态
    New {
        /// 棋盘变体 ID（缺省取服务器默认）
        #[arg(long)]
        variant: Option<String>,
        /// 对手类型：pvp / random / reveal_first / capture_first / engine / nnue / mcts_onnx
        #[arg(long)]
        opponent: Option<String>,
        /// AI 执子方：red / black（缺省 auto = AI 执黑，人类执红先行）
        #[arg(long)]
        ai_player: Option<String>,
        /// 绑定模型文件（须在服务器模型目录内；缺省按变体自动匹配）
        #[arg(long, alias = "model")]
        model_file: Option<String>,
        /// BFEN 局面串：由该局面重建对局（变体以 BFEN 自带标识为准）
        #[arg(long)]
        fen: Option<String>,
        /// MCTS 模拟次数（MctsOnnx 对手）
        #[arg(long)]
        mcts_sims: Option<usize>,
        /// Expectimax 强引擎节点预算（Engine 对手）
        #[arg(long)]
        engine_budget: Option<u64>,
        /// NNUE 预算模式：nodes / depth_nodes / time
        #[arg(long)]
        nnue_mode: Option<String>,
        /// NNUE 搜索深度（depth_nodes 模式）
        #[arg(long)]
        nnue_depth: Option<i32>,
        /// NNUE 节点预算（nodes / depth_nodes 模式）
        #[arg(long)]
        nnue_budget: Option<u64>,
        /// NNUE 时间预算毫秒（time 模式）
        #[arg(long)]
        nnue_time_ms: Option<u64>,
        /// 搜索硬超时毫秒
        #[arg(long)]
        search_timeout_ms: Option<u64>,
    },
    /// 查看对局完整状态（GET /api/games/:id）
    Show { id: String },
    /// 走子（POST /api/games/:id/move）。ACTION 可以是：
    /// action 索引（如 42），或 from:to / from-to 格式（如 3:7，
    /// 自动在对局合法动作中匹配；翻棋时 from == to）
    Move {
        id: String,
        /// action 索引，或 from:to / from-to
        action: String,
    },
    /// 强制 AI 走子（POST /api/games/:id/ai-move，仅 AI 回合可用）
    AiMove { id: String },
    /// 悔棋（POST /api/games/:id/undo）。STEPS 缺省 = 悔到当前行棋方上次行动前
    Undo {
        id: String,
        /// 显式回退步数（按快照逐帧回退）
        steps: Option<usize>,
    },
    /// 为进行中的对局重绑模型（PUT /api/games/:id/model，同步切换推理后端）
    Bind { id: String, model_file: String },
    /// 删除对局（DELETE /api/games/:id）
    Delete { id: String },
    /// 无状态引擎查询（POST /api/engine/move）：BFEN 局面 → 引擎最优动作，
    /// 不落子、无会话状态（连线程序 / 单步测试用）
    Engine {
        /// 局面 BFEN 串（变体以 BFEN 自带标识为准）
        fen: String,
        /// 模型文件（.onnx / .nnue 均可；缺省按变体自动匹配）
        #[arg(long, alias = "model")]
        model_file: Option<String>,
        /// MCTS 模拟次数（.onnx 模型）
        #[arg(long)]
        mcts_sims: Option<usize>,
        /// NNUE 预算模式：nodes / depth_nodes / time
        #[arg(long)]
        nnue_mode: Option<String>,
        /// NNUE 搜索深度
        #[arg(long)]
        nnue_depth: Option<i32>,
        /// NNUE 节点预算
        #[arg(long)]
        nnue_budget: Option<u64>,
        /// NNUE 时间预算毫秒
        #[arg(long)]
        nnue_time_ms: Option<u64>,
        /// 搜索硬超时毫秒
        #[arg(long)]
        search_timeout_ms: Option<u64>,
    },
    /// MCTS 搜索树查询（树由 AI 回合 / 手动搜索保留在对局会话中）
    Mcts {
        #[command(subcommand)]
        cmd: MctsCmd,
    },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// 读取当前设置
    Get,
    /// 修改设置（缺省字段不修改，只覆盖显式给出的字段）
    Set {
        /// 默认 MCTS 搜索预算（模拟次数）
        #[arg(long)]
        mcts_sims: Option<usize>,
        /// 默认 Expectimax 强引擎节点预算
        #[arg(long)]
        engine_budget: Option<u64>,
        /// 默认 NNUE 预算模式：nodes / depth_nodes / time
        #[arg(long)]
        nnue_mode: Option<String>,
        /// 默认 NNUE 搜索深度
        #[arg(long)]
        nnue_depth: Option<i32>,
        /// 默认 NNUE 节点预算
        #[arg(long)]
        nnue_budget: Option<u64>,
        /// 默认 NNUE 时间预算毫秒
        #[arg(long)]
        nnue_time_ms: Option<u64>,
        /// 默认搜索硬超时毫秒
        #[arg(long)]
        search_timeout_ms: Option<u64>,
    },
}

#[derive(Subcommand)]
enum MctsCmd {
    /// 根节点信息（GET /api/games/:id/mcts/root）
    Root { id: String },
    /// 在当前局面上手动触发一次搜索（POST /api/games/:id/mcts/search，不走子）
    Search { id: String },
    /// 节点详情（GET /api/games/:id/mcts/node/:node）
    Node { id: String, node: usize },
    /// 子节点列表（GET /api/games/:id/mcts/node/:node/children）
    Children { id: String, node: usize },
    /// 节点局面（GET /api/games/:id/mcts/node/:node/board）
    Board { id: String, node: usize },
}

// ============================================================================
// HTTP 客户端
// ============================================================================

/// 薄封装的阻塞式 HTTP 客户端：统一处理地址拼接 / 错误体解析。
struct Client {
    base: String,
    http: reqwest::blocking::Client,
}

impl Client {
    fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::blocking::Client::builder()
                // AI 回合可能耗时长（MCTS / NNUE 搜索），不设客户端超时
                .build()
                .expect("构建 HTTP 客户端失败"),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// 发送请求并解析响应：2xx → JSON（204 → None）；非 2xx → Err（带错误码与消息）。
    fn send(&self, method: &str, path: &str, body: Option<Value>) -> Result<Option<Value>, String> {
        let url = self.url(path);
        let req = match method {
            "GET" => self.http.get(&url),
            "POST" => self.http.post(&url),
            "PUT" => self.http.put(&url),
            "DELETE" => self.http.delete(&url),
            _ => return Err(format!("内部错误：未知 HTTP 方法 {method}")),
        };
        let req = match body {
            Some(b) => req.json(&b),
            None => req,
        };
        let resp = req
            .send()
            .map_err(|e| format!("请求失败（服务器未启动？地址 {url}）: {e}"))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let text = resp.text().map_err(|e| format!("读取响应失败: {e}"))?;
        let parsed: Value = serde_json::from_str(&text)
            .unwrap_or_else(|_| Value::String(format!("非 JSON 响应: {text}")));

        if !status.is_success() {
            let code = parsed
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let message = parsed
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| text.trim());
            return Err(format!("HTTP {status} [{code}] {message}"));
        }
        Ok(Some(parsed))
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        self.send("GET", path, None)?.ok_or_else(|| {
            "内部错误：服务器对 GET 请求返回 204".to_string()
        })
    }

    fn post(&self, path: &str, body: Option<Value>) -> Result<Value, String> {
        self.send("POST", path, body)?
            .ok_or_else(|| "内部错误：服务器对 POST 请求返回 204".to_string())
    }

    fn put(&self, path: &str, body: Value) -> Result<Value, String> {
        self.send("PUT", path, Some(body))?
            .ok_or_else(|| "内部错误：服务器对 PUT 请求返回 204".to_string())
    }
}

// ============================================================================
// 辅助：请求体构建 / action 解析
// ============================================================================

/// Option 序列化辅助：Some(v) → 插入字段，None → 跳过。
macro_rules! set_opt {
    ($obj:expr, $key:literal, $field:expr) => {
        if let Some(v) = $field {
            $obj[$key] = json!(v);
        }
    };
}

fn create_game_body(
    variant: Option<String>,
    opponent: Option<String>,
    ai_player: Option<String>,
    model_file: Option<String>,
    fen: Option<String>,
    mcts_sims: Option<usize>,
    engine_budget: Option<u64>,
    nnue_mode: Option<String>,
    nnue_depth: Option<i32>,
    nnue_budget: Option<u64>,
    nnue_time_ms: Option<u64>,
    search_timeout_ms: Option<u64>,
) -> Value {
    let mut b = json!({});
    set_opt!(b, "variant", variant);
    set_opt!(b, "opponent", opponent);
    set_opt!(b, "aiPlayer", ai_player);
    set_opt!(b, "modelFile", model_file);
    set_opt!(b, "fen", fen);
    set_opt!(b, "mctsSims", mcts_sims);
    set_opt!(b, "engineBudget", engine_budget);
    set_opt!(b, "nnueBudgetMode", nnue_mode);
    set_opt!(b, "nnueDepth", nnue_depth);
    set_opt!(b, "nnueBudget", nnue_budget);
    set_opt!(b, "nnueTimeMs", nnue_time_ms);
    set_opt!(b, "searchTimeoutMs", search_timeout_ms);
    b
}

/// 解析 move 的 ACTION 参数：
/// - 纯数字 → action 索引；
/// - `from:to` / `from-to` → 在对局合法动作中按源/目标格匹配（翻棋 from == to）。
fn resolve_action(cli: &Client, id: &str, action: &str) -> Result<usize, String> {
    if let Ok(idx) = action.parse::<usize>() {
        return Ok(idx);
    }
    let (from_s, to_s) = action
        .split_once(':')
        .or_else(|| action.split_once('-'))
        .ok_or_else(|| {
            format!(
                "非法 action: {action}（应为 action 索引，或 from:to / from-to 格式）"
            )
        })?;
    let from: usize = from_s
        .trim()
        .parse()
        .map_err(|_| format!("非法 from 格位: {from_s}"))?;
    let to: usize = to_s
        .trim()
        .parse()
        .map_err(|_| format!("非法 to 格位: {to_s}"))?;

    let state = cli.get(&format!("/api/games/{id}"))?;
    let legal = state
        .get("legalActions")
        .and_then(Value::as_array)
        .ok_or_else(|| "对局状态缺少 legalActions 字段".to_string())?;
    let matched: Vec<usize> = legal
        .iter()
        .filter(|a| {
            a.get("from").and_then(Value::as_u64) == Some(from as u64)
                && a.get("to").and_then(Value::as_u64) == Some(to as u64)
        })
        .filter_map(|a| a.get("action").and_then(Value::as_u64))
        .map(|v| v as usize)
        .collect();
    match matched.as_slice() {
        [] => Err(format!(
            "格位 {from}→{to} 不在当前合法动作中（共 {} 个合法动作；可先 banqi-cli show {id} 查看）",
            legal.len()
        )),
        [only] => Ok(*only),
        _ => Ok(matched[0]),
    }
}

// ============================================================================
// 入口
// ============================================================================

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli.url, cli.cmd) {
        Ok(out) => {
            if let Some(text) = out {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("错误: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 执行子命令；Some(text) 为要打印的响应文本（pretty JSON）。
fn run(base_url: &str, cmd: Cmd) -> Result<Option<String>, String> {
    let cli = Client::new(base_url);
    let print = |v: Value| -> Result<Option<String>, String> {
        serde_json::to_string_pretty(&v)
            .map(Some)
            .map_err(|e| format!("序列化响应失败: {e}"))
    };

    match cmd {
        Cmd::Health => print(cli.get("/api/health")?),
        Cmd::Variants => print(cli.get("/api/variants")?),
        Cmd::Games => print(cli.get("/api/games")?),
        Cmd::Settings { cmd } => match cmd {
            SettingsCmd::Get => print(cli.get("/api/settings")?),
            SettingsCmd::Set {
                mcts_sims,
                engine_budget,
                nnue_mode,
                nnue_depth,
                nnue_budget,
                nnue_time_ms,
                search_timeout_ms,
            } => {
                let mut b = json!({});
                set_opt!(b, "mctsSims", mcts_sims);
                set_opt!(b, "engineBudget", engine_budget);
                set_opt!(b, "nnueBudgetMode", nnue_mode);
                set_opt!(b, "nnueDepth", nnue_depth);
                set_opt!(b, "nnueBudget", nnue_budget);
                set_opt!(b, "nnueTimeMs", nnue_time_ms);
                set_opt!(b, "searchTimeoutMs", search_timeout_ms);
                print(cli.put("/api/settings", b)?)
            }
        },
        Cmd::New {
            variant,
            opponent,
            ai_player,
            model_file,
            fen,
            mcts_sims,
            engine_budget,
            nnue_mode,
            nnue_depth,
            nnue_budget,
            nnue_time_ms,
            search_timeout_ms,
        } => {
            let body = create_game_body(
                variant,
                opponent,
                ai_player,
                model_file,
                fen,
                mcts_sims,
                engine_budget,
                nnue_mode,
                nnue_depth,
                nnue_budget,
                nnue_time_ms,
                search_timeout_ms,
            );
            print(cli.post("/api/games", Some(body))?)
        }
        Cmd::Show { id } => print(cli.get(&format!("/api/games/{id}"))?),
        Cmd::Move { id, action } => {
            let idx = resolve_action(&cli, &id, &action)?;
            print(cli.post(&format!("/api/games/{id}/move"), Some(json!({ "action": idx })))?)
        }
        Cmd::AiMove { id } => print(cli.post(&format!("/api/games/{id}/ai-move"), None)?),
        Cmd::Undo { id, steps } => {
            let body = match steps {
                Some(s) => json!({ "steps": s }),
                None => json!({}),
            };
            print(cli.post(&format!("/api/games/{id}/undo"), Some(body))?)
        }
        Cmd::Bind { id, model_file } => print(cli.put(
            &format!("/api/games/{id}/model"),
            json!({ "modelFile": model_file }),
        )?),
        Cmd::Delete { id } => {
            // DELETE 返回 204（无响应体）
            cli.send("DELETE", &format!("/api/games/{id}"), None)?;
            println!("对局 {id} 已删除");
            Ok(None)
        }
        Cmd::Engine {
            fen,
            model_file,
            mcts_sims,
            nnue_mode,
            nnue_depth,
            nnue_budget,
            nnue_time_ms,
            search_timeout_ms,
        } => {
            let mut b = json!({ "fen": fen });
            set_opt!(b, "modelFile", model_file);
            set_opt!(b, "mctsSims", mcts_sims);
            set_opt!(b, "nnueBudgetMode", nnue_mode);
            set_opt!(b, "nnueDepth", nnue_depth);
            set_opt!(b, "nnueBudget", nnue_budget);
            set_opt!(b, "nnueTimeMs", nnue_time_ms);
            set_opt!(b, "searchTimeoutMs", search_timeout_ms);
            print(cli.post("/api/engine/move", Some(b))?)
        }
        Cmd::Mcts { cmd } => match cmd {
            MctsCmd::Root { id } => print(cli.get(&format!("/api/games/{id}/mcts/root"))?),
            MctsCmd::Search { id } => {
                print(cli.post(&format!("/api/games/{id}/mcts/search"), None)?)
            }
            MctsCmd::Node { id, node } => {
                print(cli.get(&format!("/api/games/{id}/mcts/node/{node}"))?)
            }
            MctsCmd::Children { id, node } => {
                print(cli.get(&format!("/api/games/{id}/mcts/node/{node}/children"))?)
            }
            MctsCmd::Board { id, node } => {
                print(cli.get(&format!("/api/games/{id}/mcts/node/{node}/board"))?)
            }
        },
    }
}
