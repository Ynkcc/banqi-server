//! banqi-app：gui / server 共享的应用逻辑层。
//!
//! 收敛 `banqi-gui`（Tauri 桌面端）与 `banqi-server`（axum HTTP 服务）中
//! 重复的实现，作为单一真源：
//!
//! - [`envops`]：变体无关环境操作（`with_env!` 重建泛型环境、step/终局/合法动作/坐标）；
//! - [`session`]：快照式对局会话核心（走子/悔棋/棋谱不变量 + 版本号）；
//! - [`view`]：快照的中性结构化投影（棋盘格位/阵亡/隐藏/终局原因）；
//! - [`opponent`]：对手抽象（规则对手 / 纯计算引擎的同步决策 + 引擎参数快照）；
//! - [`mcts_view`]：MCTS 搜索树 DTO、只读查询、统计与变体擦除（`MctsTreeView`）；
//! - [`search`]：Gumbel MCTS（含树复用）与 NNUE Expectimax 的同步搜索执行；
//! - [`models`]：模型目录递归扫描、EngineKind 分派、随用随加载模型池、布局校验；
//! - [`common`]：player 标签、棋谱条目映射等公共辅助；
//! - [`sync`]：锁中毒恢复辅助。
//!
//! 设计约束：本 crate 为纯逻辑层，不依赖 tauri / axum / tokio；搜索函数
//! 提供同步（blocking）签名，异步包装（`spawn_blocking`）由调用方各自实现。
//! 前端契约（DTO 字段名与序列化格式）保持与收敛前一致。

pub mod common;
pub mod envops;
pub mod models;
pub mod mcts_view;
pub mod opponent;
pub mod search;
pub mod session;
pub mod sync;
pub mod view;
