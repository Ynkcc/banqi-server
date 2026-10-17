//! 应用状态：模型池（按变体，随用随加载）、对局存储与会话（快照栈 / 版本号 / AI 配置）。
//!
//! 会话不持有具体环境类型：`GameSession` 只存 `Variant` + `PositionSnapshot`
//! （完整快照：棋盘/行棋方/血量/步数/暗子袋/阵亡计数），所有环境操作经
//! `with_env!` 宏重建泛型环境执行 —— server 侧不假设变体种类。

use std::collections::HashMap;
use std::path::Path;
#[cfg(feature = "onnx")]
use std::path::PathBuf;
#[cfg(feature = "onnx")]
use std::sync::Arc;
use std::sync::{Mutex, RwLock};

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
// ONNX 模型池：只提供「文件清单 + 按路径加载缓存」。
//
// 不做任何变体关联：不要求文件命名、不预判模型所属变体、无默认兜底。
// 用户在前端自行选择模型文件并与对局绑定（`GameSession.model_file`）；
// 输入维度不匹配不做预校验，推理时由 ort 忠实返回形状错误。
// ============================================================================

/// 模型池：启动只**扫描**模型目录得到文件清单（不加载权重）；
/// 按文件路径随用随加载并**常驻缓存**，供所有对局复用。
#[cfg(feature = "onnx")]
pub struct Models {
    /// 扫描到的模型文件清单（`rescan` 刷新）
    files: Mutex<Vec<String>>,
    /// 已加载模型缓存（key = 文件路径）。加载持锁串行化：并发首用同一文件只加载一次。
    loaded: Mutex<HashMap<String, Arc<OnnxModel>>>,
    models_dir: PathBuf,
    sessions: usize,
    device: String,
    /// NN 推理缓存条目上限（0 = 关闭；透传给 `OnnxModel::with_sessions_cached`）
    nn_cache_entries: usize,
}

#[cfg(feature = "onnx")]
impl Models {
    /// 扫描 `models_dir` 下全部 `*.onnx`（只记录文件清单，不加载权重）。
    pub fn new(models_dir: &Path, sessions: usize, device: &str, nn_cache_entries: usize) -> Self {
        let models = Self {
            files: Mutex::new(Vec::new()),
            loaded: Mutex::new(HashMap::new()),
            models_dir: models_dir.to_path_buf(),
            sessions,
            device: device.to_string(),
            nn_cache_entries,
        };
        models.rescan();
        models
    }

    fn rescan(&self) {
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        collect_onnx_files(&self.models_dir, 0, &mut paths);
        paths.sort();
        let files: Vec<String> = paths
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let n = files.len();
        *self.files.lock().expect("models files 锁中毒") = files;
        if n == 0 {
            tracing::warn!(
                "模型目录未找到任何 .onnx ({}，含子目录)",
                self.models_dir.display()
            );
        } else {
            tracing::info!(
                "已扫描到 {n} 个模型文件（{}），随用随加载：对局绑定的文件首次使用时才加载权重",
                self.models_dir.display()
            );
        }
    }

    /// 模型文件清单（每次调用重扫，保证前端下拉最新；只读目录，开销可忽略）。
    pub fn files(&self) -> Vec<String> {
        self.rescan();
        self.files.lock().expect("models files 锁中毒").clone()
    }

    /// 随用随加载：按用户选择的文件路径取模型（缓存命中直接返回；
    /// 未加载则构建 ort 会话并常驻缓存。加载持锁串行化，并发首用同一文件只加载一次）。
    /// 模型与变体是否匹配不做预判——形状不匹配由推理时 ort 忠实报错。
    pub fn acquire(&self, file: &str) -> Result<Arc<OnnxModel>, String> {
        let mut loaded = self.loaded.lock().expect("models loaded 锁中毒");
        if let Some(m) = loaded.get(file) {
            return Ok(Arc::clone(m));
        }
        tracing::info!(
            "首次使用，加载模型: {file}（会话池 {}，device {}，nncache {}）",
            self.sessions,
            self.device,
            if self.nn_cache_entries > 0 {
                format!("开启（≤{} 条）", self.nn_cache_entries)
            } else {
                "关闭".to_string()
            }
        );
        let model = Arc::new(OnnxModel::with_sessions_cached(
            file,
            &self.device,
            self.sessions,
            self.nn_cache_entries,
        )?);
        loaded.insert(file.to_string(), Arc::clone(&model));
        Ok(model)
    }

    /// 模型文件清单与加载状态（健康检查用）：`(文件路径, 是否已加载)`。
    pub fn loaded_files(&self) -> Vec<(String, bool)> {
        self.files();
        let loaded = self.loaded.lock().expect("models loaded 锁中毒");
        self.files
            .lock()
            .expect("models files 锁中毒")
            .iter()
            .map(|f| (f.clone(), loaded.contains_key(f)))
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

/// 无 onnx feature 时的空模型池：AI 不可用，人人对弈仍可正常使用。
#[cfg(not(feature = "onnx"))]
pub struct Models;

#[cfg(not(feature = "onnx"))]
impl Models {
    pub fn new(_models_dir: &Path, _sessions: usize, _device: &str, _nn_cache_entries: usize) -> Self {
        tracing::warn!("本次编译未启用 onnx feature，AI 对手不可用");
        Self
    }
    pub fn files(&self) -> Vec<String> {
        Vec::new()
    }
    pub fn loaded_files(&self) -> Vec<(String, bool)> {
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
    /// 每次决策的 MCTS 模拟次数（创建对局时快照固定；
    /// 未显式指定时取服务器当前默认值）
    pub mcts_sims: usize,
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
    /// 与对局绑定的模型文件（用户显式选择，建局必需）。
    /// AI 回合与手动搜索均使用该文件（随用随加载，常驻缓存）。
    pub model_file: Option<String>,
    /// 最近一次 MCTS 搜索保留的树（AI 回合或手动搜索时替换；仅查询用）
    pub mcts_tree: Option<crate::mcts::MctsTreeBox>,
}

impl GameSession {
    pub fn new(variant: Variant, ai: Option<AiConfig>, model_file: Option<String>) -> Self {
        Self {
            variant,
            current: variant::initial_snapshot(variant),
            history: MoveHistory::new(),
            version: 0,
            ai,
            model_file,
            mcts_tree: None,
        }
    }

    /// 由既有快照构建会话（BFEN 粘贴重建等场景；走子历史从空开始）。
    pub fn from_snapshot(
        variant: Variant,
        snapshot: PositionSnapshot,
        ai: Option<AiConfig>,
        model_file: Option<String>,
    ) -> Self {
        Self {
            variant,
            current: snapshot,
            history: MoveHistory::new(),
            version: 0,
            ai,
            model_file,
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
            fen: snap.to_fen(),
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
    /// 初始值取 `config.default_sims`，经 `PUT /api/settings` 在线修改。
    /// 仅作为创建对局时未显式指定 `mctsSims` 的取值；已建对局快照固定。
    pub default_sims: std::sync::atomic::AtomicUsize,
    pub models: Models,
    /// 对局存储（std RwLock 即可：所有持锁段均为同步 CPU 操作，不跨 await）
    pub games: RwLock<HashMap<Uuid, GameSession>>,
}

/// 路由层共享的 `Arc<AppState>` 别名（统一在此定义，避免各 handler 重复声明）。
pub type SharedState = std::sync::Arc<AppState>;

impl AppState {
    pub fn new(config: ServerConfig) -> Self {
        let models = Models::new(
            &config.models_dir,
            config.sessions,
            &config.device,
            config.nn_cache_entries,
        );
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
    /// 只影响之后创建的对局（已建对局的模拟次数在建局时快照固定）。
    pub fn set_default_sims(&self, sims: usize) -> usize {
        let v = sims.max(1);
        self.default_sims.store(v, std::sync::atomic::Ordering::Relaxed);
        v
    }

    /// 创建对局的默认变体：--default-variant > 首个变体。
    pub fn default_variant(&self) -> Variant {
        self.config.default_variant.unwrap_or(Variant::ALL[0])
    }

    /// 随用随加载：按对局绑定的模型文件取模型（缓存命中直接返回，
    /// 首次使用时加载权重）。加载（构建 ort 会话）在 blocking 线程池执行，
    /// 避免阻塞异步运行时；加载后常驻缓存，供所有对局复用。
    #[cfg(feature = "onnx")]
    pub async fn acquire_model(self: &Arc<Self>, model_file: String) -> Result<Arc<OnnxModel>, String> {
        let app = Arc::clone(self);
        tokio::task::spawn_blocking(move || app.models.acquire(&model_file))
            .await
            .map_err(|e| format!("模型加载任务失败: {e}"))?
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

    #[test]
    fn set_default_sims_clamps_to_at_least_one() {
        let app = test_app();
        assert_eq!(app.set_default_sims(0), 1);
        assert_eq!(app.default_sims(), 1);
        // 默认变体不依赖模型扫描
        assert_eq!(app.default_variant(), Variant::ALL[0]);
    }
}
