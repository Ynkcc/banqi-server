//! 快照式对局会话核心（共享）：变体 + 当前快照 + 走子历史 + 版本号。
//!
//! 收敛 banqi-gui/src/game_cmds.rs（`push_history` / `record_move` / 失败回滚
//! `pop` 的手工不变量）与 banqi-server/src/state.rs（`GameSession` 的
//! `apply_action` / `undo*`）的重复实现：
//!
//! - 「压快照 → step → 记录 `MoveRecord`（含 `last_revealed`）→ 失败回滚 pop」
//!   的不变量在此单点维护；
//! - 悔棋语义（`undo_steps` / `undo_to_current_player`）与可悔棋判定
//!   （对局未结束 + 当前行棋方存在更早的行动快照）两侧强制一致；
//! - `version` 单调递增：任何状态变更（走子/悔棋）+1（server 用于 AI 异步
//!   结果校验；gui 单用户场景可忽略）。
//!
//! 设计约束：不持有具体环境类型——所有环境操作经 [`with_env!`] 宏按
//! `(变体, 快照)` 重建泛型环境执行；banqi-core 新增变体时零改动。

use std::fmt;

use banqi_core::core::env::error::EnvError;
use banqi_core::core::env::{GameEnv, MoveHistory, MoveRecord, PositionSnapshot, SnapshotEnv, Variant};

use crate::app::envops::{self, StepOutcome};

/// 会话操作错误（中性枚举；各侧映射为自己的错误类型：
/// server → `ApiError`，gui → `String`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// 对局已结束，不能再走子/悔棋
    GameOver,
    /// 动作不在当前合法动作集合内
    IllegalAction { action: usize },
    /// 通用请求错误（参数非法 / 历史不足等）
    BadRequest(String),
    /// 内部错误（快照重建失败等不变量问题）
    Internal(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::GameOver => write!(f, "对局已结束"),
            SessionError::IllegalAction { action } => write!(f, "无效动作: {action}"),
            SessionError::BadRequest(s) => write!(f, "{s}"),
            SessionError::Internal(s) => write!(f, "内部错误: {s}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<EnvError> for SessionError {
    fn from(e: EnvError) -> Self {
        match e {
            EnvError::IllegalAction { action } => SessionError::IllegalAction { action },
            other => SessionError::Internal(other.to_string()),
        }
    }
}

/// 单局状态：变体 + 当前快照 + 快照栈（悔棋/棋谱）+ 单调版本号。
#[derive(Debug, Clone)]
pub struct SnapshotSession {
    pub variant: Variant,
    /// 当前局面完整快照
    pub current: PositionSnapshot,
    /// 走子前的快照栈 + 平行决策记录（core `MoveHistory`，单一语义真源）
    pub history: MoveHistory,
    /// 单调递增版本号：任何状态变更（走子/悔棋）+1
    pub version: u64,
}

impl SnapshotSession {
    /// 按变体新建（初始局面）。
    pub fn new(variant: Variant) -> Self {
        Self::from_snapshot(variant, envops::initial_snapshot(variant))
    }

    /// 由既有快照构建会话（BFEN 粘贴重建等场景；走子历史从空开始）。
    pub fn from_snapshot(variant: Variant, snapshot: PositionSnapshot) -> Self {
        Self {
            variant,
            current: snapshot,
            history: MoveHistory::new(),
            version: 0,
        }
    }

    /// 终局判定：`(terminated, truncated, winner)`。
    pub fn over_state(&self) -> (bool, bool, Option<i32>) {
        crate::with_env!(self.variant, &self.current, env => envops::game_over(&env))
            .expect("快照由本服务生成，重建不应失败")
    }

    /// 对局是否结束（terminated 或 truncated）。
    pub fn is_over(&self) -> bool {
        let (terminated, truncated, _) = self.over_state();
        terminated || truncated
    }

    /// 当前合法动作列表（升序）。
    pub fn legal_actions(&self) -> Vec<usize> {
        crate::with_env!(self.variant, &self.current, env => envops::legal_actions(&env))
            .unwrap_or_default()
    }

    /// 动作是否为机会动作（翻棋 / 吃暗子）。
    pub fn is_chance_action(&self, action: usize) -> bool {
        crate::with_env!(self.variant, &self.current, env => {
            GameEnv::is_chance_action(&env, action)
        })
        .unwrap_or(false)
    }

    /// 动作对应的坐标 `[from, to]`（翻棋时 from == to）。
    pub fn action_coords(&self, action: usize) -> Option<Vec<usize>> {
        envops::coords_for_action(self.variant, action)
    }

    /// 应用动作（校验终局与合法性；维护快照栈、决策历史与版本号）。
    ///
    /// 合法性预检后 step 仍失败只可能是内部不变量问题：回滚压栈并报错，
    /// 恢复 `stack.len() == records.len()`。
    pub fn apply_action(&mut self, action: usize) -> Result<StepOutcome, SessionError> {
        if self.is_over() {
            return Err(SessionError::GameOver);
        }
        if !self.legal_actions().contains(&action) {
            return Err(SessionError::IllegalAction { action });
        }
        let prev = self.current.clone();
        self.history.push(prev.clone());
        let step_res = crate::with_env!(self.variant, &self.current, env => {
            envops::step(&mut env, action)
                .map(|outcome| (outcome, env.to_snapshot(), GameEnv::last_revealed(&env)))
        })
        .and_then(std::convert::identity);
        match step_res {
            Ok((outcome, snap, revealed)) => {
                // 本步实际翻出的棋子（翻棋/吃暗子的真实结果，快照差分推不出）
                self.history
                    .record(MoveRecord::from_step(&prev, &snap, action, revealed));
                self.current = snap;
                self.version += 1;
                Ok(outcome)
            }
            Err(e) => {
                self.history.pop();
                Err(SessionError::from(e))
            }
        }
    }

    /// 悔棋：按快照逐帧回退 `steps` 步（对局结束后不可悔棋）。
    pub fn undo_steps(&mut self, steps: usize) -> Result<(), SessionError> {
        if self.is_over() {
            return Err(SessionError::GameOver);
        }
        if steps == 0 {
            return Err(SessionError::BadRequest("steps 必须至少为 1".to_string()));
        }
        let snap = self.history.undo_steps(steps).ok_or_else(|| {
            SessionError::BadRequest(format!(
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
    pub fn undo_to_current_player(&mut self) -> Result<(), SessionError> {
        if self.is_over() {
            return Err(SessionError::GameOver);
        }
        let target = self.current.current_player;
        let snap = self
            .history
            .undo_to_player_turn(target)
            .ok_or_else(|| SessionError::BadRequest("没有可悔的棋步（当前玩家尚未行动过）".to_string()))?;
        self.current = snap;
        self.version += 1;
        Ok(())
    }

    /// 当前是否可悔棋：对局未结束，且历史中存在「当前行棋方上次行动前」的快照
    /// （悔棋总是回到当前行棋方自己上次行动前，可连续触发以反悔所有自己的行动）。
    pub fn undoable(&self) -> bool {
        !self.is_over() && self.history.undoable(self.current.current_player)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banqi_core::core::env::DarkChessEnv;

    fn session() -> SnapshotSession {
        SnapshotSession::new(Variant::DarkChess4x8)
    }

    fn first_flip_action(s: &SnapshotSession) -> usize {
        *s.legal_actions()
            .iter()
            .find(|&&a| s.is_chance_action(a))
            .expect("初始局面必有可翻棋动作")
    }

    #[test]
    fn apply_action_maintains_history_invariant() {
        let mut s = session();
        let action = first_flip_action(&s);
        let outcome = s.apply_action(action).expect("翻棋应成功");
        assert!(!outcome.is_over());
        // 压栈 + 落账成对：stack.len() == records.len()
        assert_eq!(s.history.len(), s.history.records().len());
        assert_eq!(s.history.len(), 1);
        let rec = &s.history.records()[0];
        assert!(rec.is_flip && rec.is_chance);
        assert!(rec.revealed.is_some(), "翻棋必须落账实际翻出的棋子");
        assert_eq!(s.version, 1);
    }

    #[test]
    fn illegal_action_rejected_without_mutation() {
        let mut s = session();
        let before = s.current.clone();
        let action = *s
            .legal_actions()
            .iter()
            .max()
            .expect("总有合法动作");
        // 找一个不合法的动作（合法动作集合之外必然存在：动作空间远大于合法集）
        let illegal = (0..)
            .find(|&a| !s.legal_actions().contains(&a))
            .expect("动作空间大于合法集合");
        let _ = action;
        assert!(matches!(
            s.apply_action(illegal),
            Err(SessionError::IllegalAction { .. })
        ));
        assert_eq!(s.current, before, "非法动作不得修改局面");
        assert_eq!(s.version, 0);
    }

    #[test]
    fn undo_to_current_player_reverts_own_last_action() {
        let mut s = session();
        let flip = first_flip_action(&s);
        s.apply_action(flip).expect("红方翻棋");
        let flip2 = *s
            .legal_actions()
            .iter()
            .find(|&&a| s.is_chance_action(a))
            .expect("仍可翻棋");
        s.apply_action(flip2).expect("第二次翻棋");
        assert!(s.undoable());
        // 当前行棋方为红（黑翻完换红）：悔到红方上次行动前 = 对局起点
        s.undo_to_current_player().expect("悔到红方上次行动前");
        assert_eq!(s.history.len(), 0);
        assert_eq!(s.version, 3);
        // 已无自己的行动可反悔
        assert!(!s.undoable());
        assert!(s.undo_to_current_player().is_err());
    }

    #[test]
    fn fresh_session_not_undoable() {
        let mut s = session();
        assert!(!s.undoable(), "当前行棋方尚未行动过");
        assert!(s.undo_to_current_player().is_err());
    }

    #[test]
    fn rebuilt_env_matches_session_snapshot() {
        let mut s = session();
        let action = first_flip_action(&s);
        s.apply_action(action).expect("翻棋应成功");
        let env = DarkChessEnv::from_snapshot(&s.current).expect("重建不应失败");
        assert_eq!(env.to_snapshot(), s.current);
    }
}
