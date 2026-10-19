//! MCTS 搜索树可视化：实现已收敛到 `banqi-app::mcts_view`（单一真源）。
//!
//! 本模块仅 re-export，保持既有引用路径（`crate::mcts::*`）稳定：
//! - 树以泛型 `MctsTree<G>`（`G: GameEnv`）承载，经 `MctsTreeView` trait object
//!   （`MctsTreeBox`）挂在 `GameSession` 上（变体无关）；
//! - DTO 序列化为 snake_case，与 banqi-core serde 风格一致，client 适配层直传。
//!
//! 生命周期：每次 AI 回合（或手动搜索）替换整棵树；不做跨回合复用
//! （GUI 的 tree_reuse_plan 依赖具体变体的环境重放，如需可在
//! `banqi_app::search` 中启用）。

pub use banqi_app::mcts_view::*;
