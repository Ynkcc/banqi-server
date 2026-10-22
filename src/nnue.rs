//! Expectimax 搜索轨迹树可视化：实现已收敛到 `crate::app::expectimax_view`
//! （单一真源）。
//!
//! 本模块仅 re-export，保持既有引用路径（`crate::nnue::*`）稳定：
//! - 树由 `app::search::nnue_search_tree`（`collect_tree` 开启）产生，挂
//!   `GameSession.expectimax_tree`；
//! - DTO 序列化为 snake_case，契约真源为 `banqi-frontend/src/api/types.ts`。
//!
//! 生命周期：每次 NNUE 对手回合（或手动搜索）替换整棵树；不做跨回合复用
//! （Alpha-Beta 每回合全新建树，无 MCTS 式 root 下移语义）。

pub use crate::app::expectimax_view::*;
