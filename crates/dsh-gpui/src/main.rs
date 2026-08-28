//! dsh-gpui — 原生 GPUI 客户端，1:1 对齐参考 dsh Web UI。
//!
//! 布局契约（`packages/client/ui-layout`）：左 sidebar（280px，可折叠成 56px
//! 图标栏，<1024 自动折叠）· 中会话区（≥640px，内容列 748px）· 右 details
//! （360px，300–520 可拖）。视觉 token 见 `theme.rs`（对齐
//! `packages/client/ui-theme` 暗色主题）。会话区 = header（标题 + tab）+
//! 消息流（用户气泡 / Think 折叠行 / 工具行 / markdown）+ 底部胶囊输入卡。

mod assets;
pub(crate) mod layout;
mod settings;
mod theme;
pub(crate) mod widgets;

use crate::layout::*;
use crate::widgets::*;

use async_stream::stream;
use async_trait::async_trait;
use dsh_agent_loop::{AgentEvent, AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_fs::FsTool;
use dsh_llm::{
    BoxStream, ContentBlock, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, Message, MessageSource, SessionId, StreamChunk,
};
use dsh_llm_deepseek::DeepSeekAdapter;
use dsh_persist::SessionRecorder;
use dsh_session::{Session, SessionEvent};
use dsh_shell::ShellTool;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use dsh_web::WebTool;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{Icon, IconName, Root, StyledExt};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

// --- 参考 ui-layout/columns.ts 列宽契约 ---------------------------------------

// --- 消息块模型 ---------------------------------------------------------------

/// 工具调用块（web 版 ToolRow）。
struct ToolBlock {
    id: String,
    name: String,
    arguments: String,
    result: Option<String>,
    error: bool,
    open: bool,
}

enum MsgBlock {
    Text(String),
    Reasoning { text: String, open: bool },
    Tool(ToolBlock),
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    User,
    Assistant,
    Error,
}

struct ChatEntry {
    role: Role,
    blocks: Vec<MsgBlock>,
    done: bool,
    /// 回合用时（TurnStarted → TurnEnded）。
    elapsed: Option<Duration>,
}

/// One session shown in the sidebar list.
#[derive(Clone)]
struct SessionMeta {
    id: SessionId,
    title: String,
    /// 相对时间（「刚刚 / 6分钟 / 8天」，由 JSONL 的 mtime 计算）。
    time_label: String,
}

/// 详情面板当前选中的工具调用。
struct ToolDetail {
    name: String,
    arguments: String,
    result: Option<String>,
    error: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum CenterTab {
    Conversation,
    Trajectory,
}



// --- 根视图 ------------------------------------------------------------------

/// 设置页签。
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SettingsTab {
    General,
    Models,
    Plugins,
    Presets,
}

/// 模型页添加卡的两种来源（web：adopt known / declare custom）。
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum AddingMode {
    None,
    Adopt,
    Declare,
}

/// 外观模式（通用设置 → 外观分段）。
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum AppearanceMode {
    Light,
    Dark,
    #[serde(rename = "system")]
    System,
}

/// 智能体运行中按 Enter 的行为（通用设置）。
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum EnterBehavior {
    /// 排队（投递到 inbox，当前轮结束后处理）。
    #[serde(rename = "queue")]
    Queue,
    /// 打断（cancel 当前轮后立即处理）。
    #[serde(rename = "interrupt")]
    Interrupt,
}

/// 持久化到 settings.json 的用户设置。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct AppSettings {
    pub(crate) appearance: AppearanceMode,
    pub(crate) enter: EnterBehavior,
    pub(crate) model: String,
    /// 用户声明的 OpenAI 兼容提供方（web 模型页「添加提供方 / 添加自定义提供方」）。
    #[serde(default)]
    pub(crate) providers: Vec<CustomProvider>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            appearance: AppearanceMode::System,
            enter: EnterBehavior::Queue,
            model: "deepseek-chat".into(),
            providers: Vec::new(),
        }
    }
}

/// 一个自定义（OpenAI 兼容）提供方声明。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CustomProvider {
    /// 路由标识（唯一，小写字母开头）。
    pub(crate) id: String,
    /// 显示名。
    pub(crate) name: String,
    /// OpenAI 兼容 base URL（…/v1）。
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    /// 线协议（当前恒为 openai——对齐 web pi-ai 的协议字段）。
    #[serde(default = "default_protocol")]
    pub(crate) protocol: String,
    /// 模型目录（至少一项；路由默认使用第一项）。
    #[serde(default)]
    pub(crate) models: Vec<CustomModel>,
}

fn default_protocol() -> String {
    "openai".into()
}

/// 模型目录中的一行（web ModelListEditor 的 modelEntry）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CustomModel {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) context_window: String,
    #[serde(default)]
    pub(crate) max_tokens: String,
}

/// 「添加提供方」的内置目录（OpenAI 兼容、可被 adopt 的提供方）。
pub(crate) struct ProviderCatalogEntry {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) base_url: &'static str,
    pub(crate) model: &'static str,
}

pub(crate) const PROVIDER_CATALOG: &[ProviderCatalogEntry] = &[
    ProviderCatalogEntry { id: "amazon-bedrock", name: "Amazon Bedrock", base_url: "", model: "" },
    ProviderCatalogEntry { id: "ant-ling", name: "Ant Ling", base_url: "", model: "" },
    ProviderCatalogEntry { id: "anthropic", name: "Anthropic", base_url: "https://api.anthropic.com/v1", model: "claude-3-7-sonnet-latest" },
    ProviderCatalogEntry { id: "azure-openai-responses", name: "Azure OpenAI", base_url: "", model: "" },
    ProviderCatalogEntry { id: "cerebras", name: "Cerebras", base_url: "", model: "" },
    ProviderCatalogEntry { id: "cloudflare-ai-gateway", name: "Cloudflare AI Gateway", base_url: "", model: "" },
    ProviderCatalogEntry { id: "cloudflare-workers-ai", name: "Cloudflare Workers AI", base_url: "", model: "" },
    ProviderCatalogEntry { id: "fireworks", name: "Fireworks", base_url: "", model: "" },
    ProviderCatalogEntry { id: "github-copilot", name: "GitHub Copilot", base_url: "", model: "" },
    ProviderCatalogEntry { id: "google", name: "Google", base_url: "https://generativelanguage.googleapis.com/v1beta/openai", model: "gemini-2.5-flash" },
    ProviderCatalogEntry { id: "google-vertex", name: "Google Vertex", base_url: "", model: "" },
    ProviderCatalogEntry { id: "groq", name: "Groq", base_url: "https://api.groq.com/openai/v1", model: "llama-3.3-70b-versatile" },
    ProviderCatalogEntry { id: "huggingface", name: "Hugging Face", base_url: "", model: "" },
    ProviderCatalogEntry { id: "kimi-coding", name: "Kimi Coding", base_url: "", model: "" },
    ProviderCatalogEntry { id: "minimax", name: "MiniMax", base_url: "https://api.minimax.chat/v1", model: "" },
    ProviderCatalogEntry { id: "minimax-cn", name: "MiniMax CN", base_url: "https://api.minimaxi.com/v1", model: "" },
    ProviderCatalogEntry { id: "mistral", name: "Mistral", base_url: "https://api.mistral.ai/v1", model: "mistral-large-latest" },
    ProviderCatalogEntry { id: "moonshotai", name: "Moonshot AI", base_url: "https://api.moonshot.ai/v1", model: "kimi-k2-0905-preview" },
    ProviderCatalogEntry { id: "moonshotai-cn", name: "Moonshot AI CN", base_url: "https://api.moonshot.cn/v1", model: "kimi-k2-0905-preview" },
    ProviderCatalogEntry { id: "nvidia", name: "NVIDIA", base_url: "", model: "" },
    ProviderCatalogEntry { id: "openai", name: "OpenAI", base_url: "https://api.openai.com/v1", model: "gpt-4o-mini" },
    ProviderCatalogEntry { id: "openai-codex", name: "OpenAI Codex", base_url: "", model: "" },
    ProviderCatalogEntry { id: "opencode", name: "OpenCode", base_url: "", model: "" },
    ProviderCatalogEntry { id: "opencode-go", name: "OpenCode Go", base_url: "", model: "" },
    ProviderCatalogEntry { id: "openrouter", name: "OpenRouter", base_url: "https://openrouter.ai/api/v1", model: "" },
    ProviderCatalogEntry { id: "qwen-token-plan", name: "Qwen Token Plan", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "" },
    ProviderCatalogEntry { id: "together", name: "Together", base_url: "https://api.together.xyz/v1", model: "" },
    ProviderCatalogEntry { id: "vercel-ai-gateway", name: "Vercel AI Gateway", base_url: "", model: "" },
    ProviderCatalogEntry { id: "xai", name: "xAI", base_url: "https://api.x.ai/v1", model: "grok-4" },
    ProviderCatalogEntry { id: "xiaomi", name: "Xiaomi", base_url: "", model: "" },
    ProviderCatalogEntry { id: "xiaomi-token-plan-ams", name: "Xiaomi Token Plan AMS", base_url: "", model: "" },
    ProviderCatalogEntry { id: "xiaomi-token-plan-cn", name: "Xiaomi Token Plan CN", base_url: "", model: "" },
    ProviderCatalogEntry { id: "xiaomi-token-plan-sgp", name: "Xiaomi Token Plan SGP", base_url: "", model: "" },
    ProviderCatalogEntry { id: "zai", name: "ZAI", base_url: "", model: "" },
];

/// Harness home（对齐 dsh home-paths）：`$DSH_HOME` 优先（空白视为未设），
/// 否则 `~/.dsh`。所有用户数据在这一个根下。
pub(crate) fn dsh_home() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("DSH_HOME") {
        let p = p.trim();
        if !p.is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".dsh")
}

/// settings.yaml —— 与 web 版 dsh 共享同一份配置文档（YAML）。
/// 只读写我们拥有的段，其余段原样保留。
pub(crate) fn settings_path() -> std::path::PathBuf {
    dsh_home().join("settings.yaml")
}

/// 用系统默认关联程序直接打开 settings.yaml。
pub(crate) fn open_settings_file() {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", &settings_path().to_string_lossy()])
        .spawn();
    #[cfg(not(target_os = "windows"))]
    let _ = std::process::Command::new("xdg-open")
        .arg(settings_path())
        .spawn();
}

/// .credentials.yaml —— 与 web 共享的密钥文档：{version: 1, refs: {REF: key}}。
fn credentials_path() -> std::path::PathBuf {
    dsh_home().join(".credentials.yaml")
}

/// 会话目录（默认 `{home}/sessions`；DSH_SESSIONS_DIR 显式设置时覆盖）。
pub(crate) fn sessions_dir() -> std::path::PathBuf {
    std::env::var("DSH_SESSIONS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dsh_home().join("sessions"))
}

/// 读 settings.yaml 为 YAML Value（不存在则空 map）。
fn load_settings_doc() -> serde_yaml::Value {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_yaml::from_str(&s).ok())
        .unwrap_or(serde_yaml::Value::Mapping(Default::default()))
}

fn save_settings_doc(doc: &serde_yaml::Value) {
    let _ = std::fs::create_dir_all(dsh_home());
    if let Ok(text) = serde_yaml::to_string(doc) {
        let _ = std::fs::write(settings_path(), text);
    }
}

/// 读 .credentials.yaml 的 refs：REF 名 -> 密钥。
fn load_credentials() -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(credentials_path())
        .ok()
        .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).ok())
        .and_then(|d| d.get("refs").cloned())
        .and_then(|r| {
            r.as_mapping().map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        Some((k.as_str()?.to_string(), v.as_str()?.to_string()))
                    })
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// 写 .credentials.yaml（幂等合并 refs，保留既有条目与 version）。
fn save_credentials(patch: &[(String, String)]) {
    let mut doc = std::fs::read_to_string(credentials_path())
        .ok()
        .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).ok())
        .unwrap_or_else(|| serde_yaml::Value::Mapping(Default::default()));
    if doc.get("version").is_none() {
        if let serde_yaml::Value::Mapping(m) = &mut doc {
            m.insert("version".into(), serde_yaml::Value::Number(1.into()));
        }
    }
    if let serde_yaml::Value::Mapping(m) = &mut doc {
        let entry = m
            .entry("refs".into())
            .or_insert(serde_yaml::Value::Mapping(Default::default()));
        if let serde_yaml::Value::Mapping(refs) = entry {
            for (k, v) in patch {
                refs.insert(k.clone().into(), serde_yaml::Value::String(v.clone()));
            }
        }
    }
    let _ = std::fs::create_dir_all(dsh_home());
    if let Ok(text) = serde_yaml::to_string(&doc) {
        let _ = std::fs::write(credentials_path(), text);
    }
}

/// web deriveKeyRef：大写 route、非字母数字转 `_`、后缀 _API_KEY。
fn derive_key_ref(route: &str) -> String {
    let mut out = String::new();
    for c in route.chars() {
        if c.is_ascii_alphanumeric() && !c.is_ascii_digit() {
            out.push(c.to_ascii_uppercase());
        } else if c.is_ascii_digit() {
            out.push(c);
        } else {
            if !out.ends_with('_') {
                out.push('_');
            }
        }
    }
    format!("{}_API_KEY", out.trim_end_matches('_'))
}

/// 从 settings.yaml 构建内存态用户设置（只读我们拥有的段）。
fn load_user_config() -> (AppSettings, String, String, bool, String) {
    let doc = load_settings_doc();
    let get = |path: &[&str]| -> Option<serde_yaml::Value> {
        let mut cur = &doc;
        for key in path {
            cur = cur.get(*key)?;
        }
        Some(cur.clone())
    };
    let mut settings = AppSettings::default();

    // 外观：ui-theme.preference
    if let Some(v) = get(&["ui-theme", "preference"]).and_then(|v| v.as_str().map(|s| s.to_string())) {
        settings.appearance = match v.as_str() {
            "light" => AppearanceMode::Light,
            "dark" => AppearanceMode::Dark,
            _ => AppearanceMode::System,
        };
    }
    // Enter：ui-conversation.busyEnter
    if let Some(v) = get(&["ui-conversation", "busyEnter"]).and_then(|v| v.as_str().map(|s| s.to_string())) {
        settings.enter = if v == "steer" { EnterBehavior::Interrupt } else { EnterBehavior::Queue };
    }
    // providers：llm-pi-ai.providers.<route>
    let credentials = load_credentials();
    if let Some(provs) = get(&["llm-pi-ai", "providers"]).and_then(|v| v.as_mapping().cloned()) {
        for (route, def) in provs {
            let route = route.as_str().unwrap_or_default().to_string();
            if route.is_empty() {
                continue;
            }
            let str_at = |k: &str| def.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
            let key_env = str_at("apiKeyEnv").unwrap_or_default();
            let api_key = credentials.get(&key_env).cloned().unwrap_or_default();
            let models = def
                .get("models")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|m| {
                            Some(CustomModel {
                                id: m.get("id")?.as_str()?.to_string(),
                                display_name: m.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string(),
                                context_window: m.get("contextWindow").and_then(|n| n.as_u64()).map(|n| n.to_string()).unwrap_or_default(),
                                max_tokens: m.get("maxTokens").and_then(|n| n.as_u64()).map(|n| n.to_string()).unwrap_or_default(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            settings.providers.push(CustomProvider {
                id: route.clone(),
                name: str_at("displayName").unwrap_or_else(|| route.clone()),
                base_url: str_at("baseURL").unwrap_or_default(),
                api_key,
                protocol: str_at("api").unwrap_or_else(|| "openai-completions".into()),
                models,
            });
        }
    }
    // 当前模型：agent-default-model
    let mut active = "deepseek".to_string();
    let mut desired = settings.model.clone();
    if let Some(p) = get(&["agent-default-model", "provider"]).and_then(|v| v.as_str().map(|s| s.to_string())) {
        if p == "deepseek-official" {
            active = "deepseek".into();
        } else if settings.providers.iter().any(|x| x.id == p) {
            active = p.clone();
            desired = settings
                .providers
                .iter()
                .find(|x| x.id == p)
                .and_then(|x| x.models.first())
                .map(|m| m.id.clone())
                .unwrap_or_default();
        }
    }
    if let Some(m) = get(&["agent-default-model", "model"]).and_then(|v| v.as_str().map(|s| s.to_string())) {
        desired = m;
    }
    settings.model = desired.clone();

    let stored_deepseek_key = credentials.get("DEEPSEEK_API_KEY").cloned().unwrap_or_default();
    let deepseek_env_locked = DeepSeekAdapter::from_env().is_some();
    (settings, active, desired, deepseek_env_locked, stored_deepseek_key)
}

/// 持久化：patch settings.yaml 与 .credentials.yaml（其余段原样）。
fn persist_user_config(
    settings: &AppSettings,
    active_provider: &str,
    desired_model: &str,
    deepseek_key: &str,
    env_key_locked: bool,
) {
    let doc = load_settings_doc();
    // 以现有文档的 mapping 为基底（保留 ui-onboarding/locale 等其它段）
    let mapping = doc.as_mapping().cloned().unwrap_or_default();

    // providers：全量替换 llm-pi-ai.providers
    let mut providers = serde_yaml::Mapping::new();
    for p in &settings.providers {
        let mut def = serde_yaml::Mapping::new();
        if !p.api_key.is_empty() {
            def.insert("apiKeyEnv".into(), derive_key_ref(&p.id).into());
        }
        if !p.name.is_empty() && p.name != p.id {
            def.insert("displayName".into(), p.name.clone().into());
        }
        if !p.base_url.is_empty() {
            def.insert("baseURL".into(), p.base_url.clone().into());
        }
        def.insert("api".into(), "openai-completions".into());
        let models: Vec<serde_yaml::Value> = p
            .models
            .iter()
            .map(|m| {
                let mut row = serde_yaml::Mapping::new();
                row.insert("id".into(), m.id.clone().into());
                if !m.display_name.is_empty() {
                    row.insert("name".into(), m.display_name.clone().into());
                } else {
                    row.insert("name".into(), m.id.clone().into());
                }
                if !m.context_window.is_empty() {
                    if let Ok(n) = m.context_window.parse::<u64>() {
                        row.insert("contextWindow".into(), n.into());
                    }
                }
                if !m.max_tokens.is_empty() {
                    if let Ok(n) = m.max_tokens.parse::<u64>() {
                        row.insert("maxTokens".into(), n.into());
                    }
                }
                serde_yaml::Value::Mapping(row)
            })
            .collect();
        def.insert("models".into(), serde_yaml::Value::Sequence(models));
        providers.insert(serde_yaml::Value::String(p.id.clone()), serde_yaml::Value::Mapping(def));
    }
    let pi_block = {
        let mut pi = serde_yaml::Mapping::new();
        pi.insert("providers".into(), serde_yaml::Value::Mapping(providers));
        serde_yaml::Value::Mapping(pi)
    };

    let mut mapping = mapping;
    mapping.insert("llm-pi-ai".into(), pi_block);
    // agent-default-model：内部 "deepseek" -> web route "deepseek-official"
    let route = if active_provider == "deepseek" { "deepseek-official" } else { active_provider };
    let mut adm = serde_yaml::Mapping::new();
    adm.insert("provider".into(), route.into());
    adm.insert("model".into(), desired_model.to_string().into());
    mapping.insert("agent-default-model".into(), serde_yaml::Value::Mapping(adm));
    // Enter 行为：ui-conversation.busyEnter
    let mut conv = serde_yaml::Mapping::new();
    conv.insert(
        "busyEnter".into(),
        match settings.enter {
            EnterBehavior::Interrupt => "steer",
            EnterBehavior::Queue => "queue",
        }
        .into(),
    );
    mapping.insert("ui-conversation".into(), serde_yaml::Value::Mapping(conv));
    // 外观：ui-theme.preference
    let mut theme = serde_yaml::Mapping::new();
    theme.insert(
        "preference".into(),
        match settings.appearance {
            AppearanceMode::Light => "light",
            AppearanceMode::Dark => "dark",
            AppearanceMode::System => "system",
        }
        .into(),
    );
    mapping.insert("ui-theme".into(), serde_yaml::Value::Mapping(theme));

    save_settings_doc(&serde_yaml::Value::Mapping(mapping));

    // 密钥：.credentials.yaml refs
    let mut creds: Vec<(String, String)> = Vec::new();
    if !deepseek_key.is_empty() && !env_key_locked {
        creds.push(("DEEPSEEK_API_KEY".into(), deepseek_key.to_string()));
    }
    for p in &settings.providers {
        if !p.api_key.is_empty() {
            creds.push((derive_key_ref(&p.id), p.api_key.clone()));
        }
    }
    if !creds.is_empty() {
        save_credentials(&creds);
    }
}

/// 旧版 settings.json / credentials.json 一次性并入 YAML 后删除。
fn migrate_legacy() {
    // settings.yaml 已经存在（web 版或本版此前写过）→ 视为已同步，跳过迁移。
    if settings_path().exists() {
        return;
    }
    let legacy_settings = dsh_home().join("settings.json");
    let legacy_creds = dsh_home().join("credentials.json");
    let old_local = std::env::var("LOCALAPPDATA")
        .ok()
        .map(|p| std::path::PathBuf::from(p).join("dsh-rust"));
    let mut fj = legacy_settings.clone();
    if !fj.exists()
        && let Some(l) = &old_local
    {
        fj = l.join("settings.json");
    }
    if !fj.exists() {
        return;
    }
    // 解析旧 json 设置
    let Ok(raw) = std::fs::read_to_string(&fj) else { return };
    let Ok(raw_j) = serde_json::from_str(&raw) else { return };
    let mut old: serde_json::Value = raw_j;
    let mut settings = AppSettings::default();
    if let Some(a) = old.get("appearance").and_then(|v| v.as_str()) {
        settings.appearance = match a {
            "Light" | "light" => AppearanceMode::Light,
            "Dark" | "dark" => AppearanceMode::Dark,
            _ => AppearanceMode::System,
        };
    }
    if let Some(e) = old.get("enter").and_then(|v| v.as_str()) {
        settings.enter = if e == "interrupt" || e == "steer" { EnterBehavior::Interrupt } else { EnterBehavior::Queue };
    }
    if let Some(m) = old.get("model").and_then(|v| v.as_str()) {
        settings.model = m.to_string();
    }
    let mut old_keys: std::collections::HashMap<String, String> = Default::default();
    if let Some(provs) = old.get_mut("providers").and_then(|p| p.as_array_mut()) {
        for p in provs.iter_mut() {
            let id = p.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
            if id.is_empty() {
                continue;
            }
            let key = p.get("api_key").and_then(|k| k.as_str()).unwrap_or_default().to_string();
            if !key.is_empty() {
                old_keys.insert(id.clone(), key.clone());
            }
            settings.providers.push(CustomProvider {
                id: id.clone(),
                name: p.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string(),
                base_url: p.get("base_url").and_then(|b| b.as_str()).unwrap_or_default().to_string(),
                api_key: key,
                protocol: p.get("protocol").and_then(|x| x.as_str()).unwrap_or("openai-completions").to_string(),
                models: p
                    .get("models")
                    .and_then(|v| v.as_array())
                    .map(|seq| {
                        seq.iter()
                            .filter_map(|m| {
                                Some(CustomModel {
                                    id: m.get("id")?.as_str()?.to_string(),
                                    display_name: m.get("display_name").and_then(|n| n.as_str()).unwrap_or_default().to_string(),
                                    context_window: m.get("context_window").and_then(|n| n.as_str()).unwrap_or_default().to_string(),
                                    max_tokens: m.get("max_tokens").and_then(|n| n.as_str()).unwrap_or_default().to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }
    }
    // 旧 credentials.json 的 key 合并
    let mut creds_files = vec![legacy_creds.clone()];
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let lc = std::path::PathBuf::from(local).join("dsh-rust").join("credentials.json");
        if lc.exists() {
            creds_files.push(lc);
        }
    }
    for cf in creds_files {
        if let Ok(rawc) = std::fs::read_to_string(cf) {
            if let Ok(_doc) = serde_json::from_str::<serde_json::Value>(&rawc) {
                if let Some(o) = _doc.as_object() {
                    for (k, v) in o {
                        if let Some(sv) = v.as_str() {
                            old_keys.insert(k.clone(), sv.to_string());
                        }
                    }
                }
            }
        }
    }
    if false {
    if let Ok(rawc) = std::fs::read_to_string(legacy_creds.clone()) {
        if let Ok(_doc) = serde_json::from_str::<serde_json::Value>(&rawc) {
            }
        }
    }
    // deepseek key
    let deepseek_key = old_keys.remove("deepseek").unwrap_or_default();
    for (id, key) in &old_keys {
        if let Some(p) = settings.providers.iter_mut().find(|p| &p.id == id) {
            p.api_key = key.clone();
        }
    }
    // 写 yaml（providers 的 keys 会经 derive_key_ref 进 credentials）
    persist_user_config(&settings, "deepseek", &settings.model, &deepseek_key, false);
    let _ = std::fs::remove_file(&legacy_settings);
    let _ = std::fs::remove_file(&legacy_creds);
}

/// AppView 的运行期依赖（打包传入以控制构造参数个数）。
struct AppDeps {
    recorder: Arc<SessionRecorder>,
    llm: Arc<LlmRuntime>,
}

struct AppView {
    agent: Arc<ReactLoopAgent>,
    recorder: Arc<SessionRecorder>,
    llm: Arc<LlmRuntime>,
    sessions: Vec<SessionMeta>,
    entries: Vec<ChatEntry>,
    input: Entity<InputState>,
    #[allow(dead_code)] // 被 DeepSeek 卡引用
    api_input: Entity<InputState>,
    desired_model: String,
    active_provider: String,
    settings_open: bool,
    settings_tab: SettingsTab,
    settings: AppSettings,
    llm_configured: bool,
    /// DEEPSEEK_API_KEY 由启动环境提供（web keyEnvLocked：只读）。
    env_key_locked: bool,
    /// 用户输入的 DeepSeek 密钥（内存态；落盘在 credentials.json）。
    deepseek_key: String,
    // 模型页添加/编辑卡状态（对齐 web ModelsSection 的 adding/declaring/editing）
    adding: AddingMode,
    adopt_pick: usize,
    adopt_dropdown_open: bool,
    adopt_customized_open: bool,
    edit_customized_open: bool,
    editing_provider: Option<String>,
    /// 待删除确认（web deleteDialog：删除前弹确认）。
    confirm_delete: Option<String>,
    /// 「创建提供方」的失败原因（web 卡内 error 行）。
    declare_error: Option<String>,
    // adopt 卡输入
    adopt_key: Entity<InputState>,
    adopt_base: Entity<InputState>,
    // declare（自定义提供方）卡：已添加的模型行
    dc_models: Vec<String>,
    dc_route: Entity<InputState>,
    dc_name: Entity<InputState>,
    dc_base: Entity<InputState>,
    dc_key: Entity<InputState>,
    dc_new_model: Entity<InputState>,
    // 编辑卡输入
    edit_key: Entity<InputState>,
    edit_base: Entity<InputState>,
    pending_clear: bool,
    _input_subscription: Subscription,
    chat_scroll: ScrollHandle,
    // 布局状态
    sidebar_collapsed: bool,
    sidebar_width: f32,
    details_open: bool,
    details_width: f32,
    viewport: f32,
    // 会话运行态
    running: bool,
    turn_started_at: Option<Instant>,
    stats_turns: u64,
    stats_tools: u64,
    // 中栏 tab + 详情选中
    tab: CenterTab,
    selected_tool: Option<ToolDetail>,
}

impl AppView {
    fn new(
        agent: Arc<ReactLoopAgent>,
        deps: AppDeps,
        sessions: Vec<SessionMeta>,
        input: Entity<InputState>,
        #[allow(dead_code)] // 被 DeepSeek 卡引用
    api_input: Entity<InputState>,
        desired_model: String,
        active_provider: String,
        settings: AppSettings,
        llm_configured: bool,
        env_key_locked: bool,
        deepseek_key: String,
        adopt_key: Entity<InputState>,
        adopt_base: Entity<InputState>,
        dc_route: Entity<InputState>,
        dc_name: Entity<InputState>,
        dc_base: Entity<InputState>,
        dc_key: Entity<InputState>,
        dc_new_model: Entity<InputState>,
        edit_key: Entity<InputState>,
        edit_base: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&input, |chat, input, event, cx| {
            if matches!(event, InputEvent::PressEnter { secondary: false }) {
                let text: String = input.read_with(cx, |s, _| s.value().to_string());
                let text = text.trim().to_string();
                if !text.is_empty() {
                    chat.push_user(text.clone());
                    chat.agent.followup(text);
                    chat.pending_clear = true;
                    cx.notify();
                }
            }
        });
        let mut view = Self {
            agent,
            recorder: deps.recorder,
            llm: deps.llm,
            sessions,
            entries: Vec::new(),
            input,
            api_input,
            desired_model,
            active_provider,
            settings_open: false,
            settings_tab: SettingsTab::General,
            settings,
            llm_configured,
            env_key_locked,
            deepseek_key,
            adding: AddingMode::None,
            adopt_pick: 0,
            adopt_dropdown_open: false,
            adopt_customized_open: false,
            edit_customized_open: false,
            editing_provider: None,
            confirm_delete: None,
            declare_error: None,
            adopt_key,
            adopt_base,
            dc_models: Vec::new(),
            dc_route,
            dc_name,
            dc_base,
            dc_key,
            dc_new_model,
            edit_key,
            edit_base,
            pending_clear: false,
            _input_subscription: subscription,
            chat_scroll: ScrollHandle::new(),
            sidebar_collapsed: false,
            sidebar_width: SIDEBAR_DEFAULT,
            details_open: true,
            details_width: DETAILS_DEFAULT,
            viewport: 1280.0,
            running: false,
            turn_started_at: None,
            stats_turns: 0,
            stats_tools: 0,
            tab: CenterTab::Conversation,
            selected_tool: None,
        };
        view.rebuild_from_session();
        view
    }

    /// Rebuild the transcript from the agent's session log (restore/switch).
    fn rebuild_from_session(&mut self) {
        self.entries.clear();
        self.running = false;
        self.turn_started_at = None;
        self.selected_tool = None;
        let session = self.agent.session();
        let session = session.lock().unwrap();
        for entry in session.entries() {
            match &entry.event {
                SessionEvent::UserMessage(m) => {
                    let text: String = m
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    if !text.is_empty() {
                        self.entries.push(ChatEntry {
                            role: Role::User,
                            blocks: vec![MsgBlock::Text(text)],
                            done: true,
                            elapsed: None,
                        });
                    }
                }
                SessionEvent::AssistantMessage { message, .. } => {
                    let mut blocks = Vec::new();
                    for b in &message.content {
                        match b {
                            ContentBlock::Text { text } => blocks.push(MsgBlock::Text(text.clone())),
                            ContentBlock::Reasoning { text } => {
                                blocks.push(MsgBlock::Reasoning { text: text.clone(), open: false })
                            }
                            ContentBlock::ToolCall { id, name, arguments } => {
                                blocks.push(MsgBlock::Tool(ToolBlock {
                                    id: id.0.clone(),
                                    name: name.clone(),
                                    arguments: arguments.clone(),
                                    result: None,
                                    error: false,
                                    open: false,
                                }))
                            }
                            _ => {}
                        }
                    }
                    if !blocks.is_empty() {
                        self.entries.push(ChatEntry {
                            role: Role::Assistant,
                            blocks,
                            done: true,
                            elapsed: None,
                        });
                    }
                }
                SessionEvent::ToolResult { message, .. } => {
                    if let Some(ContentBlock::ToolResult { tool_call_id, content, is_error }) =
                        message.content.first()
                    {
                        let result_text: String = content
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();
                        attach_tool_result(self.entries.last_mut(), &tool_call_id.0, &result_text, is_error.unwrap_or(false));
                    }
                }
                _ => {}
            }
        }
    }

    /// Switch the agent to a persisted session and rebuild the transcript.
    fn switch_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if let Ok(session) = self.recorder.load(&id) {
            self.agent.set_session(session);
            self.rebuild_from_session();
            self.stats_turns = 0;
            self.stats_tools = 0;
            self.tab = CenterTab::Conversation;
            self.chat_scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    /// Create a fresh session and make it current.
    fn new_session(&mut self, cx: &mut Context<Self>) {
        let id = SessionId::new(uuid::Uuid::new_v4().to_string());
        self.agent.set_session(Session::new(id.clone()));
        self.entries.clear();
        self.running = false;
        self.turn_started_at = None;
        self.selected_tool = None;
        self.stats_turns = 0;
        self.stats_tools = 0;
        self.tab = CenterTab::Conversation;
        self.chat_scroll.scroll_to_bottom();
        self.sessions.insert(0, SessionMeta { id, title: "新会话".into(), time_label: "刚刚".into() });
        cx.notify();
    }

/// 消息流当前是否贴底（offset 向下滚动趋于 -max，阈值 40px）。
    fn chat_near_bottom(&self) -> bool {
        self.chat_scroll.offset().y <= -self.chat_scroll.max_offset().height + px(40.0)
    }

    fn current_session_id(&self) -> SessionId {
        self.agent.session().lock().unwrap().id.clone()
    }

    fn is_empty_session(&self) -> bool {
        self.entries.is_empty()
    }

    fn push_user(&mut self, text: String) {
        // 会话标题：首条用户消息后自动生成（截断到 30 字符）
        let current = self.current_session_id();
        if let Some(meta) = self.sessions.iter_mut().find(|s| s.id == current)
            && meta.title == "新会话"
            && !text.is_empty()
        {
            meta.title = text.chars().take(30).collect();
        }
        self.entries.push(ChatEntry {
            role: Role::User,
            blocks: vec![MsgBlock::Text(text)],
            done: true,
            elapsed: None,
        });
        self.chat_scroll.scroll_to_bottom();
    }

    fn last_assistant(&mut self) -> &mut ChatEntry {
        let new = !matches!(self.entries.last(), Some(e) if e.role == Role::Assistant && !e.done);
        if new {
            self.entries.push(ChatEntry {
                role: Role::Assistant,
                blocks: Vec::new(),
                done: false,
                elapsed: None,
            });
        }
        self.entries.last_mut().unwrap()
    }

    fn push_text(&mut self, text: &str) {
        let blocks = &mut self.last_assistant().blocks;
        match blocks.last_mut() {
            Some(MsgBlock::Text(t)) => t.push_str(text),
            _ => blocks.push(MsgBlock::Text(text.to_string())),
        }
    }

    fn push_reasoning(&mut self, text: &str) {
        let blocks = &mut self.last_assistant().blocks;
        match blocks.last_mut() {
            Some(MsgBlock::Reasoning { text: t, .. }) => t.push_str(text),
            _ => blocks.push(MsgBlock::Reasoning { text: text.to_string(), open: false }),
        }
    }

    fn push_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::TurnStarted { .. } => {
                self.running = true;
                self.turn_started_at = Some(Instant::now());
                self.stats_turns += 1;
            }
            AgentEvent::TextDelta { text } => self.push_text(&text),
            AgentEvent::ReasoningDelta { text } => self.push_reasoning(&text),
            AgentEvent::ToolCall { tool_call_id, name, arguments: args } => {
                self.stats_tools += 1;
                self.last_assistant().blocks.push(MsgBlock::Tool(ToolBlock {
                    id: tool_call_id.0,
                    name,
                    arguments: args,
                    result: None,
                    error: false,
                    open: false,
                }));
            }
            AgentEvent::ToolResult { tool_call_id, is_error } => {
                // 实时结果文本从会话日志回读（事件本身只带 id/error）。
                let result = self.latest_tool_result_text(&tool_call_id.0);
                let last = self.entries.last_mut();
                attach_tool_result(last, &tool_call_id.0, result.0.as_str(), is_error);
            }
            AgentEvent::AssistantMessage { .. } => {}
            AgentEvent::TurnEnded { .. } => {
                self.running = false;
                let elapsed = self.turn_started_at.map(|t| t.elapsed());
                if let Some(e) = self.entries.last_mut() {
                    e.done = true;
                    e.elapsed = elapsed;
                }
                self.turn_started_at = None;
            }
            AgentEvent::Error { message, .. } => {
                self.running = false;
                self.turn_started_at = None;
                self.entries.push(ChatEntry {
                    role: Role::Error,
                    blocks: vec![MsgBlock::Text(message)],
                    done: true,
                    elapsed: None,
                });
            }
        }
        // 智能吸底：只有用户本来就贴在底部时才跟随滚动（web 同款行为）；
        // 用户上翻阅读时不再被流式输出拽走。
        if self.chat_near_bottom() {
            self.chat_scroll.scroll_to_bottom();
        }
    }

    /// 从会话日志回读指定工具调用的最新结果文本。
    fn latest_tool_result_text(&self, call_id: &str) -> (String, bool) {
        let session = self.agent.session();
        let session = session.lock().unwrap();
        for entry in session.entries().iter().rev() {
            if let SessionEvent::ToolResult { message, .. } = &entry.event
                && let Some(ContentBlock::ToolResult { tool_call_id, content, is_error }) =
                    message.content.first()
                && tool_call_id.0 == call_id
            {
                let text: String = content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                return (text, is_error.unwrap_or(false));
            }
        }
        (String::new(), false)
    }

    /// 发送按钮路径：读输入框 → 追加 → 清空（window 由 on_click 闭包提供）。
    fn send_from_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text: String =
            self.input.read_with(cx, |s, _| s.value().to_string()).trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.running {
            // 通用设置「繁忙时 Enter 行为」：排队投递，或打断当前轮
            match self.settings.enter {
                EnterBehavior::Queue => {}
                EnterBehavior::Interrupt => self.agent.cancel(),
            }
        }
        self.push_user(text.clone());
        self.agent.followup(text);
        self.input.update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    // --- 设置 ----------------------------------------------------------------

    /// 解析 System 外观为实际亮/暗（跟随系统时每帧按窗口外观校正）。
    fn effective_appearance(&self, window: &Window) -> gpui_component::ThemeMode {
        match self.settings.appearance {
            AppearanceMode::Light => gpui_component::ThemeMode::Light,
            AppearanceMode::Dark => gpui_component::ThemeMode::Dark,
            AppearanceMode::System => match window.appearance() {
                gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark => {
                    gpui_component::ThemeMode::Dark
                }
                _ => gpui_component::ThemeMode::Light,
            },
        }
    }

    fn set_appearance(&mut self, mode: AppearanceMode, cx: &mut Context<Self>) {
        self.settings.appearance = mode;
        self.persist_settings();
        // 非跟随系统：立即应用；System 由 render 每帧按窗口外观校正
        match mode {
            AppearanceMode::Light => theme::apply(gpui_component::ThemeMode::Light, cx),
            AppearanceMode::Dark => theme::apply(gpui_component::ThemeMode::Dark, cx),
            AppearanceMode::System => {}
        }
        cx.notify();
    }

    fn set_enter_behavior(&mut self, behavior: EnterBehavior, cx: &mut Context<Self>) {
        self.settings.enter = behavior;
        self.persist_settings();
        cx.notify();
    }

    /// 模型页「保存并启用」：注册 DeepSeek adapter 并切换路由。
    #[allow(dead_code)] // 保留：未来 on-demand 保存路径
    fn apply_api_key(&mut self, cx: &App) {
        let key: String = self.api_input.read_with(cx, |s, _| s.value().to_string());
        let key = key.trim().to_string();
        if key.is_empty() {
            return;
        }
        let adapter = DeepSeekAdapter::new(key.clone());
        let _ = self.llm.register_adapter(&["deepseek".to_string()], Arc::new(adapter));
        self.agent.set_provider_and_model("deepseek", self.desired_model.clone());
        self.llm_configured = true;
        self.deepseek_key = key;
        self.settings.model = self.desired_model.clone();
        self.persist_settings();
    }

    /// 添加自定义提供方：注册 adapter + 入 settings + 启用。
    /// adopt：从目录添加一个已知提供方（web addCard + ProviderEditor 的 apply）。
    /// 返回错误文案；Ok(()) 表示已保存。
    fn adopt_provider(&mut self, cx: &App) -> Result<(), String> {
        let entry = &PROVIDER_CATALOG[self.adopt_pick.min(PROVIDER_CATALOG.len() - 1)];
        if self.settings.providers.iter().any(|p| p.id == entry.id) || entry.id == "deepseek" {
            return Err("已有提供方使用了这个 ID。".into());
        }
        let key = self.adopt_key.read_with(cx, |s, _| s.value().trim().to_string());
        let base = {
            let v = self.adopt_base.read_with(cx, |s, _| s.value().trim().to_string());
            if v.is_empty() { entry.base_url.to_string() } else { v }
        };
        let provider = CustomProvider {
            id: entry.id.to_string(),
            name: entry.name.to_string(),
            base_url: base,
            api_key: key,
            protocol: "openai".into(),
            models: vec![CustomModel {
                id: entry.model.to_string(),
                display_name: String::new(),
                context_window: String::new(),
                max_tokens: String::new(),
            }],
        };
        self.register_custom(&provider);
        self.settings.providers.push(provider);
        self.persist_settings();
        Ok(())
    }

    /// declare：创建自定义提供方（web CustomProviderCard 的 create）。
    fn declare_provider(&mut self, cx: &App) -> Result<(), String> {
        let route = self.dc_route.read_with(cx, |s, _| s.value().trim().to_string());
        let base = self.dc_base.read_with(cx, |s, _| s.value().trim().to_string());
        let key = self.dc_key.read_with(cx, |s, _| s.value().trim().to_string());
        let name = self.dc_name.read_with(cx, |s, _| s.value().trim().to_string());
        if route.is_empty() {
            return Err("以小写字母开头的标识，在请求中唯一标识该提供方，并用于派生凭据名。".into());
        }
        if !valid_route_id(&route) {
            return Err("需以小写字母开头，之后可用小写字母、数字和短横线。".into());
        }
        if self.settings.providers.iter().any(|p| p.id == route) || route == "deepseek" {
            return Err("已有提供方使用了这个 ID。".into());
        }
        if base.is_empty() {
            return Err("自定义提供方需要填写 API 地址。".into());
        }
        if self.dc_models.is_empty() {
            return Err("自定义提供方至少需要一个模型。".into());
        }
        let provider = CustomProvider {
            id: route,
            name: if name.is_empty() { String::new() } else { name },
            base_url: base,
            api_key: key,
            protocol: "openai".into(),
            models: self
                .dc_models
                .iter()
                .map(|id| CustomModel {
                    id: id.clone(),
                    display_name: String::new(),
                    context_window: String::new(),
                    max_tokens: String::new(),
                })
                .collect(),
        };
        self.register_custom(&provider);
        self.settings.providers.push(provider);
        self.persist_settings();
        Ok(())
    }

    /// 注册一个自定义提供方的 adapter（OpenAI 兼容）。
    fn register_custom(&self, p: &CustomProvider) {
        let adapter = DeepSeekAdapter::with_base_url(&p.api_key, &p.base_url);
        let _ = self.llm.register_adapter(&[p.id.clone()], Arc::new(adapter));
    }

    /// 向 declare 卡追加一行模型（查重）。
    fn dc_push_model(&mut self, cx: &App) {
        let id = self.dc_new_model.read_with(cx, |s, _| s.value().trim().to_string());
        if !id.is_empty() && !self.dc_models.contains(&id) {
            self.dc_models.push(id);
        }
    }

    /// 编辑卡保存（web ProviderEditor apply：换 key / 改 API 地址）。
    fn save_edit(&mut self, cx: &App) {
        let Some(id) = self.editing_provider.clone() else { return };
        let key = self.edit_key.read_with(cx, |s, _| s.value().trim().to_string());
        let base = self.edit_base.read_with(cx, |s, _| s.value().trim().to_string());
        if id == "deepseek" {
            if !key.is_empty() {
                let adapter = DeepSeekAdapter::new(&key);
                let _ = self.llm.register_adapter(&["deepseek".to_string()], Arc::new(adapter));
                self.llm_configured = true;
            }
        } else if let Some(p) = self.settings.providers.iter_mut().find(|p| p.id == id) {
            let mut changed = false;
            if !key.is_empty() {
                p.api_key = key;
                changed = true;
            }
            if !base.is_empty() {
                p.base_url = base;
                changed = true;
            }
            if changed {
                let snapshot = p.clone();
                self.register_custom(&snapshot);
            }
        }
        self.persist_settings();
        self.editing_provider = None;
    }

    /// 切换当前 provider 路由（composer 模型选择器接入后使用；
    /// web 模型页没有启用按钮，选择在 composer 完成）。
    #[allow(dead_code)]
    fn activate_provider(&mut self, id: &str) {
        if id == "deepseek" {
            self.agent.set_provider_and_model("deepseek", self.desired_model.clone());
            self.active_provider = "deepseek".into();
            self.llm_configured = true;
        } else if let Some(p) = self.settings.providers.iter().find(|p| p.id == id).cloned() {
            let model = p.models.first().map(|m| m.id.clone()).unwrap_or_default();
            self.agent.set_provider_and_model(p.id.clone(), model.clone());
            self.active_provider = p.id;
            self.desired_model = model;
            self.llm_configured = true;
        }
        self.persist_settings();
    }

    /// 删除自定义提供方（若激活中则回退 deepseek）。
    fn remove_provider(&mut self, id: &str) {
        self.settings.providers.retain(|p| p.id != id);
        if self.active_provider == id {
            self.active_provider = "deepseek".into();
            self.agent.set_provider_and_model("deepseek", self.desired_model.clone());
        }
        self.persist_settings();
    }


    /// 写盘：settings.yaml + .credentials.yaml（与 web 共享同一份文档）。
    fn persist_settings(&self) {
        persist_user_config(
            &self.settings,
            &self.active_provider,
            &self.desired_model,
            &self.deepseek_key,
            self.env_key_locked,
        );
    }

    // --- 渲染 ---------------------------------------------------------------

    fn block_element(&self, block: &MsgBlock, ei: usize, bi: usize, this: &Entity<AppView>) -> AnyElement {
        match block {
            MsgBlock::Text(t) => div()
                .w_full()
                .text_color(theme::t().text)
                .child(MarkdownBlock { text: t.clone(), id: 1_000_000 + ei * 1000 + bi })
                .into_any_element(),

            MsgBlock::Reasoning { text, open } => {
                let open = *open;
                let t = this.clone();
                let active = self.running
                    && !self.entries.get(ei).map(|e| e.done).unwrap_or(true);
                let sweep_ms = if active {
                    self.turn_started_at.map(|t| t.elapsed().as_millis() as u64)
                } else {
                    None
                };
                let mut row = div()
                    .id(("think-row", (ei * 1000 + bi) as u64))
                    .relative()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .gap_1p5()
                    .cursor_pointer()
                    .rounded(px(6.0))
                    .hover(|s| s.bg(theme::t().hover))
                    .on_click(move |_, _, cx| {
                        t.update(cx, |v, cx| {
                            if let Some(MsgBlock::Reasoning { open: o, .. }) =
                                v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                            {
                                *o = !*o;
                            }
                            cx.notify();
                        });
                    })
                    .child(
                        Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                            .size(px(12.0))
                            .text_color(theme::t().text_2),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text)
                            .child("Think"),
                    )
                    .child(dot_sep())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text_3)
                            .child(first_line(text)),
                    );
                if open {
                    row = row.child(
                        div()
                            .pt_1()
                            .pb_1()
                            .pl(px(22.0))
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text_3)
                            .child(text.clone()),
                    );
                }
                if let Some(ms) = sweep_ms {
                    row = row.child(row_sweep(ms, CHAT_CONTENT_WIDTH));
                }
                div().w_full().child(row).into_any_element()
            }

            MsgBlock::Tool(tool) => {
                let open = tool.open;
                let (label, icon) = tool_display(&tool.name);
                let t = this.clone();
                let running = tool.result.is_none();
                let sweep_ms = if running {
                    self.turn_started_at.map(|t| t.elapsed().as_millis() as u64)
                } else {
                    None
                };
                let mut row = div()
                    .id(("tool-row", (ei * 1000 + bi) as u64))
                    .relative()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .gap_1p5()
                    .cursor_pointer()
                    .rounded(px(6.0))
                    .hover(|s| s.bg(theme::t().hover))
                    .on_click(move |_, _, cx| {
                        t.update(cx, |v, cx| {
                            if let Some(MsgBlock::Tool(tool)) =
                                v.entries.get_mut(ei).and_then(|e| e.blocks.get_mut(bi))
                            {
                                tool.open = !tool.open;
                                v.selected_tool = Some(ToolDetail {
                                    name: tool.name.clone(),
                                    arguments: tool.arguments.clone(),
                                    result: tool.result.clone(),
                                    error: tool.error,
                                });
                                v.details_open = true;
                            }
                            cx.notify();
                        });
                    })
                    .child(
                        Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                            .size(px(12.0))
                            .text_color(theme::t().text_2),
                    )
                    .when(tool.error && tool.result.is_some(), |r| {
                        // web ToolRow leadingFor：终态 error 用状态点替换工具图标
                        r.child(state_dot(theme::t().error))
                    })
                    .when(!(tool.error && tool.result.is_some()), |r| {
                        r.child(Icon::new(icon).size(px(14.0)).text_color(theme::t().text_2))
                    })
                    .child(
                        div()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(theme::t().text)
                            .child(label),
                    )
                    .child(dot_sep())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(theme::FONT_ROW_LEADING))
                            .text_color(if tool.error { theme::t().error } else { theme::t().text_3 })
                            .child(first_line(&tool.arguments)),
                    );
                if open {
                    row = row.child(io_card(
                        (ei * 1000 + bi) as u64,
                        &tool.arguments,
                        tool.result.as_deref(),
                        tool.error,
                    ));
                }
                if let Some(ms) = sweep_ms {
                    row = row.child(row_sweep(ms, CHAT_CONTENT_WIDTH));
                }
                div().w_full().child(row).into_any_element()
            }
        }
    }

    fn render_entry(&self, entry: &ChatEntry, ei: usize, this: &Entity<AppView>) -> Div {
        match entry.role {
            Role::User => {
                let text = entry
                    .blocks
                    .first()
                    .map(|b| match b {
                        MsgBlock::Text(t) => t.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                let t = this.clone();
                let bubble_text = text.clone();
                let group: SharedString = format!("user-msg-{ei}").into();
                let group_copy = group.clone();
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(6.0))
                    .group(group)
                    .child(
                        div()
                            .max_w(px(USER_BUBBLE_MAX))
                            .rounded(px(22.0))
                            .bg(theme::t().surface)
                            .px_4()
                            .py(px(10.0))
                            .text_size(px(theme::FONT_BUBBLE))
                            .line_height(px(theme::FONT_BUBBLE_LEADING))
                            .text_color(theme::t().text)
                            
                            .child(bubble_text),
                    )
                    .child(
                        // 气泡下方的复制按钮（web MessageIconActions：悬停显现）
                        div()
                            .id(("copy-user", ei as u64))
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .text_color(theme::t().caption)
                            .opacity(0.0)
                            .group_hover(group_copy, |s| s.opacity(1.0))
                            .hover(|s| s.text_color(theme::t().text_2).bg(theme::t().hover))
                            .tooltip(tip("复制"))
                            .on_click(move |_, _, cx| {
                                let text = text.clone();
                                t.update(cx, |_, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                                });
                            })
                            .child(Icon::new(IconName::Copy).size(px(14.0))),
                    )
            }
            Role::Assistant => {
                let group: SharedString = format!("assistant-msg-{ei}").into();
                let mut col = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .group(group);
                for (bi, block) in entry.blocks.iter().enumerate() {
                    col = col.child(self.block_element(block, ei, bi, this));
                }
                if entry.done && let Some(elapsed) = entry.elapsed {
                    col = col.child(render_entry_footer(elapsed, this, ei));
                }
                div().w_full().child(col)
            }
            Role::Error => {
                let text = entry
                    .blocks
                    .first()
                    .map(|b| match b {
                        MsgBlock::Text(t) => t.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                div().w_full().child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_size(px(13.0))
                        .line_height(px(20.0))
                        .child(
                            div()
                                .mt(px(6.0))
                                .size(px(8.0))
                                .rounded_full()
                                .flex_none()
                                .bg(theme::t().error),
                        )
                        .child(
                            div().child(
                                div()
                                    .text_color(theme::t().error)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("出错了"),
                            ),
                        )
                        .child(
                            div().flex_1().min_w_0().text_color(theme::t().text_2).child(text),
                        ),
                )
            }
        }
    }

    /// 消息流底部进行中的状态行（web ChatView .turnStatus）。
    fn render_status_line(&self) -> Div {
        div().w_full().child(
            div()
                .h(px(26.0))
                .flex()
                .items_center()
                .child(
                    div()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::t().accent)
                        .child("Deep diving…"),
                )
                .children(self.turn_started_at.map(|t| {
                    div()
                        .ml_2()
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(theme::t().caption)
                        .child(format!("{}秒", t.elapsed().as_secs()))
                })),
        )
    }

    /// 轨迹 tab：全量工具调用台账。
    fn render_trajectory(&self, this: &Entity<AppView>) -> Div {
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ei, entry) in self.entries.iter().enumerate() {
            for (bi, block) in entry.blocks.iter().enumerate() {
                if let MsgBlock::Tool(tool) = block {
                    let (label, icon) = tool_display(&tool.name);
                    let t = this.clone();
                    let name = tool.name.clone();
                    let arguments = tool.arguments.clone();
                    let result = tool.result.clone();
                    let error = tool.error;
                    rows.push(
                        div()
                            .id(("traj", (ei * 1000 + bi) as u64))
                            .flex()
                            .items_center()
                            .h(px(32.0))
                            .px_2()
                            .gap_1p5()
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::t().hover))
                            .on_click(move |_, _, cx| {
                                t.update(cx, |v, cx| {
                                    v.selected_tool = Some(ToolDetail {
                                        name: name.clone(),
                                        arguments: arguments.clone(),
                                        result: result.clone(),
                                        error,
                                    });
                                    v.details_open = true;
                                    cx.notify();
                                });
                            })
                            .when(tool.error && tool.result.is_some(), |r| {
                        // web ToolRow leadingFor：终态 error 用状态点替换工具图标
                        r.child(state_dot(theme::t().error))
                    })
                    .when(!(tool.error && tool.result.is_some()), |r| {
                        r.child(Icon::new(icon).size(px(14.0)).text_color(theme::t().text_2))
                    })
                            .child(
                                div()
                                    .text_size(px(theme::FONT_ROW))
                                    .line_height(px(theme::FONT_ROW_LEADING))
                                    .text_color(theme::t().text)
                                    .child(label),
                            )
                            .child(dot_sep())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(theme::FONT_ROW))
                                    .line_height(px(theme::FONT_ROW_LEADING))
                                    .text_color(theme::t().text_3)
                                    .child(first_line(&tool.arguments)),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        let mut col = div().w_full().max_w(px(CHAT_CONTENT_WIDTH)).mx_auto().v_flex().py_4();
        if rows.is_empty() {
            col = col.child(
                div()
                    .py_4()
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .text_color(theme::t().text_3)
                    .child("本轮还没有工具调用记录"),
            );
        } else {
            col = col.children(rows);
        }
        col
    }
}

// --- Render ------------------------------------------------------------------

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_clear {
            self.pending_clear = false;
            self.input.update(cx, |state, cx| state.set_value("", window, cx));
        }

        // 跟随系统：按窗口外观校正主题（与当前生效主题不同才重应用）
        if self.settings.appearance == AppearanceMode::System {
            let want = self.effective_appearance(window);
            if want.is_dark() != theme::is_dark() {
                theme::apply(want, cx);
                cx.notify();
            }
        }

        let vw: f32 = window.viewport_size().width.into();
        self.viewport = vw;
        let narrow = vw < SIDEBAR_AUTO_COLLAPSE;
        let collapsed = self.sidebar_collapsed || narrow;
        let sidebar_pref = if collapsed { 0.0 } else { self.sidebar_width };
        let details_pref = if self.details_open { self.details_width } else { 0.0 };
        let (sw, cw, dw) = compute_columns(vw, sidebar_pref, details_pref);
        let has_text = !self.input.read_with(cx, |s, _| s.value().trim().is_empty());

        let this = cx.entity();
        let drag_target = this.clone();

        let mut root = div()
            .size_full()
            .h_flex()
            .relative()
            .bg(theme::t().bg_base)
            .text_color(theme::t().text)
            // Zed redistributable_columns 模式：拖拽期间 move 事件全窗捕获，
            // 指针越过 8px 把手也照常跟手（捕获阶段，一处分发）。
            .on_drag_move::<ColumnDrag>(move |ev, window, cx| {
                let side = ev.drag(cx).side;
                let x: f32 = ev.event.position.x.into();
                let vw: f32 = window.viewport_size().width.into();
                drag_target.update(cx, |v, cx| match side {
                    DragSide::Sidebar => {
                        let w = x.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
                        if (w - v.sidebar_width).abs() > 0.5 {
                            v.sidebar_width = w;
                            cx.notify();
                        }
                    }
                    DragSide::Details => {
                        let w = (vw - x).clamp(DETAILS_MIN, DETAILS_MAX);
                        if (w - v.details_width).abs() > 0.5 {
                            v.details_width = w;
                            cx.notify();
                        }
                    }
                });
            })
            .child(self.render_sidebar(collapsed, sw, this.clone()));
        if !collapsed {
            root = root.child(drag_handle(DragSide::Sidebar));
        }
        root = root.child(self.render_center(cw, this.clone(), has_text));
        if dw > 0.0 {
            root = root.child(drag_handle(DragSide::Details));
            root = root.child(self.render_details(dw, this.clone()));
        }
        if self.settings_open {
            root = root.child(settings::render_settings(self, this, window, cx));
        }
        root
    }
}

impl AppView {

    fn render_sidebar(&self, collapsed: bool, width: f32, this: Entity<AppView>) -> Div {
        let mut col = div()
            .h_full()
            .w(px(width))
            .flex_none()
            .bg(theme::t().sidebar_bg)
            .border_r_1()
            .border_color(theme::t().border_l1);
        if collapsed {
            let t_expand = this.clone();
            let t_new = this.clone();
            let t_settings = this.clone();
            col = col
                .v_flex()
                .items_center()
                .pt(px(18.0))
                .px(px(10.0))
                .pb_1p5()
                .gap_3()
                .child(
                    // web rail：折叠态 toggle 呈现鲸鱼标记
                    div()
                        .id("sb-expand")
                        .size(px(36.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::t().hover))
                        .tooltip(tip("展开侧边栏"))
                        .on_click(move |_, _, cx| {
                            t_expand.update(cx, |v, cx| { v.sidebar_collapsed = false; cx.notify(); });
                        })
                        .child(
                            gpui::svg()
                                .path("brands/fish.svg")
                                .w(px(24.0)).h(px(17.65))
                                .text_color(theme::t().text),
                        ),
                )
                .child(
                    rail_icon("sb-new", IconName::Plus, "新建会话", move |_, _, cx| {
                        t_new.update(cx, |v, cx| { v.new_session(cx); });
                    }),
                )
                .child(div().flex_grow())
                .child(
                    rail_icon("sb-settings", IconName::Settings, "设置", move |_, _, cx| {
                        t_settings.update(cx, |v, cx| { v.settings_open = true; cx.notify(); });
                    }),
                );
        } else {
            let t_collapse = this.clone();
            let t_new = this.clone();
            let t_settings = this.clone();
            let current_id = self.current_session_id();
            let session_rows: Vec<Stateful<Div>> = self
                .sessions
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let t = this.clone();
                    let id = s.id.clone();
                    let active = s.id == current_id;
                    session_row(i, s.title.clone(), s.time_label.clone(), active, move |_, _, cx| {
                        let id = id.clone();
                        t.update(cx, |v, cx| { v.switch_session(id, cx); });
                    })
                })
                .collect();
            col = col
                .v_flex()
                .px_3()
                .py_1p5()
                .child(
                    // 品牌行：logo + 名字 + HARNESS pill + 折叠按钮（60px 高）
                    div()
                        .h(px(60.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .pl_1()
                        .child(
                            div().flex_1().min_w_0().flex().items_center().gap_2()
                                .child(
                                    // 官方鲸鱼标记（web FishLogo，currentColor 随主题）
                                    gpui::svg()
                                        .path("brands/fish.svg")
                                        .w(px(24.0)).h(px(17.65))
                                        .text_color(theme::t().text),
                                )
                                .child(
                                    div()
                                        .text_size(px(theme::FONT_BRAND))
                                        .line_height(px(24.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("deepseek"),
                                )
                                .child(
                                    // 徽牌（web buildRevision：品牌色底 + 反色字）
                                    div()
                                        .px(px(4.0))
                                        .rounded(px(3.0))
                                        .bg(theme::t().text)
                                        .text_color(theme::t().bg_base)
                                        .font_family(theme_mono())
                                        .text_size(px(8.0))
                                        .line_height(px(16.0))
                                        .child("HARNESS"),
                                ),
                        )
                        .child(
                            icon_btn("sb-collapse", IconName::PanelLeftClose, theme::t().text_2, "收起侧边栏", move |_, _, cx| {
                                t_collapse.update(cx, |v, cx| { v.sidebar_collapsed = true; cx.notify(); });
                            }),
                        ),
                )
                .child(
                    // 新建会话：38px、r12、白 12% 描边（web .newSession）
                    div()
                        .id("sb-new-session")
                        .h(px(38.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_1p5()
                        .mx_0p5()
                        .mb_2()
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(theme::t().border_l2)
                        .bg(theme::t().surface)
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::t().text)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::t().surface_2))
                        .on_click(move |_, _, cx| {
                            t_new.update(cx, |v, cx| { v.new_session(cx); });
                        })
                        .child(Icon::new(IconName::Plus).size(px(16.0)))
                        .child("新建会话"),
                )
                .child(
                    // 区块头：会话 + 搜索 / 视图 / 新建工作区（web .sectionHeader）
                    div()
                        .h(px(36.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .pl_1()
                        .mb_1()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(theme::FONT_ROW))
                                .line_height(px(20.0))
                                .text_color(theme::t().text_3)
                                .child("会话"),
                        )
                        .child(icon_btn("sb-search", IconName::Search, theme::t().text_2, "搜索会话", |_, _, _| {}))
                        .child(icon_btn("sb-view", IconName::Ellipsis, theme::t().text_2, "视图选项", |_, _, _| {}))
                        .child(icon_btn("sb-add-workspace", IconName::Plus, theme::t().text_2, "添加工作区", |_, _, _| {})),
                )
                .child(
                    // 工作区行（folder + 名称 + 展开指示）
                    div()
                        .h(px(34.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .rounded(px(8.0))
                        .child(
                            Icon::new(IconName::Folder)
                                .size(px(16.0))
                                .text_color(theme::t().accent),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(theme::FONT_ROW))
                                .line_height(px(20.0))
                                .text_color(theme::t().text)
                                .child("DSH"),
                        )
                        .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(theme::t().caption)),
                )
                .child(
                    // 会话列表 + 底部渐隐（web .fade）
                    div()
                        .relative()
                        .flex_grow()
                        .min_h_0()
                        .child(
                            div()
                                .id("sidebar-list")
                                .h_full()
                                .overflow_y_scroll()
                                .v_flex()
                                .gap_0p5()
                                .children(session_rows),
                        )
                        .child(
                            // 底部渐隐（web .fade）：起点必须是与 sidebar_bg 同
                            // 色但 alpha=0 的颜色——若用 transparent_black，浅色
                            // 侧栏上插值的中间像素会变灰，呈现一条黑带。
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .h(px(24.0))
                                .bg(linear_gradient(
                                    180.0,
                                    linear_color_stop(
                                        {
                                            let sb = theme::t().sidebar_bg;
                                            gpui::Rgba { r: sb.r, g: sb.g, b: sb.b, a: 0.0 }
                                        },
                                        0.0,
                                    ),
                                    linear_color_stop(theme::t().sidebar_bg, 1.0),
                                )),
                        ),
                )
                .child(
                    // 底部设置
                    div()
                        .id("sb-settings")
                        .h(px(32.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .rounded(px(8.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::t().hover))
                        .on_click(move |_, _, cx| {
                            t_settings.update(cx, |v, cx| { v.settings_open = true; cx.notify(); });
                        })
                        .child(Icon::new(IconName::Settings).size(px(16.0)).text_color(theme::t().text_3))
                        .child(
                            div()
                                .text_size(px(theme::FONT_ROW))
                                .line_height(px(20.0))
                                .text_color(theme::t().text_2)
                                .child("设置"),
                        ),
                );
        }
        col
    }

    fn render_center(&self, width: f32, this: Entity<AppView>, has_text: bool) -> Div {
        let t = this.clone();
        let mut center = div()
            .h_full()
            .w(px(width))
            .min_w_0()
            .flex_none()
            .v_flex()
            .bg(theme::t().bg_base);

        let show_header = !self.is_empty_session() || self.running;
        if show_header {
            center = center.child(self.render_header(this.clone()));
        }

        // 主体：hero（空会话）/ 对话 / 轨迹
        if self.is_empty_session() && !self.running {
            center = center.child(self.render_hero(this, has_text));
        } else {
            let body = match self.tab {
                CenterTab::Conversation => self.render_chat(&this),
                CenterTab::Trajectory => self.render_trajectory(&this),
            };
            let show_jump = !self.chat_near_bottom();
            let t_jump = this.clone();
            center = center
                .child(
                    // 相对定位包裹层承载「回到底部」浮钮（web ChatView .toBottom）
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(
                            div()
                                .id("chat-scroll")
                                .h_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.chat_scroll)
                                .px_8()
                                .vertical_scrollbar(&self.chat_scroll)
                                .child(body),
                        )
                        .when(show_jump, |d| {
                            d.child(
                                div()
                                    .id("chat-jump-bottom")
                                    .absolute()
                                    .right(px(24.0))
                                    .bottom(px(16.0))
                                    .size(px(34.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(theme::t().border_l2)
                                    .bg(theme::t().surface)
                                    .text_color(theme::t().text_2)
                                    .cursor_pointer()
                                    .shadow_md()
                                    .hover(|s| s.bg(theme::t().surface_2).text_color(theme::t().text))
                                    .tooltip(tip("回到底部"))
                                    .on_click(move |_, _, cx| {
                                        t_jump.update(cx, |v, _cx| {
                                            v.chat_scroll.scroll_to_bottom();
                                        });
                                    })
                                    .child(Icon::new(IconName::ChevronDown).size(px(16.0))),
                            )
                        }),
                )
                .child(self.render_composer_area(t, has_text));
        }
        center
    }

    /// 会话 header：标题行 + 对话/轨迹 tab（ConversationRoot .header）。
    fn render_header(&self, this: Entity<AppView>) -> Div {
        let title = self
            .sessions
            .iter()
            .find(|s| s.id == self.current_session_id())
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "会话".into());
        let t_details = this.clone();
        div()
            .flex_none()
            .pt_3()
            .pl(px(20.0))
            .pr(px(28.0))
            .border_b_1()
            .border_color(theme::t().border_l2)
            .child(
                div()
                    .min_h(px(32.0))
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::t().text)
                            .child(title),
                    )
                    .child(
                        div().flex().items_center().gap_1().px_2().h(px(24.0)).rounded(px(12.0))
                            .hover(|s| s.bg(theme::t().hover))
                            .child(Icon::new(IconName::Bot).size(px(12.0)).text_color(theme::t().text_2))
                            .child(
                                div()
                                    .text_size(px(theme::FONT_TAB))
                                    .line_height(px(theme::FONT_ROW_LEADING))
                                    .text_color(theme::t().text_2)
                                    .child("标准模式"),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        icon_btn("details-toggle", IconName::PanelRight, theme::t().text_2, "详情", move |_, _, cx| {
                            t_details.update(cx, |v, cx| { v.details_open = !v.details_open; cx.notify(); });
                        }),
                    ),
            )
            .child(self.render_tabs(this))
    }

    /// 对话 / 轨迹 tab 条（13/16 wt500，激活蓝 + 2px 底条）。
    fn render_tabs(&self, this: Entity<AppView>) -> Div {
        let t1 = this.clone();
        let t2 = this;
        let tab = |id: &'static str, label: &'static str, active: bool, t: Entity<AppView>| {
            div()
                .id(id)
                .cursor_pointer()
                .pb(px(11.0))
                .border_b_2()
                .border_color(if active { theme::t().accent.into() } else { gpui::transparent_black() })
                .text_size(px(theme::FONT_TAB))
                .line_height(px(16.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if active { theme::t().accent } else { theme::t().text_3 })
                .hover(|s| s.text_color(theme::t().text_2))
                .on_click(move |_, _, cx| {
                    t.update(cx, |v, cx| {
                        v.tab = if label == "轨迹" { CenterTab::Trajectory } else { CenterTab::Conversation };
                        cx.notify();
                    });
                })
                .child(label)
        };
        div()
            .mt_1()
            .pl_2()
            .flex()
            .gap(px(36.0))
            .child(tab("tab-chat", "对话", self.tab == CenterTab::Conversation, t1))
            .child(tab("tab-traj", "轨迹", self.tab == CenterTab::Trajectory, t2))
    }

    /// 消息流（748px 内容列 + 16px 项间距，ChatView .column）。
    fn render_chat(&self, this: &Entity<AppView>) -> Div {
        let mut col = div()
            .w_full()
            .max_w(px(CHAT_CONTENT_WIDTH))
            .mx_auto()
            .v_flex()
            .gap_4()
            .py_4()
            .children(
                self.entries.iter().enumerate().map(|(i, e)| self.render_entry(e, i, this)),
            );
        if self.running {
            col = col.child(self.render_status_line());
        }
        if self.stats_turns > 0 && !self.running {
            // web StatsLine：流内居中，12/20 tertiary，nowrap ellipsis
            col = col.child(
                div()
                    .w_full()
                    .text_center()
                    .text_size(px(theme::FONT_CAPTION))
                    .line_height(px(20.0))
                    .text_color(theme::t().text_3)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(format!(
                        "{} 轮 · {} 次工具调用",
                        self.stats_turns, self.stats_tools
                    )),
            );
        }
        div().w_full().child(col)
    }

    /// 底部 composer 区：输入卡（统计行已移入消息流 StatsLine 位）。
    fn render_composer_area(&self, this: Entity<AppView>, has_text: bool) -> Div {
        div()
            .flex_none()
            .v_flex()
            .bg(theme::t().bg_base)
            .pb_2()
            .child(self.composer_card(this, has_text))
    }

    /// 空会话 hero：标题 + 工作区行 + 居中输入卡（HeroShell）。
    fn render_hero(&self, this: Entity<AppView>, has_text: bool) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .px_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(COMPOSER_CARD_WIDTH))
                    .v_flex()
                    .gap_3()
                    .pb(px(32.0))
                    .child(
                        // 标题行：fish + 探索未至之境 + 预览版 badge
                        div().flex().items_center().justify_center().gap_2p5().child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2p5()
                                .text_size(px(theme::FONT_HERO))
                                .line_height(px(theme::FONT_HERO_LEADING))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::t().text)
                                .child(
                                    gpui::svg()
                                        .path("brands/fish.svg")
                                        .w(px(34.0)).h(px(25.0))
                                        .text_color(theme::t().text),
                                )
                                .child("探索未至之境")
                                .child(
                                    div()
                                        .mt(px(2.0))
                                        .px_1p5()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(theme::t().hover)
                                        .bg(gpui::rgb(0x283142))
                                        .text_color(theme::t().text)
                                        .font_family(theme_mono())
                                        .text_size(px(theme::FONT_CAPTION))
                                        .line_height(px(theme::FONT_CAPTION_LEADING))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("预览版"),
                                ),
                        ),
                    )
                    .child(
                        // 工作区行：folder + 目录名 + chevron + 标准模式 chip（web hero 同排）
                        div()
                            .flex()
                            .items_center()
                            .pl(px(20.0))
                            .gap_1()
                            .child(Icon::new(IconName::FolderClosed).size(px(14.0)).text_color(theme::t().text))
                            .child(
                                div()
                                    .text_size(px(theme::FONT_TAB))
                                    .line_height(px(theme::FONT_ROW_LEADING))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::t().text)
                                    .child(workspace_name()),
                            )
                            .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(theme::t().caption))
                            .child(
                                div()
                                    .id("hero-mode")
                                    .mx_2()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .px_2()
                                    .h(px(24.0))
                                    .rounded(px(12.0))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::t().hover))
                                    .child(Icon::new(IconName::Bot).size(px(12.0)).text_color(theme::t().text_2))
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_TAB))
                                            .line_height(px(theme::FONT_ROW_LEADING))
                                            .text_color(theme::t().text_2)
                                            .child("标准模式"),
                                    ),
                            ),
                    )
                    .child(self.composer_card(this, has_text)),
            )
    }

    /// 输入卡（web InputBar .card）：r22、白 6% 描边、850 表面、
    /// 文本在上（16/24），控件行在下（+ / 模式 | 模型 / 发送）。
    fn composer_card(&self, this: Entity<AppView>, has_text: bool) -> Div {
        let running = self.running;

        let left = div()
            .flex()
            .items_center()
            .gap_4()
            .child(icon_btn("composer-add", IconName::Plus, theme::t().text, "添加附件", |_, _, _| {}))
            .child(
                div()
                    .id("composer-mode")
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(28.0))
                    .px_2()
                    .rounded(px(8.0))
                    .child(Icon::new(IconName::Eye).size(px(14.0)).text_color(theme::t().text_2))
                    .child(
                        div()
                            .text_size(px(theme::FONT_TAB))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::t().text_2)
                            .child("Workspace Write"),
                    )
                    .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(theme::t().caption)),
            );

        let mut right = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .id("composer-model")
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(28.0))
                    .px_2()
                    .rounded(px(8.0))
                    .child(
                        div()
                            .text_size(px(theme::FONT_TAB))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::t().text_2)
                            .child(self.desired_model.clone()),
                    )
                    .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(theme::t().caption)),
            );

        let trailing: AnyElement = if running {
            let t_stop = this.clone();
            // 停止按钮：蓝圆 + 白色方块
            div()
                .id("composer-stop")
                .size(px(34.0))
                .rounded_full()
                .bg(theme::t().accent)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme::t().accent_hover))
                .tooltip(tip("停止生成"))
                .on_click(move |_, _, cx| {
                    t_stop.update(cx, |v, _cx| {
                        v.agent.cancel();
                    });
                })
                .child(div().size(px(10.0)).rounded(px(2.0)).bg(gpui::white()))
                .into_any_element()
        } else {
            let t_send = this.clone();
            div()
                .id("composer-send")
                .size(px(34.0))
                .rounded_full()
                .bg(theme::t().accent)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(!has_text, |d| d.opacity(0.4))
                .when(has_text, |d| d.hover(|s| s.bg(theme::t().accent_hover)))
                .tooltip(tip("发送 (Enter)"))
                .on_click(move |_, window, cx| {
                    t_send.update(cx, |v, cx| v.send_from_composer(window, cx));
                })
                .child(Icon::new(IconName::ArrowUp).size(px(18.0)).text_color(gpui::white()))
                .into_any_element()
        };
        right = right.child(trailing);

        div()
            .w_full()
            .max_w(px(COMPOSER_CARD_WIDTH))
            .mx_auto()
            .flex_none()
            .v_flex()
            .gap_3()
            .pt_2p5()
            .rounded(px(22.0))
            .border_1()
            .border_color(theme::t().border_l1)
            .bg(theme::t().surface)
            .child(div().pl_4().pr_3().pt_1().child(Input::new(&self.input).appearance(false).w_full()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_2()
                    .pt(px(2.0))
                    .pb(px(6.0))
                    .child(left)
                    .child(right),
            )
    }

    /// 右侧详情面板（DetailsPanel）。
    fn render_details(&self, width: f32, this: Entity<AppView>) -> Div {
        let t = this.clone();
        let mut body = div().id("details-body").flex_1().min_h_0().overflow_y_scroll().px_4().py_3();
        match &self.selected_tool {
            None => {
                body = body.child(
                    div()
                        .py_2()
                        .text_size(px(13.0))
                        .line_height(px(20.0))
                        .text_color(theme::t().text_3)
                        .child("点击消息流中的工具行查看详情"),
                );
            }
            Some(tool) => {
                body = body
                    .child(detail_section("工具", div().text_color(theme::t().text).child(tool.name.clone())))
                    .child(detail_section(
                        "输入",
                        code_card(&first_line(&tool.arguments), false),
                    ))
                    .child(detail_section(
                        "输出",
                        match &tool.result {
                            Some(r) => code_card(r, tool.error),
                            None => div()
                                .text_size(px(13.0))
                                .line_height(px(20.0))
                                .text_color(theme::t().caption)
                                .child("（运行中…）"),
                        },
                    ));
            }
        }
        div()
            .h_full()
            .w(px(width))
            .flex_none()
            .v_flex()
            .bg(theme::t().bg_base)
            .border_l_1()
            .border_color(theme::t().border_l2)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .pt(px(14.0))
                    .px_3()
                    .pb_3()
                    .border_b_1()
                    .border_color(theme::t().border_l2)
                    .child(
                        div()
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::t().text)
                            .child("详情"),
                    )
                    .child(
                        icon_btn("details-close", IconName::Close, theme::t().text_2, "关闭详情", move |_, _, cx| {
                            t.update(cx, |v, cx| { v.details_open = false; cx.notify(); });
                        }),
                    ),
            )
            .child(body)
    }
}









/// 助手消息完成后的 footer：复制按钮（悬停显现）+ 用时。
fn render_entry_footer(elapsed: Duration, this: &Entity<AppView>, ei: usize) -> Div {
    let t = this.clone();
    let secs = elapsed.as_secs();
    let text = if secs >= 60 {
        format!("用时 {}分{}秒", secs / 60, secs % 60)
    } else {
        format!("用时 {}秒", secs)
    };
    let group: SharedString = format!("assistant-msg-{ei}").into();
    let group_copy = group.clone();
    div()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .id(("copy-assistant", ei as u64))
                .size(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .cursor_pointer()
                .text_color(theme::t().caption)
                .opacity(0.0)
                .group_hover(group_copy, |s| s.opacity(1.0))
                .hover(|s| s.text_color(theme::t().text_2).bg(theme::t().hover))
                .tooltip(tip("复制"))
                .on_click(move |_, _, cx| {
                    t.update(cx, |v, cx| {
                        // 复制本条助手消息全部文本块
                        let mut text = String::new();
                        if let Some(entry) = v.entries.get(ei) {
                            for block in &entry.blocks {
                                if let MsgBlock::Text(t) = block {
                                    text.push_str(t);
                                }
                            }
                        }
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    });
                })
                .child(Icon::new(IconName::Copy).size(px(14.0))),
        )
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(theme::t().caption)
                .child(text),
        )
}


fn attach_tool_result(
    last: Option<&mut ChatEntry>,
    call_id: &str,
    result: &str,
    error: bool,
) {
    if let Some(entry) = last
        && let Some(MsgBlock::Tool(tool)) = entry.blocks.iter_mut().rev().find(|b| matches!(b, MsgBlock::Tool(t) if t.id == call_id))
    {
        tool.result = Some(result.to_string());
        tool.error = error;
    }
}

// --- 启动 --------------------------------------------------------------------

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _guard = rt.enter();

    let events = EventBus::new();
    // agent/request-error 退避重试（参考 dsh-llm-retry 的角色）
    let _retry_disposer = dsh_agent_loop::retry::attach_retry(&events);
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));

    // 存储与 web 版 dsh 共享：{DSH_HOME|~/.dsh}/settings.yaml + .credentials.yaml + sessions/
    migrate_legacy();
    let (mut user_settings, startup_active, startup_desired, deepseek_env_locked, stored_deepseek_key) =
        load_user_config();
    if !startup_desired.is_empty() {
        user_settings.model = startup_desired.clone();
    }

    let (provider, model) = match DeepSeekAdapter::from_env() {
        Some(adapter) => {
            let _h = llm.register_adapter(&["deepseek".to_string()], Arc::new(adapter)).expect("register deepseek");
            let model = std::env::var("DSH_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string());
            ("deepseek".to_string(), model)
        }
        None => {
            if !stored_deepseek_key.is_empty() {
                let adapter = DeepSeekAdapter::new(stored_deepseek_key.clone());
                let _h = llm.register_adapter(&["deepseek".to_string()], Arc::new(adapter))
                    .expect("register deepseek from credentials");
                let model = std::env::var("DSH_MODEL")
                    .unwrap_or_else(|_| "deepseek-chat".to_string());
                ("deepseek".to_string(), model)
            } else {
                let _h = llm.register_adapter(&["mock".to_string()], Arc::new(MockAdapter)).expect("register mock");
                ("mock".to_string(), "mock".to_string())
            }
        }
    };

    let tools = Arc::new(ToolRegistry::new());
    let _fs = tools.register(Arc::new(FsTool)).unwrap();
    let _shell = tools.register(Arc::new(ShellTool)).unwrap();
    let _web = tools.register(Arc::new(WebTool::new())).unwrap();
    let prompt = Arc::new(SystemPrompt::new());
    let demo_prompt = std::env::var("DSH_PROMPT").ok().filter(|s| !s.trim().is_empty());

    // --- 会话持久化：恢复最近的会话，或新建 ---
    let recorder = Arc::new(SessionRecorder::new(sessions_dir()));
    let existing = recorder.list().unwrap_or_default();
    let (initial_session, sessions_meta, is_fresh) = if let Some(id) = existing.first() {
        let meta: Vec<SessionMeta> = existing
            .iter()
            .map(|sid| {
                let title = recorder
                    .load(sid)
                    .ok()
                    .and_then(|s| s.first_user_text())
                    .map(|t| t.chars().take(30).collect())
                    .unwrap_or_else(|| "新会话".into());
                let time_label = session_time_label(&recorder, sid);
                SessionMeta { id: sid.clone(), title, time_label }
            })
            .collect();
        let session = recorder.load(id).unwrap_or_else(|_| Session::new(id.clone()));
        let is_fresh = session.entries().is_empty();
        (session, meta, is_fresh)
    } else {
        let id = SessionId::new(uuid::Uuid::new_v4().to_string());
        (Session::new(id.clone()), vec![SessionMeta { id, title: "新会话".into(), time_label: String::new() }], true)
    };

    let agent = ReactLoopAgent::new(
        initial_session.id.clone(),
        AgentOptions {
            provider: provider.clone(),
            model: model.clone(),
            max_tokens: None,
            system_prompt: Some("You are DeepSeek Harness (Rust), a helpful coding agent.".into()),
        },
        Arc::clone(&llm),
        tools,
        prompt,
        events,
    );
    agent.set_session(initial_session);

    // 持久化：每个追加的会话事件写入 JSONL
    let recorder_sink = Arc::clone(&recorder);
    let agent_sink = Arc::clone(&agent);
    agent.set_event_sink(move |event| {
        let id = agent_sink.session().lock().unwrap().id.clone();
        let _ = recorder_sink.append(&id, &event);
    });

    let event_rx = agent.subscribe();
    agent.spawn();

    // （设置加载已前置到 agent 构造前，见下方 storage 段）
    let startup_theme = match user_settings.appearance {
        AppearanceMode::Light => gpui_component::ThemeMode::Light,
        AppearanceMode::Dark => gpui_component::ThemeMode::Dark,
        AppearanceMode::System => gpui_component::ThemeMode::Dark, // 实际值由首帧 render 按窗口外观校正
    };

    // 注册用户声明的自定义提供方（OpenAI 兼容，复用 DeepSeek adapter）
    for p in &user_settings.providers {
        if !p.base_url.is_empty() {
            let adapter = DeepSeekAdapter::with_base_url(&p.api_key, &p.base_url);
            let _ = llm.register_adapter(&[p.id.clone()], Arc::new(adapter));
        }
    }
    let initial_route = if startup_active == "deepseek" {
        "deepseek".to_string()
    } else if user_settings.providers.iter().any(|p| p.id == startup_active) {
        startup_active.clone()
    } else {
        "deepseek".to_string()
    };
    let initial_model = if startup_desired.is_empty() {
        "deepseek-chat".to_string()
    } else {
        startup_desired.clone()
    };
    // 初始路由：环境变量 key > 自定义提供方 > mock
    // agent 初始路由优先级：
    //   1. 环境变量 DEEPSEEK_KEY（显式意图，最优先）
    //   2. settings.yaml 的 agent-default-model 所指 provider（web 里选定的当前模型）
    //   3. .credentials.yaml 有 DEEPSEEK key
    //   4. 声明的第一个 provider / 5. mock
    let effective_startup = if provider == "deepseek" {
        "deepseek".to_string()
    } else if initial_route != "deepseek"
        && user_settings.providers.iter().any(|p| p.id == initial_route)
    {
        initial_route.clone()
    } else if !stored_deepseek_key.is_empty() {
        "deepseek".to_string()
    } else if user_settings.providers.is_empty() {
        "mock".to_string()
    } else {
        user_settings
            .providers
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_default()
    };
    if effective_startup != "mock"
        && let Some(p) = user_settings.providers.iter().find(|p| p.id == effective_startup)
    {
        let model = p.models.first().map(|m| m.id.clone()).unwrap_or(initial_model.clone());
        agent.set_provider_and_model(p.id.clone(), model);
    }
    let _ = initial_model;

    Application::new()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
        gpui_component::init(cx);
        theme::apply(startup_theme, cx);
        let bounds = Bounds::centered(None, size(px(1300.0), px(800.0)), &*cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("DeepSeek Harness");
                let input = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx)
                        .placeholder("给智能体发消息")
                        .multi_line(true)
                        .auto_grow(1, 14)
                });
                let api_input = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).masked(true).placeholder("输入 API 密钥，或留空使用环境认证")
                });
                let adopt_key = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).masked(true).placeholder("输入 API 密钥，或留空使用环境认证")
                });
                let adopt_base = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).placeholder("提供方默认")
                });
                let dc_route = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).placeholder("acme-gateway")
                });
                let dc_name = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).placeholder("显示名称")
                });
                let dc_base = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).placeholder("https://gateway.example/v1")
                });
                let dc_key = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).masked(true).placeholder("输入 API 密钥，或留空使用环境认证")
                });
                let dc_new_model = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).placeholder("模型 ID")
                });
                let edit_key = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).masked(true).placeholder("已配置——输入新值可替换")
                });
                let edit_base = cx.new(|cx: &mut Context<InputState>| {
                    InputState::new(window, cx).placeholder("提供方默认")
                });
                let desired_model = std::env::var("DSH_MODEL")
                    .unwrap_or_else(|_| user_settings.model.clone());
                let app = cx.new(|cx| {
                    let deps = AppDeps { recorder: Arc::clone(&recorder), llm: Arc::clone(&llm) };
                    AppView::new(
                        Arc::clone(&agent),
                        deps,
                        sessions_meta.clone(),
                        input.clone(),
                        api_input,
                        desired_model,
                        effective_startup.clone(),
                        user_settings.clone(),
                        provider == "deepseek" || !stored_deepseek_key.is_empty() || !user_settings.providers.is_empty(),
                        deepseek_env_locked,
                        stored_deepseek_key.clone(),
                        adopt_key.clone(),
                        adopt_base.clone(),
                        dc_route.clone(),
                        dc_name.clone(),
                        dc_base.clone(),
                        dc_key.clone(),
                        dc_new_model.clone(),
                        edit_key.clone(),
                        edit_base.clone(),
                        cx,
                    )
                });

                let view = app.clone();
                cx.spawn(move |cx: &mut AsyncApp| {
                    let mut cx = cx.clone();
                    async move {
                        while let Ok(ev) = event_rx.recv_async().await {
                            let _ = cx.update_entity(&view, |app: &mut AppView, cx: &mut Context<AppView>| {
                                app.push_event(ev);
                                cx.notify();
                            });
                        }
                    }
                })
                .detach();

                // 流式期间每秒重绘一次，让状态行的耗时计时跳动
                let tick_view = app.clone();
                cx.spawn(move |cx: &mut AsyncApp| {
                    let mut cx = cx.clone();
                    async move {
                        loop {
                            Timer::after(Duration::from_millis(100)).await;
                            let Ok(running) = tick_view.update(&mut cx, |v, _| v.running) else {
                                return;
                            };
                            if running {
                                let _ = tick_view.update(&mut cx, |_, cx| cx.notify());
                            }
                        }
                    }
                })
                .detach();

                if is_fresh
                    && let Some(demo) = demo_prompt.as_deref() {
                        Arc::clone(&agent).followup(demo);
                    }

                cx.new(|cx| Root::new(app, window, cx))
            },
        )
        .expect("failed to open window");
    });
}

/// web CustomProviderCard 的 ROUTE_PATTERN：小写字母开头，后接小写/数字/短横线段。
fn valid_route_id(route: &str) -> bool {
    let bytes = route.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    route
        .split('-')
        .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
        && !route.ends_with('-')
}

/// 会话 JSONL 的修改时间 → web 侧栏的相对时间文案。
fn session_time_label(recorder: &SessionRecorder, id: &SessionId) -> String {
    let path = recorder.path_for(id);
    let Ok(md) = std::fs::metadata(&path) else {
        return String::new();
    };
    let Ok(modified) = md.modified() else {
        return String::new();
    };
    let Ok(d) = modified.duration_since(SystemTime::UNIX_EPOCH) else {
        return String::new();
    };
    let Some(now) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).ok() else {
        return String::new();
    };
    let secs = now.as_secs().saturating_sub(d.as_secs());
    match secs {
        s if s < 60 => "刚刚".into(),
        s if s < 3600 => format!("{}分钟", s / 60),
        s if s < 86400 => format!("{}小时", s / 3600),
        s if s < 86400 * 30 => format!("{}天", s / 86400),
        _ => format!("{}个月", secs / 86400 / 30),
    }
}

fn message_text(m: &Message) -> String {
    m.content.iter().filter_map(|b| match b {
        ContentBlock::Text { text } | ContentBlock::Reasoning { text } => Some(text.as_str()),
        _ => None,
    }).collect()
}

struct MockAdapter;

#[async_trait]
impl LlmAdapter for MockAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock (无 API key)".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let last_user = options.messages.iter().rev().find(|m| matches!(m.source, MessageSource::User)).map(message_text).unwrap_or_default();
        let reply = format!(
            "你刚才说的是：**{last_user}**。\n\n## 这是 markdown 渲染演示\n\n- **加粗**文本\n- `行内代码`\n- 有序列表\n\n1. 第一项\n2. 第二项\n\n> 引用块（设置 DEEPSEEK_API_KEY 后可接入真实模型）。"
        );
        let signal = options.signal.clone();
        Ok(Box::pin(stream! {
            yield StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Reasoning };
            yield StreamChunk::ReasoningDelta { index: 0, text: "让我梳理一下要点，再组织回答…".into() };
            yield StreamChunk::BlockStart { index: 1, block_type: ContentBlockType::Text };
            for word in reply.split_inclusive(' ') {
                if signal.aborted() { yield StreamChunk::stream_aborted(); return; }
                yield StreamChunk::TextDelta { index: 1, text: word.to_string() };
                tokio::time::sleep(Duration::from_millis(4)).await;
            }
            yield StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None };
        }))
    }
}
