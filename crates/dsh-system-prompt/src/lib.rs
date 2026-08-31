//! dsh-system-prompt — prompt-section and tool-schema assembly.
//!
//! Mirrors [`packages/core/system-prompt`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/core/system-prompt):
//! collects registered prompt sections and tool schemas into the rendered
//! system string plus the tool set the model sees each step.
//!
//! 0.1.2-alpha.2 对齐：节顺序集中分配（`SECTION_ORDERS` / `CONTEXT_ORDERS`
//! 私有表 + `get_section_order` / `get_context_order` 服务 API）。消费方经
//! 服务查询顺序，不再各自 import 常量（remove cross-package runtime relays）；
//! 渲染按 `(order, name)` 升序拼接（同 order 用 name 的码元序）。

use dsh_llm::ToolSchema;
use dsh_tools::ToolRegistry;
use std::sync::{Arc, RwLock};

/// 仓库自有 prompt 节的集中分配位次名（上游 `PromptSectionOrderName`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptSectionOrderName {
    HarnessIdentity,
    HarnessSource,
    WebSurface,
    DeploymentPersona,
    PlanPolicy,
    TeamPolicy,
    PtcOnly,
    FileReference,
    ToolBash,
    ToolPwsh,
    ToolRead,
    ToolWrite,
    ToolEdit,
    ToolGlob,
    ToolGrep,
    ToolJobs,
    ToolPty,
    ToolWebSearch,
    ToolWebFetch,
    ToolLsp,
    ToolSessionQuery,
    ToolGoal,
    ToolCordis,
    ToolWorkflow,
    ToolRalph,
    ToolSubagent,
    ToolReport,
    ToolsSdk,
    DeliverableFileReferences,
    StructuredOutput,
}

/// 运行时上下文的集中分配位次名（上游 `PromptContextOrderName`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptContextOrderName {
    SandboxPolicy,
    ApprovalPolicy,
    SubagentDelegation,
}

/// 私有序位表（上游 `SECTION_ORDERS`；相邻值至少差 10，让首位冲突可机械检出）。
const SECTION_ORDERS: &[(PromptSectionOrderName, i32)] = &[
    (PromptSectionOrderName::HarnessIdentity, -1000),
    (PromptSectionOrderName::HarnessSource, -900),
    (PromptSectionOrderName::WebSurface, -800),
    (PromptSectionOrderName::DeploymentPersona, 0),
    (PromptSectionOrderName::PlanPolicy, 500),
    (PromptSectionOrderName::TeamPolicy, 600),
    (PromptSectionOrderName::PtcOnly, 800),
    (PromptSectionOrderName::FileReference, 900),
    (PromptSectionOrderName::ToolBash, 1000),
    (PromptSectionOrderName::ToolPwsh, 1010),
    (PromptSectionOrderName::ToolRead, 1100),
    (PromptSectionOrderName::ToolWrite, 1200),
    (PromptSectionOrderName::ToolEdit, 1300),
    (PromptSectionOrderName::ToolGlob, 1400),
    (PromptSectionOrderName::ToolGrep, 1500),
    (PromptSectionOrderName::ToolJobs, 1600),
    (PromptSectionOrderName::ToolPty, 1700),
    (PromptSectionOrderName::ToolWebSearch, 2000),
    (PromptSectionOrderName::ToolWebFetch, 2100),
    (PromptSectionOrderName::ToolLsp, 2200),
    (PromptSectionOrderName::ToolSessionQuery, 2300),
    (PromptSectionOrderName::ToolGoal, 2400),
    (PromptSectionOrderName::ToolCordis, 2500),
    (PromptSectionOrderName::ToolWorkflow, 2600),
    (PromptSectionOrderName::ToolRalph, 2700),
    (PromptSectionOrderName::ToolSubagent, 2800),
    (PromptSectionOrderName::ToolReport, 2900),
    (PromptSectionOrderName::ToolsSdk, 5000),
    (PromptSectionOrderName::DeliverableFileReferences, 9000),
    (PromptSectionOrderName::StructuredOutput, 9900),
];

/// 运行时上下文私有序位表（上游 `CONTEXT_ORDERS`）。
const CONTEXT_ORDERS: &[(PromptContextOrderName, i32)] = &[
    (PromptContextOrderName::SandboxPolicy, 110),
    (PromptContextOrderName::ApprovalPolicy, 115),
    (PromptContextOrderName::SubagentDelegation, 120),
];

/// One named contribution to the rendered system prompt.
#[derive(Clone, Debug)]
pub struct PromptSection {
    pub name: String,
    /// 节的渲染序位：升序拼接，同 order 用 name 码元序（上游同契约）。
    pub order: i32,
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

    /// 解析仓库 prompt 节的集中分配序位（上游 `SystemPrompt.getSectionOrder`）。
    pub fn get_section_order(&self, name: PromptSectionOrderName) -> i32 {
        SECTION_ORDERS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, order)| *order)
            .unwrap_or(0)
    }

    /// 解析仓库运行时上下文的集中分配序位（上游 `getContextOrder`）。
    pub fn get_context_order(&self, name: PromptContextOrderName) -> i32 {
        CONTEXT_ORDERS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, order)| *order)
            .unwrap_or(0)
    }

    /// Add a prompt section, returning a disposer. Sections render in
    /// ascending `(order, name)` order.
    pub fn add_section(&self, section: PromptSection) -> dsh_llm::Disposer {
        let sections = Arc::clone(&self.sections);
        let name = section.name.clone();
        sections.write().unwrap().push(section);
        Box::new(move || {
            sections.write().unwrap().retain(|s| s.name != name);
        })
    }

    /// Render the joined system text from the registered sections, sorted by
    /// `(order, name)`.
    pub fn render(&self) -> String {
        let mut sections = self.sections.read().unwrap().clone();
        sections.sort_by(|a, b| (a.order, &a.name).cmp(&(b.order, &b.name)));
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
