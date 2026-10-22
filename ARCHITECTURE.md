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
│   ├── player.rs      ★ 统一选手层：PlayerSlot/Slots 红黑槽位 + StrategyKind 契约 +
│   │                  decide/dry_run 单一决策入口（含 MCTS 树复用三档分派）+ EngineParams
│   ├── search.rs      SearchBudget、run_gumbel（泛型核心）、tree_reuse_plan（泛型树复用）、
│   │                  mcts_search_torch / mcts_search_onnx、nnue_search、nnue_search_tree（树捕获）
│   ├── models.rs      目录扫描（torch 时含 .pt）、EngineKind、ModelPool（随用随加载缓存）
│   ├── mcts_view.rs   MCTS 树 DTO、只读查询、统计、MctsTreeBox 变体擦除（含 into_any 下沉）
│   ├── expectimax_view.rs Expectimax 搜索轨迹树 DTO（ExEdge/ExNodeSummary/ExRootInfo/ExNodeBoard）、
│   │                  只读查询（ex_root_info/ex_edge_list/ex_node_summary/ex_node_board）
│   ├── common.rs      player 标签、棋谱条目映射
│   └── sync.rs        锁中毒恢复辅助（lock / read_lock / write_lock）
├── api/               HTTP handlers
│   ├── mod.rs         路由表 + health / variants / settings / capabilities / models
│   ├── games.rs       对局 CRUD、走子、run 单步、dry-run、悔棋、槽位切换、move-action
│   ├── agents.rs      Agent（玩家实体）CRUD + 搜索参数实时调整
│   ├── engine.rs      POST /api/engine/move 无状态引擎查询（连线程序用）
│   ├── mcts.rs        MCTS 树查询 4 端点 + 手动搜索（按模型扩展名分派 torch / onnx）
│   └── nnue.rs        Expectimax 轨迹树查询 4 端点 + 手动搜索（.nnue 模型）
├── state.rs           AppState（games + params + ModelPool）、GameSession（slots 双槽位）、模型加载包装
├── ai.rs              策略回合编排与 dry-run（锁 1 取数 → 锁外 player::decide → 锁 2 版本校验落子）
├── dto.rs             请求 / 响应类型（slots 双槽位契约，真源为 banqi-frontend/src/api/types.ts）
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

## 选手槽位（PlayerSlot 单一真源，2026-10-22 解耦重构）

「对局模式」（PvP / 单一对手）概念已废除：每局红、黑各一个独立槽位
（`app::player::PlayerSlot`），可随时设为 `Human`（用户本人操作）或任意策略托管；
双方均为 `Human` 即原本地双人。全部策略的「策略 → 动作」分派收敛于
`app::player::decide`（单一真源），dry-run 走 `app::player::dry_run`（不落子）。

| 字符串 | 后端 | 模型 | feature |
|---|---|---|---|
| Random / RevealFirst / CaptureFirst | 规则策略（锁内同步） | 无 | — |
| Expectimax（旧名 Engine） | Expectimax 强引擎（blocking） | 无 | — |
| ExpectimaxNnue（旧名 Nnue） | Expectimax + NNUE 叶评估（blocking） | .nnue | — |
| MctsOnnx | Gumbel MCTS + ONNX（blocking） | .onnx | onnx |
| MctsDL | Gumbel MCTS + TorchScript（blocking） | .pt | torch |

运行控制：
- `POST /api/games/{id}/dry-run`：当前行棋方策略建议走法（不落子；Human 方作提示，
  缺省 Expectimax；返回 `version` 供前端丢弃过期高亮；MCTS 树入会话供浏览与续用；
  `agentId` 指定 Agent 时以其当前配置为当前行动方计算——与托管阵营无关）
- `POST /api/games/{id}/run`：单步——当前行棋方按其槽位策略执行一步（Human 方 409；
  `agentId` 指定 Agent 时以其当前配置为当前行动方执行一步，Human 方亦可）
- `PUT /api/games/{id}/slots`：对局中切换任一方槽位（模型一并指定并预加载校验；
  `kind: "Agent"` 绑定 Agent 实体，其策略/搜索配置实时生效）
- auto（自动对弈到终局/可暂停）由前端循环调 run 实现：每步后检查
  `over` / `can_run` / `autoEnabled`，把当前方槽位切回 Human 即随时接管

Agent（玩家实体，`PlayerSlot::Agent(Uuid)`）：命名的策略配置——变体 + 策略种类 +
模型文件 + 可实时调整的搜索参数。创建后只读；决策时从注册表实时解析为策略槽位
（`AppState::resolve_agent_slot`，变体不一致拒绝），搜索参数调整对所有绑定对局立即生效；
Agent 的建议/单步端点永远作用于当前行动方，与托管阵营无关。

## 跨回合 MCTS 树复用

- 会话持有 `mcts_tree: Option<MctsTreeBox>`（变体擦除）；MCTS 类策略在锁 1 取出上一棵树
  （`GameSession::take_reusable_mcts_tree`，经 `MctsTreeView::into_any` 下沉还原为
  `MctsTree<DarkChessEnv>`；本 server 的 MCTS 搜索统一以 `DarkChessEnv` 实例化，
  `DarkChessEnv::from_snapshot` 按快照自带变体配置重建，所有变体通用）。
- `app::search::mcts_reuse` 三档分派（泛型实现，所有变体环境可用）：
  ① 树根与当前局面一致（如 dry-run 后紧接 run）→ `run_gumbel_reuse_root` 直接续用；
  ② `tree_reuse_plan` 沿「策略上手 → 对手上手」推进两步，机会动作（翻棋/吃暗子）
  用 `chance_outcomes` 枚举全部候选 + ResNet 特征校验确定性重放；
  ③ 均对不上 → 全新建树。
- 复用尝试会消费旧树——最终版本号校验失败结果作废时旧树不恢复（可重新搜索）。
- 手动搜索（`POST /api/games/:id/mcts/search`）同样按策略槽位模型分派并尝试复用。

## HTTP 端点

| 端点 | 说明 |
|---|---|
| POST/GET /api/games | 建局（`slots{red,black}`，缺省双方 Human）/ 列表 |
| GET/DELETE /api/games/{id} | 状态（含 `slots` / `can_run`）/ 删除 |
| POST /api/games/{id}/move | 人类走子（轮到策略方时自动附带其回合） |
| POST /api/games/{id}/run | 单步：当前策略方执行一步 |
| POST /api/games/{id}/dry-run | 当前方策略建议走法（不落子） |
| PUT /api/games/{id}/slots | 对局中切换槽位 |
| POST /api/agents | 创建 Agent（玩家实体：变体+种类+模型+搜索参数，创建即预加载校验） |
| GET /api/agents、GET/DELETE /api/agents/{id} | Agent 列表 / 详情 / 删除（创建后只读） |
| PUT /api/agents/{id}/settings | 实时调整 Agent 搜索参数（唯一可变字段，对绑定对局立即生效） |
| POST /api/games/{id}/undo | 悔棋 |
| GET /api/games/{id}/move-action?from=&to= | 坐标对 → 动作索引 |
| GET /api/games/{id}/mcts/root | MCTS 树查询（root / node/{n}[/children|/board] / search） |
| GET /api/games/{id}/nnue/... | Expectimax 轨迹树可视化 + 手动搜索 |
| GET /api/health、/api/variants、/api/settings、/api/capabilities、/api/models | 基础 / 设置 / 能力 / 模型 |
| POST /api/engine/move | 无状态引擎查询（BFEN → 着法） |

## DTO 契约（真源：banqi-frontend/src/api/types.ts）

- 状态响应 = 前端 `GameState` 全量字段（棋盘短码 `"R_Sol"`、阵亡/隐藏棋子全名
  `"Soldier"`、`action_masks` / `reveal_probabilities` / `bitboards`、BFEN）flatten 展开
  + server 专属扩展（`game_id` / `version` / `legal_actions` / `slots` / `can_run` /
  `can_undo` / `over` / `terminated` / `truncated` / `over_reason` / `winner` / `max_steps`）。
- 走子 / run / 悔棋响应 = 前端 `StepResult`（state + terminated/truncated/winner 数字编码
  + mcts_stats / nnue_stats）+ server 扩展 `actions`。
- dry-run 响应 = 前端 `Suggestion`（version / action / from / to / is_flip / is_capture /
  score? / 搜索统计）。
- snake_case 直传（Exception：请求 DTO（`CreateGameReq` / `SlotReq` 等）与
  `/api/settings`、`EngineSettings` 为 camelCase，与前端 types.ts 一致）。
- MCTS 节点棋盘 `state` 为完整 `GameState` 形状（`app::view::GameStateView`，与状态端点共用
  构造器），机会节点动作经任一机会结果子节点回溯。

## 锁与并发纪律

- 所有持锁段为同步 CPU 操作，不跨 await；搜索 / 模型加载经 `spawn_blocking`。
- 策略回合三段式：锁 1 取快照与槽位（MCTS 类同时取上一棵树；规则策略锁内直出）→
  锁外搜索（`app::player::decide`）→ 锁 2 版本校验后落子，期间任何状态变更
  （走子/悔棋/切槽位）使结果作废（不视为错误）。
- dry-run 同三段式但不落子：锁 1 取快照与生效策略 → 锁外 `dry_run` →
  锁 2 版本未变时存入搜索树；建议随响应携带 `version` 供前端丢弃过期高亮。
- `AppState.params`（默认引擎参数）经 `PUT /api/settings` 部分更新 + 钳制
  （`EngineParams::updated/clamped` 单一真源）；槽位参数建局/改槽位时快照固定。

## 构建与运行

```bash
cargo run --bin banqi-server -- --addr 127.0.0.1:8080 [--models-dir DIR] [--device cpu]
cargo test                                  # 34 个单元测试
cargo run --bin banqi-cli -- health         # CLI 冒烟测试
```

## 变更记录

- 2026-10-23：去重与正确性收敛：①手动搜索锁 2 版本校验收口——`state.rs` 新增
  `store_manual_search_trees`（版本号变化时丢弃结果），`api/mcts.rs` / `api/nnue.rs`
  的 search 端点此前无版本校验（与 ai.rs 三段式口径分叉，并发下树可能写入错误局面）；
  ②`state.rs` 新增 `strategy_slot_view`（`slot_view` 与 `api/agents.rs::agent_view`
  的 budget 五元组 match 单一真源）与泛型 `parse_id`（对局/Agent/节点 ID 解析收敛）；
  ③删除 `variant.rs` 薄 re-export（唯一消费者 state.rs 改指 `app::envops`）；
  删除 Cargo.toml 零引用的 `ort` 直接依赖；④`app/search.rs` 抽取 `mcts_search_impl`
  （torch/onnx 两个三臂 match 逐字同构）与 `nnue_stats_from`，删除无调用方的
  `nnue_search`（非 tree 版，唯一调用方 banqi-gui 已归档）；⑤`app/player.rs::decide`
  的 MctsOnnx/MctsDL 两臂收敛为 `mcts_branch` 公共装配。

- 2026-10-22：对局与玩家/策略解耦：废除"对局模式"，新增 `app/player.rs` 统一选手层
  （`PlayerSlot`/`Slots` + `decide`/`dry_run` 单一决策入口，收编 opponent.rs 与 ai.rs
  大 match）；`GameSession.slots` 双槽位替代 `Option<OpponentConfig>`；REST 契约破坏性
  演进为 `slots{red,black}`（删 opponent/aiEnabled/aiPlayer/modelFile 与
  ai-move/model/opponent 端点），新增 dry-run / run / slots 端点与 `HumanTurn` 错误码；
  MCTS 树复用三档分派（根一致直接续用 / 两步推进 / 全新建树）；前端槽位选择器 +
  dry-run 高亮 + run/auto 控制；banqi-cli 同步更新。
- 2026-10-22：Expectimax+NNUE 搜索轨迹树可视化：①`app/expectimax_view.rs`（`ExpectimaxTree` +
  `ExEdge`/`ExNodeSummary`/`ExRootInfo`/`ExNodeBoard` DTO 与只读查询，树语义为 Alpha-Beta
  搜索轨迹：flag 界类型 / TT 截断标注 / 机会 outcome 概率，见 banqi-core `expectimax::tree`）；
  ②`app/search.rs` 新增 `nnue_search_tree`（threads=1 + 剔除 FEAT_ASP + `collect_tree`）；
  ③`GameSession` 新增 `expectimax_tree`，`ai.rs::choose_nnue_blocking` 改返回树并在
  `apply_opponent_action` 写入会话；④新增 `api/nnue.rs` 五端点（root/node/children/board +
  POST search）与 `nnue.rs` 薄 re-export，`error.rs` 新增 `NnueTreeMissing`
  （HTTP 404 / `NNUE_TREE_MISSING`）
- 2026-10-22：归档 banqi-gui / banqi-app 至 `archive/`；banqi-app 8 模块并入 `src/app/`；
  补齐 MctsDL（torch）对手、`.pt` 模型加载绑定、跨回合树复用（泛型化 `tree_reuse_plan`）、
  capabilities / models / opponent / move-action 端点；DTO 契约统一为前端 types.ts
  （snake_case + 短码，删除前端适配层与 `@tauri-apps/api` 依赖）；新增本文档。
