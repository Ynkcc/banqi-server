//! Agent（玩家实体）REST handlers。
//!
//! Agent = 命名的策略配置（变体 + 策略种类 + 模型文件 + 搜索参数）。
//! - 创建时解析并预加载模型（模型类策略），坏文件 / 跨变体权重即报错；
//! - 创建后只读，仅搜索参数可经 `PUT /api/agents/{id}/settings` 实时调整
//!   （决策时从注册表实时读取，对所有绑定它的对局立即生效）；
//! - Agent 通过槽位绑定（`PUT /api/games/{id}/slots`，`kind: "Agent"`）托管到
//!   红/黑；其「建议 / 单步」端点（带 `agentId` 的 dry-run / run）永远作用于
//!   当前行棋方，与托管阵营无关。
//!
//! 持锁段均为同步操作，不跨 await；模型预加载经 `AppState::preload_engine_model`。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use uuid::Uuid;

use crate::app::player::{Agent, StrategyKind};
use crate::dto::{AgentResp, AgentsListResp, CreateAgentReq, SlotParamsReq};
use crate::error::ApiError;
use crate::state::{parse_id, strategy_slot_view, SharedState};

fn parse_agent_id(raw: &str) -> Result<Uuid, ApiError> {
    parse_id(raw, "Agent ID")
}

fn agent_view(agent: &Agent) -> AgentResp {
    AgentResp {
        id: agent.id.to_string(),
        variant: agent.variant.as_str().to_string(),
        // 与 SlotView（策略槽位视图）同构：按策略种类下发生效参数摘要（单一真源）
        slot: strategy_slot_view(agent.kind, agent.model_file.clone(), agent.params),
    }
}

/// POST /api/agents：创建 Agent（玩家实体）。
///
/// 变体 + 策略种类 + 可选模型文件 + 可选搜索参数；模型类策略在此解析并
/// 预加载模型（坏文件 / 跨变体权重即报错），搜索参数缺省字段取服务器默认值。
pub async fn create_agent(
    State(app): State<SharedState>,
    Json(req): Json<CreateAgentReq>,
) -> Result<Json<AgentResp>, ApiError> {
    let variant = crate::api::parse_variant_or_err(&req.variant)?;
    if req.kind.eq_ignore_ascii_case("human") {
        return Err(ApiError::BadRequest(
            "Agent 不能为 Human（Human 由对局槽位直接表达）".into(),
        ));
    }
    let kind = StrategyKind::parse(&req.kind).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "非法策略种类: {}（可选 Random / RevealFirst / CaptureFirst / Expectimax / ExpectimaxNnue / MctsOnnx / MctsDL）",
            req.kind
        ))
    })?;
    let params = app.slot_params(req.params.as_ref())?;
    // 模型类策略：解析（缺省按变体自动匹配）+ 预加载 + 布局校验（坏文件即报错）
    let model_file = if kind.needs_model() {
        let f = app.resolve_model_file(req.model_file.as_deref(), variant)?;
        let engine = kind.engine_kind().expect("needs_model() 蕴含模型类策略");
        app.preload_engine_model(engine, f.clone(), variant).await?;
        Some(f)
    } else {
        None
    };
    let agent = app.register_agent(Agent {
        id: Uuid::new_v4(),
        variant,
        kind,
        model_file,
        params,
    });
    tracing::info!(
        agent = %agent.id,
        "创建 Agent: variant={}，kind={}，model={:?}",
        agent.variant.as_str(),
        agent.kind.as_str(),
        agent.model_file
    );
    Ok(Json(agent_view(&agent)))
}

/// GET /api/agents：Agent 列表。
pub async fn list_agents(State(app): State<SharedState>) -> Json<AgentsListResp> {
    let mut agents: Vec<AgentResp> =
        crate::app::sync::read_lock(&app.agents).values().map(agent_view).collect();
    agents.sort_by(|a, b| a.id.cmp(&b.id));
    Json(AgentsListResp { agents })
}

/// GET /api/agents/:id：Agent 详情。
pub async fn get_agent(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<AgentResp>, ApiError> {
    let id = parse_agent_id(&id)?;
    Ok(Json(agent_view(&app.get_agent(id)?)))
}

/// PUT /api/agents/:id/settings：实时调整搜索参数（唯一可变字段）。
///
/// 部分更新：只覆盖给出的字段，返回钳制后的生效快照。对所有绑定该 Agent 的
/// 对局立即生效（决策时实时读取，无建局时快照）。
pub async fn update_agent_settings(
    State(app): State<SharedState>,
    Path(id): Path<String>,
    Json(p): Json<SlotParamsReq>,
) -> Result<Json<AgentResp>, ApiError> {
    let id = parse_agent_id(&id)?;
    let new_params = app.slot_params(Some(&p))?;
    let agent = {
        use crate::app::sync::write_lock;
        let mut agents = write_lock(&app.agents);
        let agent = agents
            .get_mut(&id)
            .ok_or(ApiError::AgentNotFound(id.to_string()))?;
        agent.params = new_params;
        agent.clone()
    };
    tracing::info!(agent = %id, "Agent 搜索参数已更新（实时生效）");
    Ok(Json(agent_view(&agent)))
}

/// DELETE /api/agents/:id：删除 Agent。
///
/// 已绑定到对局槽位的 Agent 删除后，对局决策将得到 404 AGENT_NOT_FOUND
/// （前端应先解除绑定或改绑其他策略）。
pub async fn delete_agent(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_agent_id(&id)?;
    app.remove_agent(id)?;
    tracing::info!(agent = %id, "删除 Agent");
    Ok(StatusCode::NO_CONTENT)
}
