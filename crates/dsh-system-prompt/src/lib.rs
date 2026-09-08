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
//! 0.1.3-alpha.2 对齐：harness:source/web:surface 移到序位 10000/10100
//! （环境事实跟在可复用指令后），persona 拆前缀（0）/后缀（10200）两节。

use dsh_llm::ToolSchema;

pub mod agent_instructions;
use dsh_tools::ToolRegistry;
use std::sync::{Arc, RwLock};

/// 仓库自有 prompt 节的集中分配位次名（上游 `PromptSectionOrderName`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptSectionOrderName {
    HarnessIdentity,
    HarnessSource,
    WebSurface,
    DeploymentPersonaPrefix,
    DeploymentPersonaSuffix,
    WorkspaceInstructions,
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
/// 0.1.3-alpha.2 对齐：本地路径/端点（harness:source、web:surface）与 persona
/// 后缀移到可复用指令之后（e28862db57）；persona 拆前缀（order 0，仅 identity
/// 之后）+ 后缀（order 10200）两节（40792330c0）。
const SECTION_ORDERS: &[(PromptSectionOrderName, i32)] = &[
    (PromptSectionOrderName::HarnessIdentity, -1000),
    (PromptSectionOrderName::DeploymentPersonaPrefix, 0),
    (PromptSectionOrderName::WorkspaceInstructions, 400),
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
    (PromptSectionOrderName::HarnessSource, 10000),
    (PromptSectionOrderName::WebSurface, 10100),
    (PromptSectionOrderName::DeploymentPersonaSuffix, 10200),
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
    /// `(order, name)`. Empty-text sections (占位待重建的动态节) contribute
    /// nothing rather than stray blank lines.
    pub fn render(&self) -> String {
        let mut sections = self.sections.read().unwrap().clone();
        sections.sort_by(|a, b| (a.order, &a.name).cmp(&(b.order, &b.name)));
        sections
            .iter()
            .filter(|s| !s.text.trim().is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_instructions_order_sits_between_persona_prefix_and_plan() {
        let prompt = SystemPrompt::new();
        let prefix = prompt.get_section_order(PromptSectionOrderName::DeploymentPersonaPrefix);
        let ws = prompt.get_section_order(PromptSectionOrderName::WorkspaceInstructions);
        let bash = prompt.get_section_order(PromptSectionOrderName::ToolBash);
        assert!(prefix < ws && ws < bash, "{prefix} < {ws} < {bash}");
    }

    /// 0.1.3-alpha.2：本地路径/端点与 persona 后缀在可复用指令之后
    /// （e28862db57 + 40792330c0）。
    #[test]
    fn local_facts_and_persona_suffix_follow_reusable_instructions() {
        let prompt = SystemPrompt::new();
        let structured = prompt.get_section_order(PromptSectionOrderName::StructuredOutput);
        let source = prompt.get_section_order(PromptSectionOrderName::HarnessSource);
        let web = prompt.get_section_order(PromptSectionOrderName::WebSurface);
        let suffix = prompt.get_section_order(PromptSectionOrderName::DeploymentPersonaSuffix);
        assert!(
            structured < source && source < web && web < suffix,
            "{structured} < {source} < {web} < {suffix}"
        );
    }

    #[test]
    fn render_skips_empty_sections_and_replaces_by_dispose() {
        let prompt = SystemPrompt::new();
        let disposer = prompt.add_section(PromptSection {
            name: "workspace:instructions".into(),
            order: 400,
            text: String::new(),
        });
        assert!(!prompt.render().contains("workspace rules"));
        disposer();
        prompt.add_section(PromptSection {
            name: "workspace:instructions".into(),
            order: 400,
            text: "workspace rules".into(),
        });
        assert!(prompt.render().contains("workspace rules"));
    }
}
