use crate::app::envops::{self, StepOutcome};
use banqi_core::core::env::types::Piece;
use banqi_core::core::env::{
    DarkChessEnv, GameEnv, MoveHistory, MoveRecord, PositionSnapshot, SnapshotEnv, Variant,
};
use bqsrv_dto::ApiError;
#[derive(Debug, Clone)]
pub struct SnapshotSession {
    pub variant: Variant,
    pub current: PositionSnapshot,
    pub history: MoveHistory,
    pub version: u64,
}
impl SnapshotSession {
    pub fn new(variant: Variant) -> Self {
        Self::from_snapshot(variant, envops::initial_snapshot(variant))
    }
    pub fn new_with_scenario(
        variant: Variant,
        scenario: Option<&banqi_core::core::env::OpeningScenario>,
    ) -> Result<Self, ApiError> {
        let snap = envops::initial_snapshot_with_scenario(variant, scenario)?;
        Ok(Self::from_snapshot(variant, snap))
    }
    pub fn from_snapshot(variant: Variant, snapshot: PositionSnapshot) -> Self {
        Self {
            variant,
            current: snapshot,
            history: MoveHistory::new(),
            version: 0,
        }
    }
    pub fn over_state(&self) -> (bool, bool, Option<i32>) {
        crate::with_env!(self.variant, &self.current, env => envops::game_over(&env))
            .expect("快照由本服务生成，重建不应失败")
    }
    pub fn is_over(&self) -> bool {
        let (terminated, truncated, _) = self.over_state();
        terminated || truncated
    }
    pub fn legal_actions(&self) -> Vec<usize> {
        crate::with_env!(self.variant, &self.current, env => envops::legal_actions(&env))
            .unwrap_or_default()
    }
    pub fn is_chance_action(&self, action: usize) -> bool {
        crate::with_env!(self.variant, &self.current, env => {
            GameEnv::is_chance_action(&env, action)
        })
        .unwrap_or(false)
    }
    pub fn action_coords(&self, action: usize) -> Option<Vec<usize>> {
        envops::coords_for_action(self.variant, action)
    }
    pub fn apply_action(&mut self, action: usize) -> Result<StepOutcome, ApiError> {
        if self.is_over() {
            return Err(ApiError::GameOver);
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
                self.history
                    .record(MoveRecord::from_step(&prev, &snap, action, revealed));
                self.current = snap;
                self.version += 1;
                Ok(outcome)
            }
            Err(e) => {
                self.history.pop();
                Err(ApiError::from(e))
            }
        }
    }

    pub fn apply_action_with_reveal(
        &mut self,
        action: usize,
        reveal: Option<Piece>,
    ) -> Result<StepOutcome, ApiError> {
        if self.is_over() {
            return Err(ApiError::GameOver);
        }
        let prev = self.current.clone();
        let mut env = <DarkChessEnv as SnapshotEnv>::from_snapshot(&prev)
            .map_err(|e| ApiError::Internal(format!("重建环境失败: {e}")))?;
        let is_chance = GameEnv::is_chance_action(&env, action);

        let reveal = if is_chance { reveal } else { None };
        self.history.push(prev.clone());
        let step_res = env
            .step(action, reveal)
            .map(|(_, terminated, truncated, winner)| StepOutcome {
                terminated,
                truncated,
                winner,
            })
            .map_err(ApiError::from);
        match step_res {
            Ok(outcome) => {
                let post = env.to_snapshot();
                let revealed = GameEnv::last_revealed(&env);
                self.history
                    .record(MoveRecord::from_step(&prev, &post, action, revealed));
                self.current = post;
                self.version += 1;
                Ok(outcome)
            }
            Err(e) => {
                self.history.pop();
                Err(e)
            }
        }
    }
    pub fn undo_steps(&mut self, steps: usize) -> Result<(), ApiError> {
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
        let action = *s.legal_actions().iter().max().expect("总有合法动作");
        let illegal = (0..)
            .find(|&a| !s.legal_actions().contains(&a))
            .expect("动作空间大于合法集合");
        let _ = action;
        assert!(matches!(
            s.apply_action(illegal),
            Err(ApiError::IllegalAction { .. })
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
        s.undo_to_current_player().expect("悔到红方上次行动前");
        assert_eq!(s.history.len(), 0);
        assert_eq!(s.version, 3);
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
    #[test]
    fn scenario_session_builds_constrained_opening() {
        use banqi_core::core::env::OpeningScenario;
        let s = SnapshotSession::new_with_scenario(
            Variant::DarkChess4x8,
            Some(&OpeningScenario::with_general(5, 19)),
        )
        .expect("场景建局应成功");
        let env = DarkChessEnv::from_snapshot(&s.current).expect("重建不应失败");
        let slots = env.get_board_slots();
        assert!(
            matches!(&slots[5], banqi_core::core::env::Slot::Revealed(p) if p.short_name() == "R_Can"),
            "sq5 应为红炮明子，实际 {:?}",
            slots[5]
        );
        assert!(
            matches!(&slots[19], banqi_core::core::env::Slot::Revealed(p) if p.short_name() == "B_Gen"),
            "sq19 应为黑将明子，实际 {:?}",
            slots[19]
        );
        assert_eq!(s.current.current_player, banqi_core::core::env::Player::Red);
        let err = SnapshotSession::new_with_scenario(
            Variant::DarkChess4x8Fast,
            Some(&OpeningScenario::new(5)),
        )
        .expect_err("不支持的变体应报错");
        assert!(matches!(err, ApiError::BadRequest(_)));
    }
}
