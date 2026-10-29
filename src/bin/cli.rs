use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use std::process::ExitCode;
#[derive(Parser)]
#[command(
    name = "banqi-cli",
    version,
    about = "banqi-server 命令行客户端：REST API 的薄封装（LLM / 脚本测试用，替代 curl）"
)]
struct Cli {
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
    Health,
    Variants,
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    Games,
    New {
        #[arg(long)]
        variant: Option<String>,
        #[arg(long)]
        red: Option<String>,
        #[arg(long)]
        black: Option<String>,
        #[arg(long, alias = "model")]
        model_file: Option<String>,
        #[arg(long)]
        fen: Option<String>,
        #[command(flatten)]
        budget: BudgetOpts,
    },
    Show {
        id: String,
    },
    Move {
        id: String,
        action: String,
    },
    Run {
        id: String,
    },
    DryRun {
        id: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, alias = "model")]
        model_file: Option<String>,
    },
    Undo {
        id: String,
        steps: Option<usize>,
    },
    Slots {
        id: String,
        #[arg(long)]
        red: Option<String>,
        #[arg(long)]
        black: Option<String>,
        #[arg(long, alias = "model")]
        model_file: Option<String>,
    },
    Delete {
        id: String,
    },
    Engine {
        fen: String,
        #[arg(long, alias = "model")]
        model_file: Option<String>,
        #[command(flatten)]
        budget: BudgetOpts,
    },
    Mcts {
        #[command(subcommand)]
        cmd: MctsCmd,
    },
}
#[derive(Subcommand)]
enum SettingsCmd {
    Get,
    Set {
        #[command(flatten)]
        budget: BudgetOpts,
    },
}
#[derive(Subcommand)]
enum MctsCmd {
    Root { id: String },
    Search { id: String },
    Node { id: String, node: usize },
    Children { id: String, node: usize },
    Board { id: String, node: usize },
}
macro_rules! set_opt {
    ($obj:expr, $key:literal, $field:expr) => {
        if let Some(v) = $field {
            $obj[$key] = json!(v);
        }
    };
}

#[derive(Args, Default)]
struct BudgetOpts {
    #[arg(long)]
    mcts_sims: Option<usize>,
    #[arg(long)]
    engine_budget: Option<u64>,
    #[arg(long)]
    nnue_mode: Option<String>,
    #[arg(long)]
    nnue_depth: Option<i32>,
    #[arg(long)]
    nnue_budget: Option<u64>,
    #[arg(long)]
    nnue_time_ms: Option<u64>,
    #[arg(long)]
    search_timeout_ms: Option<u64>,
}
impl BudgetOpts {
    fn apply(&self, obj: &mut Value) {
        set_opt!(obj, "mctsSims", self.mcts_sims);
        set_opt!(obj, "engineBudget", self.engine_budget);
        set_opt!(obj, "nnueBudgetMode", &self.nnue_mode);
        set_opt!(obj, "nnueDepth", self.nnue_depth);
        set_opt!(obj, "nnueBudget", self.nnue_budget);
        set_opt!(obj, "nnueTimeMs", self.nnue_time_ms);
        set_opt!(obj, "searchTimeoutMs", self.search_timeout_ms);
    }
}
struct Client {
    base: String,
    http: reqwest::blocking::Client,
}
impl Client {
    fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::blocking::Client::builder()
                .build()
                .expect("构建 HTTP 客户端失败"),
        }
    }
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }
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
        self.send("GET", path, None)?
            .ok_or_else(|| "内部错误：服务器对 GET 请求返回 204".to_string())
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
fn slot_body(kind: &Option<String>, model_file: &Option<String>, budget: &BudgetOpts) -> Value {
    let Some(kind) = kind.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return json!({ "kind": "Human" });
    };
    let mut s = json!({ "kind": kind });
    if !kind.eq_ignore_ascii_case("human") {
        if let Some(m) = model_file.as_deref().filter(|s| !s.is_empty()) {
            s["modelFile"] = json!(m);
        }
        let mut p = json!({});
        budget.apply(&mut p);
        if p.as_object().is_some_and(|o| !o.is_empty()) {
            s["params"] = p;
        }
    }
    s
}
fn slots_body(
    red: &Option<String>,
    black: &Option<String>,
    model_file: &Option<String>,
    budget: &BudgetOpts,
) -> Value {
    let mut b = json!({});
    let red_slot = red.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let black_slot = black.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if red_slot.is_some() || black_slot.is_some() {
        b["slots"] = json!({});
        if let Some(red) = red_slot {
            b["slots"]["red"] = slot_body(&Some(red.to_string()), model_file, budget);
        }
        if let Some(black) = black_slot {
            b["slots"]["black"] = slot_body(&Some(black.to_string()), model_file, budget);
        }
    }
    b
}
fn resolve_action(cli: &Client, id: &str, action: &str) -> Result<usize, String> {
    if let Ok(idx) = action.parse::<usize>() {
        return Ok(idx);
    }
    let (from_s, to_s) = action
        .split_once(':')
        .or_else(|| action.split_once('-'))
        .ok_or_else(|| {
            format!("非法 action: {action}（应为 action 索引，或 from:to / from-to 格式）")
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
        .get("legal_actions")
        .and_then(Value::as_array)
        .ok_or_else(|| "对局状态缺少 legal_actions 字段".to_string())?;
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
            SettingsCmd::Set { budget } => {
                let mut b = json!({});
                budget.apply(&mut b);
                print(cli.put("/api/settings", b)?)
            }
        },
        Cmd::New {
            variant,
            red,
            black,
            model_file,
            fen,
            budget,
        } => {
            let mut body = slots_body(&red, &black, &model_file, &budget);
            set_opt!(body, "variant", variant);
            set_opt!(body, "fen", fen);
            print(cli.post("/api/games", Some(body))?)
        }
        Cmd::Show { id } => print(cli.get(&format!("/api/games/{id}"))?),
        Cmd::Move { id, action } => {
            let idx = resolve_action(&cli, &id, &action)?;
            print(cli.post(
                &format!("/api/games/{id}/move"),
                Some(json!({ "action": idx })),
            )?)
        }
        Cmd::Run { id } => print(cli.post(&format!("/api/games/{id}/run"), None)?),
        Cmd::DryRun {
            id,
            kind,
            model_file,
        } => {
            let mut b = json!({});
            set_opt!(b, "kind", kind);
            set_opt!(b, "modelFile", model_file);
            print(cli.post(&format!("/api/games/{id}/dry-run"), Some(b))?)
        }
        Cmd::Undo { id, steps } => {
            let body = match steps {
                Some(s) => json!({ "steps": s }),
                None => json!({}),
            };
            print(cli.post(&format!("/api/games/{id}/undo"), Some(body))?)
        }
        Cmd::Slots {
            id,
            red,
            black,
            model_file,
        } => print(cli.put(
            &format!("/api/games/{id}/slots"),
            slots_body(&red, &black, &model_file, &BudgetOpts::default()),
        )?),
        Cmd::Delete { id } => {
            cli.send("DELETE", &format!("/api/games/{id}"), None)?;
            println!("对局 {id} 已删除");
            Ok(None)
        }
        Cmd::Engine {
            fen,
            model_file,
            budget,
        } => {
            let mut b = json!({ "fen": fen });
            set_opt!(b, "modelFile", model_file);
            budget.apply(&mut b);
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
