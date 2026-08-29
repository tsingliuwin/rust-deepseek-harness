//! dsh-subagent — 子 agent 工具（对齐 `packages/subagent` 的进程内分支）。
//!
//! 参考实现把子 agent 生命周期拆成 spawn/driver/control/report 多个包并支持
//! 后台运行；本实现取其前台默认路径的语义：`subagent` 工具以全新会话
//! （in-process fork）驱动一个子 ReactLoopAgent 跑完任务，等它结束并把
//! 末条助手消息作为工具结果返回。深度护栏（默认 2 层）防止子 agent 递归
//! 生子失控；超时（默认 300s）到点取消并报错。

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::event::EventBus;
use dsh_llm::types::ContentBlock;
use dsh_llm::LlmRuntime;
use dsh_session::{Session, SessionEvent};
use dsh_system_prompt::SystemPrompt;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};

/// `subagent` 工具配置。
pub struct SubagentTool {
    llm: Arc<LlmRuntime>,
    tools: Arc<dsh_tools::ToolRegistry>,
    prompt: Arc<SystemPrompt>,
    /// 子 agent 路由（默认继承父路由；宿主在切换模型时经
    /// [`Self::set_route`] 同步更新）。
    route: Arc<std::sync::RwLock<(String, String)>>,
    /// 会话工作目录（子 agent 与父共享同一句柄 → 自动继承）
    workdir: dsh_tools::Workdir,
    max_tokens: Option<u32>,
    system_prompt: Option<String>,
    /// 全局活着的子 agent 深度（跨嵌套共享）。
    depth: Arc<AtomicU32>,
    /// 允许的最大嵌套层数。
    max_depth: u32,
    /// 单次子任务等待上限。
    timeout: Duration,
}

impl SubagentTool {
    pub fn new(
        llm: Arc<LlmRuntime>,
        tools: Arc<dsh_tools::ToolRegistry>,
        prompt: Arc<SystemPrompt>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            llm,
            tools,
            prompt,
            route: Arc::new(std::sync::RwLock::new((provider.into(), model.into()))),
            workdir: dsh_tools::Workdir::new(),
            max_tokens: None,
            system_prompt: None,
            depth: Arc::new(AtomicU32::new(0)),
            max_depth: 2,
            timeout: Duration::from_secs(300),
        })
    }

    /// 覆盖子 agent 的基础 system prompt 与 token 上限。
    pub fn with_system_prompt(mut self: Arc<Self>, system_prompt: Option<String>) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.system_prompt = system_prompt;
        }
        self
    }

    pub fn with_max_tokens(mut self: Arc<Self>, max_tokens: Option<u32>) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.max_tokens = max_tokens;
        }
        self
    }

    pub fn with_max_depth(mut self: Arc<Self>, max_depth: u32) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.max_depth = max_depth;
        }
        self
    }

    pub fn with_timeout(mut self: Arc<Self>, timeout: Duration) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.timeout = timeout;
        }
        self
    }

    /// 宿主路由切换时同步子 agent 路由。
    pub fn set_route(&self, provider: impl Into<String>, model: impl Into<String>) {
        *self.route.write().unwrap() = (provider.into(), model.into());
    }

    /// 注入会话工作目录句柄（Workdir 是共享句柄：宿主更新同一对象，
    /// 子 agent 自动跟随）。
    pub fn with_workdir(mut self: Arc<Self>, workdir: dsh_tools::Workdir) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.workdir = workdir;
        }
        self
    }
}

/// 从子会话日志取末条带文本的助手消息（子任务的最终答复）。
fn final_assistant_text(session: &Session) -> Option<String> {
    session
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.event {
            SessionEvent::AssistantMessage { message, .. } => {
                let text: String = message
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if text.trim().is_empty() { None } else { Some(text) }
            }
            _ => None,
        })
}

#[async_trait]
impl Tool for SubagentTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "subagent".into(),
            description: "Delegate a self-contained task to a fresh subagent that runs in the background of this \
conversation with its own context window and the same tools. It cannot see this conversation; write the \
prompt as a complete brief. This call waits for the subagent and returns its final report."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "A short (3-5 word) description of the delegated task, for display." },
                    "prompt": { "type": "string", "description": "The complete task brief for the subagent: goal, relevant context, and the expected shape of the result." }
                },
                "required": ["description", "prompt"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let prompt = input
            .arguments
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if prompt.trim().is_empty() {
            return ToolExecutionResult::error("prompt must be a non-empty string");
        }
        if self.depth.load(Ordering::SeqCst) >= self.max_depth {
            return ToolExecutionResult::error(format!(
                "subagent depth limit reached ({}); run the task inline instead",
                self.max_depth
            ));
        }

        let (provider, model) = self.route.read().unwrap().clone();
        let options = AgentOptions {
            provider,
            model,
            max_tokens: self.max_tokens,
            system_prompt: self.system_prompt.clone(),
            compaction: Default::default(),
            workdir: self.workdir.clone(),
        };
        let child = ReactLoopAgent::new(
            dsh_llm::types::SessionId::new(uuid::Uuid::new_v4().to_string()),
            options,
            Arc::clone(&self.llm),
            Arc::clone(&self.tools),
            Arc::clone(&self.prompt),
            EventBus::new(),
        );
        let _events = child.subscribe(); // 子事件隔离：留接收端防背压，不转发
        child.spawn();
        self.depth.fetch_add(1, Ordering::SeqCst);
        child.followup(prompt);
        let waited = tokio::time::timeout(self.timeout, child.when_idle()).await;
        self.depth.fetch_sub(1, Ordering::SeqCst);
        match waited {
            Err(_) => {
                child.cancel();
                ToolExecutionResult::error(format!(
                    "subagent timed out after {}s; its work was cancelled",
                    self.timeout.as_secs()
                ))
            }
            Ok(()) => {
                let text = final_assistant_text(&child.session().lock().unwrap());
                match text {
                    Some(text) => {
                        let description = input
                            .arguments
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("task");
                        ToolExecutionResult::text(format!("Subagent ({description}) final report:\n\n{text}"))
                    }
                    None => ToolExecutionResult::error("subagent produced no final message"),
                }
            }
        }
    }
}
