use crate::app::player::{Agent, StrategyKind};
use crate::dto::{AgentResp, AgentsListResp, CreateAgentReq, SlotParamsReq};
use crate::error::ApiError;
use crate::state::{SharedState, parse_id, strategy_slot_view};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use uuid::Uuid;
fn parse_agent_id(raw: &str) -> Result<Uuid, ApiError> {
    parse_id(raw, "Agent ID")
}
fn agent_view(agent: &Agent) -> AgentResp {
    AgentResp {
        id: agent.id.to_string(),
        variant: agent.variant.as_str().to_string(),
        slot: strategy_slot_view(agent.kind, agent.model_file.clone(), agent.params),
    }
}
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
pub async fn list_agents(State(app): State<SharedState>) -> Json<AgentsListResp> {
    let mut agents: Vec<AgentResp> = crate::app::sync::read_lock(&app.agents)
        .values()
        .map(agent_view)
        .collect();
    agents.sort_by(|a, b| a.id.cmp(&b.id));
    Json(AgentsListResp { agents })
}
pub async fn get_agent(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<AgentResp>, ApiError> {
    let id = parse_agent_id(&id)?;
    Ok(Json(agent_view(&app.get_agent(id)?)))
}
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
pub async fn delete_agent(
    State(app): State<SharedState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_agent_id(&id)?;
    app.remove_agent(id)?;
    tracing::info!(agent = %id, "删除 Agent");
    Ok(StatusCode::NO_CONTENT)
}
