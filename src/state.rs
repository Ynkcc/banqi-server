//! 应用状态：模型池（按变体）、对局存储与会话（快照栈 / 版本号 / AI 配置）。
//!
//! 会话不持有具体环境类型：`GameSession` 只存 `Variant` + `PositionSnapshot`
//! （完整快照：棋盘/行棋方/血量/步数/暗子袋/阵亡计数），所有环境操作经
//! `with_env!` 宏重建泛型环境执行 —— server 侧不假设变体种类。

use std::collections::HashMap;
use std::path::Path;
#[cfg(feature = "onnx")]
use std::sync::Arc;
use std::sync::RwLock;

use uuid::Uuid;

use banqi_core::core::env::error::EnvError;
use banqi_core::core::env::types::Player;
use banqi_core::core::env::{GameEnv, MoveHistory, MoveRecord, PositionSnapshot, SnapshotEnv, Variant};
#[cfg(feature = "onnx")]
use banqi_engine::inference::onnx::OnnxModel;

use crate::config::ServerConfig;
use crate::dto::{
    ActionView, AiInfoView, CellView, GameStateResp, HistoryMoveView, PieceView, piece_type_str,
    player_str, winner_str,
};
use crate::error::ApiError;
use crate::variant;

// ============================================================================
// ONNX 模型池（启动时动态扫描 models 目录，按输入维度自动关联变体）
// ============================================================================

/// 模型输入静态维度（board 尾三维 `[C,H,W]` 与 scalars 尾一维 `[S]`；
/// `None` = 动态维度，匹配时视为通配）。
#[cfg(feature = "onnx")]
#[derive(Clone, Copy, Debug)]
pub struct ModelDims {
    pub board: [Option<usize>; 3],
    pub scalars: Option<usize>,
}

#[cfg(feature = "onnx")]
impl ModelDims {
    /// 是否与变体特征维度匹配（board[B,C,H,W] + scalars[B,S]，batch 维不参与）。
    fn matches_variant(&self, variant: Variant) -> bool {
        let (c, r, w, s) = variant::feature_dims(variant);
        let ok = |got: Option<usize>, want: usize| got.is_none_or(|g| g == want);
        ok(self.board[0], c)
            && ok(self.board[1], r)
            && ok(self.board[2], w)
            && ok(self.scalars, s)
    }
}

/// 已加载的单个模型文件。
#[cfg(feature = "onnx")]
pub struct LoadedModel {
    pub file: String,
    /// 文件名 stem（仅作为同维度多模型时的优先提示，不做变体 ID 校验）
    pub stem: String,
    pub model: Arc<OnnxModel>,
    /// 输入静态维度（元数据读取失败时为 None，此时该模型无法按维度关联变体）
    pub dims: Option<ModelDims>,
}

/// 已加载模型池。
#[cfg(feature = "onnx")]
#[derive(Default)]
pub struct Models {
    inner: Vec<LoadedModel>,
}

#[cfg(feature = "onnx")]
impl Models {
    /// 扫描 `models_dir` 下全部 `*.onnx` 并加载。**不要求文件名是变体 ID**：
    /// 模型与变体的关联按输入维度自动匹配（`ModelDims::matches_variant`），
    /// 文件名仅在同维度多模型时作为优先提示。
    ///
    /// 单个文件加载失败只告警不阻塞启动。
    pub fn load(models_dir: &Path, sessions: usize, device: &str) -> Self {
        let mut models = Self::default();
        // 按文件名排序，保证多模型同维度时的匹配顺序确定
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        collect_onnx_files(models_dir, 0, &mut paths);
        if paths.is_empty() {
            tracing::warn!(
                "模型目录未找到任何 .onnx ({}，含子目录)，所有变体 AI 均不可用",
                models_dir.display()
            );
        }
        paths.sort();

        for path in paths {
            let file = path.to_string_lossy().to_string();
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            match OnnxModel::with_sessions(&file, device, sessions) {
                Ok(model) => {
                    let dims = read_input_dims(&file);
                    match dims.as_ref() {
                        Some(d) => {
                            let matched: Vec<&str> = Variant::ALL
                                .iter()
                                .copied()
                                .filter(|v| d.matches_variant(*v))
                                .map(|v| v.as_str())
                                .collect();
                            tracing::info!(
                                "已加载 ONNX 模型: {file}（匹配变体: [{}]，会话池 {sessions}，device={device}）",
                                if matched.is_empty() { "无".to_string() } else { matched.join(", ") }
                            );
                            if matched.is_empty() {
                                tracing::warn!("模型 {file} 的输入维度与任何变体都不匹配，AI 不会使用它");
                            }
                        }
                        None => {
                            tracing::info!(
                                "已加载 ONNX 模型: {file}（输入维度不可静态解析，仅同名文件可关联变体）"
                            );
                        }
                    }
                    models.inner.push(LoadedModel {
                        file,
                        stem,
                        model: Arc::new(model),
                        dims,
                    });
                }
                Err(e) => {
                    tracing::error!("加载 ONNX 模型失败 ({file}): {e}");
                }
            }
        }
        models
    }

    /// 取变体可用的模型：优先文件名与变体 ID 相同者，否则取第一个
    /// 输入维度匹配的模型。
    pub fn get(&self, variant: Variant) -> Option<&Arc<OnnxModel>> {
        self.inner
            .iter()
            .find(|m| m.stem == variant.as_str())
            .or_else(|| {
                self.inner
                    .iter()
                    .find(|m| m.dims.is_some_and(|d| d.matches_variant(variant)))
            })
            .map(|m| &m.model)
    }

    pub fn available(&self, variant: Variant) -> bool {
        self.get(variant).is_some()
    }

    /// 变体是否有可用 AI 模型（无 onnx feature 时恒为 false）。
    pub fn loaded_variants(&self) -> Vec<Variant> {
        Variant::ALL
            .iter()
            .copied()
            .filter(|v| self.available(*v))
            .collect()
    }

    /// 已加载文件清单（健康检查用）：`(文件路径, stem, 匹配的变体 ID 列表)`。
    pub fn loaded_files(&self) -> Vec<(String, String, Vec<String>)> {
        self.inner
            .iter()
            .map(|m| {
                let variants = match m.dims {
                    Some(d) => Variant::ALL
                        .iter()
                        .copied()
                        .filter(|v| d.matches_variant(*v))
                        .map(|v| v.as_str().to_string())
                        .collect(),
                    None => Vec::new(),
                };
                (m.file.clone(), m.stem.clone(), variants)
            })
            .collect()
    }
}

/// 递归收集目录下的 `*.onnx`（深度限制 4 层，与 banqi-gui 的模型扫描口径一致）。
#[cfg(feature = "onnx")]
fn collect_onnx_files(dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        if depth == 0 {
            tracing::warn!("模型目录不可读 ({}), 所有变体 AI 均不可用", dir.display());
        }
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        match e.file_type() {
            Ok(ft) if ft.is_dir() => collect_onnx_files(&path, depth + 1, out),
            Ok(ft) if ft.is_file() && path.extension().and_then(|x| x.to_str()) == Some("onnx") => {
                out.push(path);
            }
            _ => {}
        }
    }
}

/// 读取模型输入的静态维度（board[B,C,H,W] + scalars[B,S]）。
///
/// OnnxModel 不暴露 Session 元数据，这里用路径开一个临时单会话读取；
/// 结构不符（缺输入 / 秩不对 / 非张量）返回 None。
#[cfg(feature = "onnx")]
fn read_input_dims(path: &str) -> Option<ModelDims> {
    let session = ort::session::Session::builder()
        .ok()?
        .commit_from_file(path)
        .ok()?;

    let inputs = session.inputs();
    let tensor_dims = |name: &str| -> Option<Vec<Option<usize>>> {
        let outlet = inputs.iter().find(|i| i.name() == name)?;
        match outlet.dtype() {
            ort::value::ValueType::Tensor { shape, .. } => Some(
                shape
                    .iter()
                    .map(|&d| (d >= 0).then_some(d as usize))
                    .collect(),
            ),
            _ => None,
        }
    };

    let board = tensor_dims("board")?;
    if board.len() != 4 {
        return None;
    }
    let scalars = tensor_dims("scalars")?;
    if scalars.len() != 2 {
        return None;
    }
    Some(ModelDims {
        board: [board[1], board[2], board[3]],
        scalars: scalars[1],
    })
}

/// 无 onnx feature 时的空模型池：AI 不可用，人人对弈仍可正常使用。
#[cfg(not(feature = "onnx"))]
#[derive(Default)]
pub struct Models;

#[cfg(not(feature = "onnx"))]
impl Models {
    pub fn load(_models_dir: &Path, _sessions: usize, _device: &str) -> Self {
        tracing::warn!("本次编译未启用 onnx feature，AI 对手不可用");
        Self
    }
    pub fn available(&self, _variant: Variant) -> bool {
        false
    }
    pub fn loaded_variants(&self) -> Vec<Variant> {
        Vec::new()
    }
    pub fn loaded_files(&self) -> Vec<(String, String, Vec<String>)> {
        Vec::new()
    }
}

// ============================================================================
// 对局会话
// ============================================================================

/// AI 对手配置。
#[derive(Clone, Copy, Debug)]
pub struct AiConfig {
    /// AI 执子方
    pub player: Player,
    /// 每次决策的 MCTS 模拟次数。
    /// `follows_default = true` 时跟随服务器默认（`PUT /api/settings` 实时
    /// 同步到进行中的对局）；创建请求显式指定 `mctsSims` 后固定不变。
    pub mcts_sims: usize,
    /// 是否跟随服务器默认搜索预算（创建对局未显式指定 `mctsSims` 时为 true）
    pub follows_default: bool,
}

/// 单局状态：变体 + 当前快照 + 快照栈（悔棋）+ AI 配置 + 版本号。
pub struct GameSession {
    pub variant: Variant,
    /// 当前局面完整快照
    pub current: PositionSnapshot,
    /// 走子前的快照栈（悔棋；快照为棋盘常量级开销）
    pub history: MoveHistory,
    /// 单调递增版本号：任何状态变更（走子/悔棋）+1；
    /// AI 异步决策完成后校验版本号，不一致即丢弃结果。
    pub version: u64,
    pub ai: Option<AiConfig>,
    /// 最近一次 MCTS 搜索保留的树（AI 回合或手动搜索时替换；仅查询用）
    pub mcts_tree: Option<crate::mcts::MctsTreeBox>,
}

impl GameSession {
    pub fn new(variant: Variant, ai: Option<AiConfig>) -> Self {
        Self {
            variant,
            current: variant::initial_snapshot(variant),
            history: MoveHistory::new(),
            version: 0,
            ai,
            mcts_tree: None,
        }
    }

    /// 终局判定（经 with_env 重建环境检查）。
    fn is_over(&self) -> bool {
        let (terminated, truncated, _) = self.over_state();
        terminated || truncated
    }

    /// `(terminated, truncated, winner)`。
    pub fn over_state(&self) -> (bool, bool, Option<i32>) {
        with_env!(self.variant, &self.current, env => variant::game_over(&env))
            .expect("快照由本服务生成，重建不应失败")
    }

    /// 应用动作（校验终局与合法性；维护快照栈、决策历史与版本号）。
    pub fn apply_action(&mut self, action: usize) -> Result<variant::StepOutcome, ApiError> {
        if self.is_over() {
            return Err(ApiError::GameOver);
        }
        let legal = self.legal_actions();
        if !legal.contains(&action) {
            return Err(ApiError::IllegalAction { action });
        }
        let prev = self.current.clone();
        self.history.push(prev.clone());
        // 合法性已预检，step 失败只可能是内部不变量问题
        let (outcome, snap, revealed) = with_env!(self.variant, &self.current, env => {
            let o = variant::step(&mut env, action).expect("动作已通过合法性与终局预检");
            // 本步实际翻出的棋子（翻棋/吃暗子的真实结果，快照差分推不出）
            let revealed = GameEnv::last_revealed(&env);
            (o, env.to_snapshot(), revealed)
        })
        .map_err(|e: EnvError| ApiError::Internal(format!("重建环境失败: {e}")))?;
        self.history.record(MoveRecord::from_step(&prev, &snap, action, revealed));
        self.current = snap;
        self.version += 1;
        Ok(outcome)
    }

    /// 悔棋：按快照逐帧回退 `steps` 步（对局结束后不可悔棋）。
    pub fn undo(&mut self, steps: usize) -> Result<(), ApiError> {
        if self.is_over() {
            return Err(ApiError::GameOver);
        }
        if steps == 0 {
            return Err(ApiError::BadRequest("steps 必须至少为 1".to_string()));
        }
        let snap = self.history.undo_steps(steps).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "可回退步数不足: 请求 {steps}，当前仅 {} 步历史",
                self.history.len()
            ))
        })?;
        self.current = snap;
        self.version += 1;
        Ok(())
    }

    /// 悔棋到当前行棋方上次行动前：回退快照直至回到「当前行棋方即将行动」的局面。
    ///
    /// AI 对局中（AI 走完后轮到人类）会一并回退 AI 与人类各一步；
    /// PvP 中即回退最近一步。当前玩家尚未行动过时返回错误。
    pub fn undo_to_current_player(&mut self) -> Result<(), ApiError> {
        if self.is_over() {
            return Err(ApiError::GameOver);
        }
        let target = self.current.current_player;
        let snap = self.history.undo_to_player_turn(target).ok_or_else(|| {
            ApiError::BadRequest("没有可悔的棋步（当前玩家尚未行动过）".to_string())
        })?;
        self.current = snap;
        self.version += 1;
        Ok(())
    }

    /// 当前合法动作列表。
    pub fn legal_actions(&self) -> Vec<usize> {
        with_env!(self.variant, &self.current, env => variant::legal_actions(&env))
            .unwrap_or_default()
    }

    /// 单个动作的结构化视图（坐标 + 机会标记）。
    pub fn action_view(&self, action: usize) -> ActionView {
        let coords = variant::coords_for_action(self.variant, action).unwrap_or_default();
        ActionView {
            action,
            from: coords.first().copied().unwrap_or(action),
            to: coords.last().copied().unwrap_or(action),
            is_chance: with_env!(self.variant, &self.current, env => {
                GameEnv::is_chance_action(&env, action)
            })
            .unwrap_or(false),
        }
    }

    /// 当前是否轮到 AI 行棋。
    pub fn ai_to_move(&self) -> bool {
        match self.ai {
            Some(ai) => {
                let (terminated, truncated, _) = self.over_state();
                !terminated && !truncated && self.current.current_player == ai.player
            }
            None => false,
        }
    }

    /// 当前是否可悔棋：对局未结束，且历史中存在「当前行棋方上次行动前」的快照
    ///（悔棋总是回到当前行棋方自己上次行动前，可连续触发以反悔所有自己的行动）。
    pub fn undoable(&self) -> bool {
        !self.is_over() && self.history.undoable(self.current.current_player)
    }

    /// 当前完整状态视图（快照字段 + config 推导 + 泛型环境求合法动作）。
    pub fn view(&self, game_id: Uuid) -> GameStateResp {
        let snap = &self.current;
        let cfg = self.variant.config();
        let (terminated, truncated, winner) = self.over_state();

        // 棋盘视图（行优先；暗子不泄露真实身份）
        let board: Vec<CellView> = snap
            .slots
            .iter()
            .enumerate()
            .map(|(index, slot)| {
                use banqi_core::core::env::Slot::*;
                let (state, piece) = match slot {
                    Empty => ("empty", None),
                    Hidden => ("hidden", None),
                    Revealed(p) => (
                        "revealed",
                        Some(PieceView {
                            piece_type: piece_type_str(p.piece_type),
                            side: player_str(p.player),
                        }),
                    ),
                };
                CellView {
                    index,
                    row: index / cfg.cols,
                    col: index % cfg.cols,
                    state: state.to_string(),
                    piece,
                }
            })
            .collect();

        // 合法动作视图（坐标经 action_lookup_tables 公开 API，与具体环境类型无关）
        let legal_actions: Vec<ActionView> = self
            .legal_actions()
            .into_iter()
            .map(|action| self.action_view(action))
            .collect();

        // 阵亡 / 隐藏棋子列表（按型展开；hidden 计数可由公开信息推导，不泄露暗子身份）
        use banqi_core::core::env::types::PieceType;
        let piece_type_names: [String; 7] = [
            PieceType::Soldier,
            PieceType::Cannon,
            PieceType::Horse,
            PieceType::Chariot,
            PieceType::Elephant,
            PieceType::Advisor,
            PieceType::General,
        ]
        .map(piece_type_str);
        let mut dead_red: Vec<String> = Vec::new();
        let mut dead_black: Vec<String> = Vec::new();
        for (player_idx, out) in [(0usize, &mut dead_red), (1, &mut dead_black)] {
            for t in 0..7 {
                for _ in 0..snap.dead_counts[player_idx][t] {
                    out.push(piece_type_names[t].clone());
                }
            }
        }
        let hidden_red: Vec<String> = snap
            .hidden_bag
            .iter()
            .filter(|p| p.player == banqi_core::core::env::types::Player::Red)
            .map(|p| piece_type_str(p.piece_type))
            .collect();
        let hidden_black: Vec<String> = snap
            .hidden_bag
            .iter()
            .filter(|p| p.player == banqi_core::core::env::types::Player::Black)
            .map(|p| piece_type_str(p.piece_type))
            .collect();

        let over = terminated || truncated;
        let over_reason = if !over {
            None
        } else if matches!(winner, Some(1) | Some(-1)) {
            Some("checkmate".to_string())
        } else if truncated {
            Some("max_steps".to_string())
        } else {
            Some("draw".to_string())
        };

        let ai_view = match self.ai {
            Some(ai) => AiInfoView {
                enabled: true,
                player: Some(player_str(ai.player)),
                mcts_sims: ai.mcts_sims,
            },
            None => AiInfoView {
                enabled: false,
                player: None,
                mcts_sims: 0,
            },
        };

        // 决策历史（棋谱）：机会动作附实际翻出的棋子
        let history: Vec<HistoryMoveView> = self
            .history
            .records()
            .iter()
            .enumerate()
            .map(|(i, rec)| HistoryMoveView {
                ply: i + 1,
                player: player_str(rec.player),
                action: rec.action,
                from: rec.from,
                to: rec.to,
                is_flip: rec.is_flip,
                is_chance: rec.is_chance,
                is_capture: rec.is_capture,
                revealed: rec
                    .revealed
                    .map(|p| PieceView {
                        piece_type: piece_type_str(p.piece_type),
                        side: player_str(p.player),
                    }),
                captured: rec
                    .captured
                    .map(|p| PieceView {
                        piece_type: piece_type_str(p.piece_type),
                        side: player_str(p.player),
                    }),
            })
            .collect();

        GameStateResp {
            game_id: game_id.to_string(),
            variant: self.variant.as_str().to_string(),
            version: self.version,
            current_player: player_str(snap.current_player),
            move_count: snap.move_counter as usize,
            total_steps: snap.total_step_counter as usize,
            max_steps: cfg.max_steps_per_episode,
            red_score: snap.scores[0],
            black_score: snap.scores[1],
            red_hp: snap.scores[0],
            black_hp: snap.scores[1],
            over,
            terminated,
            truncated,
            over_reason,
            winner: winner_str(winner),
            last_action: (snap.last_action >= 0).then_some(snap.last_action as usize),
            board,
            dead_red,
            dead_black,
            hidden_red,
            hidden_black,
            legal_actions,
            history,
            ai: ai_view,
            ai_to_move: self.ai_to_move(),
            undoable: self.undoable(),
        }
    }

    /// 摘要视图（列表用）。
    pub fn summary(&self, game_id: Uuid) -> crate::dto::GameSummary {
        let (_, _, winner) = self.over_state();
        let v = self.view(game_id);
        crate::dto::GameSummary {
            game_id: game_id.to_string(),
            variant: self.variant.as_str().to_string(),
            version: self.version,
            current_player: v.current_player,
            total_steps: v.total_steps,
            over: v.over,
            winner: winner_str(winner),
            ai: v.ai,
        }
    }
}

// ============================================================================
// 应用状态
// ============================================================================

/// axum 共享状态。
pub struct AppState {
    pub config: ServerConfig,
    /// 运行时可调的默认 MCTS 搜索预算（模拟次数）；
    /// 初始值取 `config.default_sims`，经 `PUT /api/settings` 在线修改：
    /// 之后创建的对局取新值；进行中的「跟随默认」对局实时同步
    /// （显式指定 `mctsSims` 的对局不受影响）。
    pub default_sims: std::sync::atomic::AtomicUsize,
    pub models: Models,
    /// 对局存储（std RwLock 即可：所有持锁段均为同步 CPU 操作，不跨 await）
    pub games: RwLock<HashMap<Uuid, GameSession>>,
}

/// 路由层共享的 `Arc<AppState>` 别名（统一在此定义，避免各 handler 重复声明）。
pub type SharedState = std::sync::Arc<AppState>;

impl AppState {
    pub fn new(config: ServerConfig) -> Self {
        let models = Models::load(&config.models_dir, config.sessions, &config.device);
        Self {
            default_sims: std::sync::atomic::AtomicUsize::new(config.default_sims.max(1)),
            config,
            models,
            games: RwLock::new(HashMap::new()),
        }
    }

    /// 当前默认 MCTS 搜索预算（原子读取，钳制到至少 1）。
    pub fn default_sims(&self) -> usize {
        self.default_sims
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(1)
    }

    /// 运行时调整默认 MCTS 搜索预算，返回钳制后的生效值。
    ///
    /// 同步所有「跟随默认」的对局（创建时未显式指定 `mctsSims`），
    /// 使运行时调整对**进行中的对局**实时生效；显式指定的对局不受影响。
    pub fn set_default_sims(&self, sims: usize) -> usize {
        let v = sims.max(1);
        self.default_sims.store(v, std::sync::atomic::Ordering::Relaxed);
        let mut games = self.games.write().expect("games 锁中毒");
        for session in games.values_mut() {
            if let Some(ai) = &mut session.ai
                && ai.follows_default
            {
                ai.mcts_sims = v;
            }
        }
        v
    }

    /// 创建对局的默认变体：--default-variant > 首个有模型者 > 首个变体。
    pub fn default_variant(&self) -> Variant {
        if let Some(v) = self.config.default_variant {
            return v;
        }
        self.models
            .loaded_variants()
            .first()
            .copied()
            .unwrap_or(Variant::ALL[0])
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use banqi_core::core::env::Variant;

    fn test_app() -> AppState {
        // models_dir 指向不存在的临时路径：模型池为空（仅告警），测试不加载 ONNX
        let cfg = ServerConfig {
            models_dir: std::env::temp_dir().join("banqi-server-test-no-models"),
            ..Default::default()
        };
        AppState::new(cfg)
    }

    fn session_with_ai(mcts_sims: usize, follows_default: bool) -> GameSession {
        GameSession::new(
            Variant::DarkChess4x8,
            Some(AiConfig {
                player: Player::Black,
                mcts_sims,
                follows_default,
            }),
        )
    }

    #[test]
    fn set_default_sims_syncs_follow_default_games_only() {
        let app = test_app();
        let id_follow = Uuid::new_v4();
        let id_explicit = Uuid::new_v4();
        {
            let mut games = app.games.write().expect("games 锁中毒");
            games.insert(id_follow, session_with_ai(200, true));
            games.insert(id_explicit, session_with_ai(77, false));
        }

        let applied = app.set_default_sims(400);
        assert_eq!(applied, 400);
        assert_eq!(app.default_sims(), 400);

        let games = app.games.read().expect("games 锁中毒");
        assert_eq!(
            games[&id_follow].ai.as_ref().unwrap().mcts_sims,
            400,
            "跟随默认的对局应实时同步新预算"
        );
        assert!(
            games[&id_follow].ai.as_ref().unwrap().follows_default,
            "跟随标记不得被同步修改"
        );
        assert_eq!(
            games[&id_explicit].ai.as_ref().unwrap().mcts_sims,
            77,
            "显式指定的对局不受运行时调整影响"
        );
    }

    #[test]
    fn set_default_sims_clamps_to_at_least_one() {
        let app = test_app();
        assert_eq!(app.set_default_sims(0), 1);
        assert_eq!(app.default_sims(), 1);
    }
}
