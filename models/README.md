# models/

banqi-server 默认模型目录。服务启动时动态扫描本目录下全部 `*.onnx` 并加载。

**模型与变体的关联不需要文件名约定**：启动时读取每个模型的输入静态维度
（`board[B,C,H,W]` + `scalars[B,S]`），与各变体的特征维度自动匹配；
文件名仅在同维度存在多个模型时作为优先提示（stem == 变体 ID 者优先）。

- 单个文件加载失败只告警，不影响其他模型与对弈功能
- 维度与任何变体都不匹配的模型会被加载但不会用于 AI
- 启动日志与 `GET /api/health` 会列出每个模型匹配到的变体

新增/更换变体模型：把训练产出的 onnx 放入本目录即可，server 代码零改动。

模型前向契约（与 banqi-engine 固定一致）：

- 输入：`board[B, C, H, W]`、`scalars[B, S]`（输入名固定 `board` / `scalars`）
- 输出：`policy_logits[B, A]`、`value[B, 1]`（可选第三输出 `health[B, K]`）
