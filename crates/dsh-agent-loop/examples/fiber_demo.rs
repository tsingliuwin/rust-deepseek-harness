//! Demonstrates Cordis fiber lifecycle: `inject` dependency ordering and
//! patchable config.
//!
//! Base services (`LlmRuntime`, `ToolRegistry`, `SystemPrompt`) are provided,
//! then plugins are mounted with `inject` declared. The «greeter» plugin is
//! listed BEFORE its provider, so `mount_all` defers it until the
//! «greeter-provider» plugin provides the `Greeter` service. A patch replaces
//! the «prompt» plugin's config before mount, so the assembled system prompt
//! reflects the patched text.

use async_trait::async_trait;
use dsh_cordis::{Context, Patch, Plugin, PluginError, PluginManager, PluginRow, Scope};
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError, LlmProviderInfo,
    LlmRuntime, StreamChunk,
};
use dsh_system_prompt::{PromptSection, SystemPrompt};
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult, ToolRegistry};
use futures::stream;
use serde_json::{Value, json};
use std::any::TypeId;
use std::sync::Arc;

// ---------- services --------------------------------------------------------

struct Greeter(String);

// ---------- plugins ---------------------------------------------------------

struct ModelAdapterPlugin;

impl Plugin for ModelAdapterPlugin {
    fn id(&self) -> &str {
        "adapter"
    }

    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<LlmRuntime>()]
    }

    fn apply(&self, scope: &Scope, config: Option<&Value>) -> Result<(), PluginError> {
        let llm = scope.get::<LlmRuntime>().expect("LlmRuntime");
        let model = config
            .and_then(|c| c.get("model"))
            .and_then(|v| v.as_str())
            .unwrap_or("mock")
            .to_string();
        let disposer = llm
            .register_adapter(&["mock".to_string()], Arc::new(MockAdapter { model }))
            .map_err(|e| PluginError(e.message))?;
        scope.effect(dsh_cordis::Effect::from_box(disposer));
        Ok(())
    }
}

struct CalculatorPlugin;

impl Plugin for CalculatorPlugin {
    fn id(&self) -> &str {
        "calculator"
    }

    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<ToolRegistry>()]
    }

    fn apply(&self, scope: &Scope, _config: Option<&Value>) -> Result<(), PluginError> {
        let tools = scope.get::<ToolRegistry>().expect("ToolRegistry");
        let disposer = tools
            .register(Arc::new(CalculatorTool))
            .map_err(|e| PluginError(e.message))?;
        scope.effect(dsh_cordis::Effect::from_box(disposer));
        Ok(())
    }
}

struct PromptPlugin;

impl Plugin for PromptPlugin {
    fn id(&self) -> &str {
        "prompt"
    }

    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<SystemPrompt>()]
    }

    fn apply(&self, scope: &Scope, config: Option<&Value>) -> Result<(), PluginError> {
        let prompt = scope.get::<SystemPrompt>().expect("SystemPrompt");
        let text = config
            .and_then(|c| c.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("(default prompt)")
            .to_string();
        let disposer = prompt.add_section(PromptSection { name: "prompt-plugin".into(), order: 0, text });
        scope.effect(dsh_cordis::Effect::from_box(disposer));
        Ok(())
    }
}

struct GreeterPlugin;

impl Plugin for GreeterPlugin {
    fn id(&self) -> &str {
        "greeter"
    }

    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<Greeter>()]
    }

    fn apply(&self, scope: &Scope, _config: Option<&Value>) -> Result<(), PluginError> {
        let greeter = scope.get::<Greeter>().expect("Greeter");
        let prompt = scope.get::<SystemPrompt>().expect("SystemPrompt");
        let disposer = prompt.add_section(PromptSection {
            name: "greeter-plugin".into(),
            order: 0,
            text: format!("greeting: {}", greeter.0),
        });
        scope.effect(dsh_cordis::Effect::from_box(disposer));
        Ok(())
    }
}

struct GreeterProviderPlugin;

impl Plugin for GreeterProviderPlugin {
    fn id(&self) -> &str {
        "greeter-provider"
    }

    fn apply(&self, scope: &Scope, _config: Option<&Value>) -> Result<(), PluginError> {
        scope.provide(Greeter("hello from an injected service".into()));
        Ok(())
    }
}

// ---------- tool / adapter --------------------------------------------------

struct CalculatorTool;

#[async_trait]
impl Tool for CalculatorTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "calculator".into(),
            description: "Add two integers".into(),
            parameters: json!({ "type": "object", "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } }, "required": ["a", "b"] }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let a = input.arguments.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
        let b = input.arguments.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
        ToolExecutionResult::text(format!("{}", a + b))
    }
}

struct MockAdapter {
    #[allow(dead_code)]
    model: String,
}

#[async_trait]
impl LlmAdapter for MockAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, _options: GenerateOptions) -> Result<BoxStream, LlmError> {
        Ok(Box::pin(stream::iter(vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: format!("(model {})", self.model) },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ])))
    }
}

// ---------- main ------------------------------------------------------------

fn main() {
    let ctx = Context::new();
    ctx.provide_arc(Arc::new(LlmRuntime::with_events(ctx.events().clone())));
    ctx.provide_arc(Arc::new(ToolRegistry::new()));
    ctx.provide_arc(Arc::new(SystemPrompt::new()));

    // Note: «greeter» (which injects `Greeter`) is listed before its provider.
    let rows = vec![
        PluginRow::new("greeter", Arc::new(GreeterPlugin)),
        PluginRow::new("adapter", Arc::new(ModelAdapterPlugin)).with_config(json!({ "model": "deepseek-chat" })),
        PluginRow::new("calculator", Arc::new(CalculatorPlugin)),
        PluginRow::new("prompt", Arc::new(PromptPlugin)).with_config(json!({ "text": "BEFORE PATCH" })),
        PluginRow::new("greeter-provider", Arc::new(GreeterProviderPlugin)),
    ];

    // Patchable config: replace the prompt plugin's text before mount.
    let rows = PluginManager::patch(rows, vec![Patch::Replace {
        id: "prompt".into(),
        config: json!({ "text": "AFTER PATCH" }),
    }]);

    let manager = PluginManager::new();
    manager.mount_all(&ctx, rows).expect("mount all");

    // Verify: tool registered, prompt reflects both the patch and the injected service.
    let tools = ctx.get::<ToolRegistry>().unwrap();
    let names: Vec<String> = tools.schemas().into_iter().map(|t| t.name).collect();
    println!("tools: {names:?}");

    let prompt = ctx.get::<SystemPrompt>().unwrap();
    let rendered = prompt.render();
    println!("system prompt:\n{rendered}");
    assert!(names.contains(&"calculator".to_string()));
    assert!(rendered.contains("AFTER PATCH"), "patch must replace config");
    assert!(rendered.contains("hello from an injected service"), "injected service must be resolved");
    assert!(!rendered.contains("BEFORE PATCH"), "pre-patch config must not survive");

    manager.dispose_all();
    println!("\nOK");
}