//! dsh-system-prompt — prompt-section and tool-schema assembly.
//!
//! Mirrors [`packages/core/system-prompt`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/core/system-prompt):
//! collects registered prompt sections and tool schemas into the rendered
//! system string plus the tool set the model sees each step.

use dsh_llm::ToolSchema;
use dsh_tools::ToolRegistry;
use std::sync::{Arc, RwLock};

/// One named contribution to the rendered system prompt.
#[derive(Clone, Debug)]
pub struct PromptSection {
    pub name: String,
    pub text: String,
}

/// The assembled prompt: rendered system text plus the authoritative tools.
#[derive(Clone, Debug, Default)]
pub struct PromptAssembly {
    pub system: String,
    pub tools: Vec<ToolSchema>,
}

/// The system-prompt service: prompt sections contributed by plugins.
#[derive(Default)]
pub struct SystemPrompt {
    sections: Arc<RwLock<Vec<PromptSection>>>,
}

impl SystemPrompt {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a prompt section (later sections render later), returning a disposer.
    pub fn add_section(&self, section: PromptSection) -> dsh_llm::Disposer {
        let sections = Arc::clone(&self.sections);
        let name = section.name.clone();
        sections.write().unwrap().push(section);
        Box::new(move || {
            sections.write().unwrap().retain(|s| s.name != name);
        })
    }

    /// Render the joined system text from the registered sections.
    pub fn render(&self) -> String {
        let sections = self.sections.read().unwrap();
        sections
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Assemble the full prompt for one step: rendered system text plus the
    /// tool schemas contributed by the registry.
    pub fn assemble(&self, tools: Vec<ToolSchema>) -> PromptAssembly {
        PromptAssembly { system: self.render(), tools }
    }
}

/// Convenience: assemble from a tool registry.
pub fn assemble_from_registry(prompt: &SystemPrompt, registry: &ToolRegistry) -> PromptAssembly {
    prompt.assemble(registry.schemas())
}