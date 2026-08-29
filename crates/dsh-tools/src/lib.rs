//! dsh-tools — the scoped tool registry and guarded execution pipeline.
//!
//! Mirrors [`packages/core/tools`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/core/tools):
//! a tool is a JSON-schema `parameters` + async `execute`; the registry assembles
//! model-facing `ToolSchema`s and runs the guarded execution pipeline.

use async_trait::async_trait;
use dsh_llm::{CallId, ContentBlock, Disposer, ToolSchema};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// One tool invocation ready to execute.
#[derive(Clone, Debug)]
pub struct ToolExecutionInput {
    pub call_id: CallId,
    pub name: String,
    /// Parsed JSON arguments; preserved as the raw string body when the model
    /// emitted invalid JSON (the reference keeps invalid JSON as text).
    pub arguments: Value,
}

impl ToolExecutionInput {
    /// Parse model arguments, mapping empty input to `{}` and invalid JSON to
    /// the raw string (mirrors the reference `parseArguments`).
    pub fn with_raw_arguments(call_id: CallId, name: String, raw: String) -> Self {
        let arguments = if raw.is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&raw).unwrap_or(Value::String(raw))
        };
        Self { call_id, name, arguments }
    }
}

/// 会话工作目录（web `session.header.cwd` 的共享句柄）：宿主在切换
/// 工作区/会话时更新，工具据此取执行目录与相对路径基准。None = 未设置，
/// 工具回退进程 cwd（与 web 的 `header.cwd ?? process.cwd()` 同语义）。
#[derive(Clone, Debug, Default)]
pub struct Workdir(Arc<std::sync::RwLock<Option<std::path::PathBuf>>>);

impl Workdir {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_value(path: impl Into<std::path::PathBuf>) -> Self {
        Self(Arc::new(std::sync::RwLock::new(Some(path.into()))))
    }

    pub fn set(&self, path: impl Into<std::path::PathBuf>) {
        *self.0.write().unwrap() = Some(path.into());
    }

    pub fn get(&self) -> Option<std::path::PathBuf> {
        self.0.read().unwrap().clone()
    }

    /// 相对路径按工作目录展开；绝对路径原样返回。
    pub fn resolve(&self, path: &std::path::Path) -> std::path::PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            match self.get() {
                Some(wd) => wd.join(path),
                None => path.to_path_buf(),
            }
        }
    }
}

/// The result of one tool invocation.
#[derive(Clone, Debug)]
pub struct ToolExecutionResult {
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    /// When true, the turn concludes after this step (the model is not asked
    /// for another step).
    pub concludes_turn: bool,
}

impl ToolExecutionResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::text(text)], is_error: false, concludes_turn: false }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::text(text)], is_error: true, concludes_turn: false }
    }
}

/// A model-facing tool: declares a JSON-schema definition and executes calls.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool's name, description, and JSON-schema `parameters`.
    fn definition(&self) -> ToolDefinition;

    /// Execute one invocation.
    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult;
}

/// The static part of a tool: name, description, JSON-schema parameters.
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDefinition {
    pub fn to_schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// The scoped tool registry.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Arc<RwLock<HashMap<String, Arc<dyn Tool>>>>,
    order: Arc<RwLock<Vec<String>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under its name, returning its disposer.
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<Disposer, dsh_llm::LlmError> {
        let name = tool.definition().name.clone();
        {
            let map = self.tools.read().unwrap();
            if map.contains_key(&name) {
                return Err(dsh_llm::LlmError::new(
                    format!("tool \"{name}\" is already registered"),
                    "DUPLICATE_TOOL",
                ));
            }
        }
        self.tools.write().unwrap().insert(name.clone(), tool);
        self.order.write().unwrap().push(name.clone());

        let tools = Arc::clone(&self.tools);
        let order = Arc::clone(&self.order);
        Ok(Box::new(move || {
            tools.write().unwrap().remove(&name);
            order.write().unwrap().retain(|n| n != &name);
        }))
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().unwrap().get(name).cloned()
    }

    /// Every registered tool in registration order.
    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        let order = self.order.read().unwrap();
        let map = self.tools.read().unwrap();
        order.iter().filter_map(|n| map.get(n).cloned()).collect()
    }

    /// The model-facing schemas for every registered tool.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.list().iter().map(|t| t.definition().to_schema()).collect()
    }

    /// Run the guarded execute pipeline for one call.
    pub async fn execute(&self, input: &ToolExecutionInput) -> Option<ToolExecutionResult> {
        let tool = self.get(&input.name)?;
        Some(tool.execute(input).await)
    }
}