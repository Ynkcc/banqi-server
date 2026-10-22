# banqi-server 架构

Banqi 暗棋对弈 **单一后端**：axum REST API + 推理引擎（ONNX / TorchScript / NNUE）+ AI 对手 + rust-embed 内嵌前端（`banqi-frontend/dist`）。

> 2026-10-22 架构收敛：原 Tauri 桌面端（`banqi-gui`）与共享应用层 crate（`banqi-app`）归档至
> `archive/`，全部业务逻辑收拢到本 crate；前端仅走 HTTP，DTO 契约以
> `banqi-frontend/src/api/types.ts` 为单一真源（snake_case 直传，无适配层）。

## 模块结构

```
src/
├── main.rs            启动装配（config → AppState → router → axum serve + 优雅退出）
├── app/               ★ 共享应用层（原 banqi-app crate 并入，纯逻辑零 axum/tokio 依赖）
│   ├── envops.rs      with_env! 宏（变体→泛型环境重建）、step / 终局 / 合法动作 / 坐标
│   ├── session.rs     SnapshotSession：走子/悔棋/棋谱不变量 + 版本号（单一真源）
│   ├── view.rs        中性投影（project / game_view / over_reason）
│   │                  + GameStateView（前端 types.ts GameState 契约视图，状态与 MCTS 节点棋盘共用）
│   ├── opponent.rs    OpponentKind（含 parse / needs_model）、choose_simple、EngineParams 钳制
│   ├── search.rs      SearchBudget、run_gumbel（泛型核心）、tree_reuse_plan（泛型树复用）、
│   │                  mcts_search_torch / mcts_search_onnx、nnue_search
│   ├── models.rs      目录扫描（torch 时含 .pt）、EngineKind、ModelPool（随用随加载缓存）
│   ├── mcts_view.rs   MCTS 树 DTO、只读查询、统计、MctsTreeBox 变体擦除（含 into_any 下沉）
│   ├── common.rs      player 标签、棋谱条目映射
│   └── sync.rs        锁中毒恢复辅助（lock / read_lock / write_lock）
├── api/               HTTP handlers
│   ├── mod.rs         路由表 + health / variants / settings / capabilities / models
│   ├── games.rs       对局 CRUD、走子、AI 回合、悔棋、模型绑定、opponent、move-action
│   ├── engine.rs      POST /api/engine/move 无状态引擎查询（连线程序用）
│   └── mcts.rs        MCTS 树查询 4 端点 + 手动搜索（按模型扩展名分派 torch / onnx）
├── state.rs           AppState（games + params + ModelPool）、GameSession、模型加载包装
├── ai.rs              对手回合编排（锁 1 取数 → 锁外 spawn_blocking 搜索 → 锁 2 版本校验落子）
├── dto.rs             请求 / 响应类型（契约真源为 banqi-frontend/src/api/types.ts）
├── error.rs           ApiError + HTTP 状态码 / 机器可读错误码
├── config.rs          ServerConfig（CLI 参数解析）
├── web.rs             rust-embed 内嵌前端 + SPA fallback
├── mcts.rs / variant.rs  薄 re-export（指向 crate::app::*，保持引用路径稳定）
└── bin/cli.rs         banqi-cli：REST API 薄封装客户端（LLM / 脚本测试用）
```

## 依赖图

```
banqi-simd ← banqi-core ← banqi-engine（feat: torch(tch) / onnx(ort) / onnx-cuda）
                  ▲              ▲
                  └── banqi-server（app/ + api/ + ai/）
```

- 无根 workspace，path 依赖连接；本 crate 无 default-run，需 `--bin banqi-server` 运行。
- features：`default = ["onnx"]`；`torch = ["banqi-engine/torch"]`；`onnx`；`onnx-cuda`。
  本地验证 torch 组合：`LIBTORCH_USE_PYTORCH=1 cargo check --features torch`。

## 对手种类（OpponentKind 单一真源）

| 字符串 | 后端 | 模型 | feature |
|---|---|---|---|
| Random / RevealFirst / CaptureFirst | 规则策略（锁内同步） | 无 | — |
| Engine | Expectimax 强引擎（blocking） | 无 | — |
| Nnue | Expectimax + NNUE 叶评估（blocking） | .nnue | — |
| MctsOnnx | Gumbel MCTS + ONNX（blocking） | .onnx | onnx |
| MctsDL | Gumbel MCTS + TorchScript（blocking） | .pt | torch |

## 跨回合 MCTS 树复用

- 会话持有 `mcts_tree: Option<MctsTreeBox>`（变体擦除）；MCTS 类对手在锁 1 取出上一棵树
  （`GameSession::take_reusable_mcts_tree`，经 `MctsTreeView::into_any` 下沉还原为
  `MctsTree<DarkChessEnv>`；本 server 的 MCTS 搜索统一以 `DarkChessEnv` 实例化，
  `DarkChessEnv::from_snapshot` 按快照自带变体配置重建，所有变体通用）。
- `app::search::tree_reuse_plan` 为泛型实现（所有变体环境可用）：沿「AI 上手 → 对手上手」
  推进两步，机会动作（翻棋/吃暗子）用 `chance_outcomes` 枚举全部候选 + ResNet 特征校验
  确定性重放；对不上自动回退全新建树。
- 复用尝试会消费旧树——最终版本号校验失败结果作废时旧树不恢复（可重新搜索）。
- 手动搜索（`POST /api/games/:id/mcts/search`）同样按扩展名分派 torch / onnx 并尝试复用。

## HTTP 端点 ↔ 原 Tauri 命令对照

| 原 Tauri 命令（已归档） | HTTP 端点 |
|---|---|
| reset_game / load_fen | POST /api/games |
| step_game | POST /api/games/{id}/move |
| bot_move | POST /api/games/{id}/ai-move |
| undo_game | POST /api/games/{id}/undo |
| get_game_state | GET /api/games/{id} |
| list_variants | GET /api/variants |
| get_opponent_type | GET /api/games/{id}/opponent |
| get_move_action | GET /api/games/{id}/move-action?from=&to= |
| get_capabilities | GET /api/capabilities |
| list_models | GET /api/models |
| load_model | PUT /api/games/{id}/model |
| open_models_dir（桌面专属） | 无（前端隐藏入口） |
| set_engine_settings | GET/PUT /api/settings |
| mcts_get_root | GET /api/games/{id}/mcts/root |
| mcts_get_children | GET /api/games/{id}/mcts/node/{n}/children |
| mcts_get_node_detail | GET /api/games/{id}/mcts/node/{n} |
| mcts_get_node_board | GET /api/games/{id}/mcts/node/{n}/board |
| mcts_search | POST /api/games/{id}/mcts/search |
| —（server 特有） | GET /api/health、GET /api/games、DELETE /api/games/{id}、POST /api/engine/move |

## DTO 契约（真源：banqi-frontend/src/api/types.ts）

- 状态响应 = 前端 `GameState` 全量字段（棋盘短码 `"R_Sol"`、阵亡/隐藏棋子全名
  `"Soldier"`、`action_masks` / `reveal_probabilities` / `bitboards`、BFEN）flatten 展开
  + server 专属扩展（`game_id` / `version` / `legal_actions` / `ai` / `ai_to_move` /
  `can_undo` / `over` / `terminated` / `truncated` / `over_reason` / `winner` / `max_steps`）。
- 走子 / AI 回合 / 悔棋响应 = 前端 `StepResult`（state + terminated/truncated/winner 数字编码
  + mcts_stats / nnue_stats）+ server 扩展 `actions`。
- snake_case 直传（Exception：`/api/settings` 请求与 `EngineSettings` 响应为 camelCase，
  与前端 types.ts `EngineSettings` 一致）。
- MCTS 节点棋盘 `state` 为完整 `GameState` 形状（`app::view::GameStateView`，与状态端点共用
  构造器），机会节点动作经任一机会结果子节点回溯。

## 锁与并发纪律

- 所有持锁段为同步 CPU 操作，不跨 await；搜索 / 模型加载经 `spawn_blocking`。
- 对手回合三段式：锁 1 取快照与参数（MCTS 类同时取上一棵树）→ 锁外搜索 →
  锁 2 版本校验后落子，期间任何状态变更（走子/悔棋）使结果作废（不视为错误）。
- `AppState.params`（默认引擎参数）经 `PUT /api/settings` 部分更新 + 钳制
  （`EngineParams::updated/clamped` 单一真源）；对局参数建局时快照固定。

## 构建与运行

```bash
cargo run --bin banqi-server -- --addr 127.0.0.1:8080 [--models-dir DIR] [--device cpu]
cargo test                                  # 29 个单元测试
cargo run --bin banqi-cli -- health         # CLI 冒烟测试
```

## 变更记录

- 2026-10-22：归档 banqi-gui / banqi-app 至 `archive/`；banqi-app 8 模块并入 `src/app/`；
  补齐 MctsDL（torch）对手、`.pt` 模型加载绑定、跨回合树复用（泛型化 `tree_reuse_plan`）、
  capabilities / models / opponent / move-action 端点；DTO 契约统一为前端 types.ts
  （snake_case + 短码，删除前端适配层与 `@tauri-apps/api` 依赖）；新增本文档。
