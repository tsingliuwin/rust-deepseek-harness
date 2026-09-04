//! 模型发现——web 模型页「获取可用模型」的探测引擎。
//!
//! 上游 `llm-pi-ai/src/discovery.ts` 的 1:1 移植（语义基准 master 76fda729，
//! 含 rc.1 后的模型发现扩展）：OpenAI 兼容与 Anthropic Messages 两种协议经各自
//! 的原生模型列表端点探测；解析器同时接受标准 `data` 数组与部分兼容网关返回的
//! 富化 `models` 映射；其余协议报「不可探测」，由界面回退手动录入。
//!
//! 与上游的有意差异（记录于同步记录）：
//! - 无 provider 目录短路：rustdsh 的 PROVIDER_CATALOG 每项只带一个默认模型，
//!   没有可服务的富目录，统一走端点探测；
//! - 归因 UA 沿用 adapter 的 `dsh-rust/<version>`（上游为 harness UA）；
//! - 请求加 60s 总超时（上游依赖浏览器默认超时，桌面端不能无限挂住按钮）。

use serde_json::Value;
use std::time::Duration;

/// 一个探测到的模型（上游 `LlmDiscoveredModel`）。
#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: String,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
}

/// 探测失败：诊断文案 + 上游同名的错误码。
#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryError {
    pub message: String,
    pub code: &'static str,
}

pub const DISCOVERY_FAILED: &str = "DISCOVERY_FAILED";
pub const DISCOVERY_UNSUPPORTED: &str = "DISCOVERY_UNSUPPORTED";
pub const INVALID_CREDENTIAL: &str = "INVALID_CREDENTIAL";

/// Anthropic 模型列表端点要求的稳定 API 版本。
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Anthropic 公开端点接受的单页最大模型数；探测只读一页，不追 `has_more`。
const ANTHROPIC_MODEL_LIMIT: u64 = 1000;
/// 拒绝超过此字节数的端点回应。端点是用户手填的 URL，所以上限按实际读取的
/// 字节而非声明长度执行（声明的 content-length 只用来提前拒绝诚实的服务端）。
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// 可读模型列表的协议（上游 `LISTABLE_PROTOCOLS`）。Azure 虽属 OpenAI 血统但用
/// `api-key` 头 + `api-version` 查询，Codex 走 OAuth——猜它们的形状会把认证失败
/// 误报成「无模型的提供方」，故与上游一致地缺席。
fn listable(api: &str) -> bool {
    matches!(api, "openai-completions" | "openai-responses" | "anthropic-messages")
}

/// 端点基址拼接协议的列表路径。基址按前缀处理而非 URL 解析，带部署路径的
/// 基址（如 `https://gateway.example/openai/v1`）不会丢段。OpenAI 协议列在
/// `{baseURL}/models`；Anthropic 列在 `{root}/v1/models`——root 是去掉尾斜杠
/// 和一个尾部 `/v1` 段的基址（网关文档两种写法都发布）。只有列表 URL 归一化
/// 这一段；模型请求仍收到未改动的 `baseURL`。
fn listing_url(base_url: &str, api: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if api != "anthropic-messages" {
        return format!("{base}/models");
    }
    let root = base.strip_suffix("/v1").unwrap_or(base);
    format!("{root}/v1/models?limit={ANTHROPIC_MODEL_LIMIT}")
}

/// 接受一枚探测密钥，或在拼请求头之前拒绝。没有这一步，非法字符会让 HTTP
/// 库在本地抛确定性错误，却被网络错误文案掩盖。
fn usable_probe_key(raw: &str) -> Result<String, DiscoveryError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(DiscoveryError {
            message: "this provider's API key is blank; enter it on the Models page, or clear it to probe unauthenticated".into(),
            code: INVALID_CREDENTIAL,
        });
    }
    // 上游 LEGAL_API_KEY = /^[\x21-\x7E]+$/：可见 ASCII，不含空格。
    if !value.bytes().all(|b| (0x21..=0x7E).contains(&b)) {
        return Err(DiscoveryError {
            message: "this provider's API key contains characters no HTTP header can carry; paste the raw key only".into(),
            code: INVALID_CREDENTIAL,
        });
    }
    Ok(value.to_string())
}

/// 列表条目里的正整数字段，缺失或不可用时为 `None`（上游 `capacity`）。
fn capacity(candidates: &[Option<&Value>]) -> Option<u64> {
    for candidate in candidates.iter().flatten() {
        let Some(f) = candidate.as_f64() else { continue };
        // JS Number.isInteger：整值浮点（256.0）也算整数。
        if f.fract() == 0.0 && f > 0.0 {
            return Some(f as u64);
        }
    }
    None
}

/// 列表条目里的非空字符串字段，缺失时为 `None`（上游 `label`）。
fn label(candidates: &[Option<&str>]) -> Option<String> {
    candidates
        .iter()
        .flatten()
        .map(|s| s.to_string())
        .find(|s| !s.is_empty())
}

/// 字段的取值入口：键名逐级取，父级缺失时短路。
fn field<'a>(entry: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = entry;
    for key in path {
        cur = cur.get(*key)?;
    }
    Some(cur)
}

/// 解析一份模型列表回应。标准 `data` 数组优先；富化 `models` 映射以属性键为
/// 端点侧 id（嵌套 `id` 只在键为空时兜底——网关可能把规范模型身份放那里而非
/// 请求接受的别名）；只有对象值的映射条目才算模型，原始值属性可能是目录元
/// 数据而非模型记录。缺可用 id 的条目跳过而不失败整次探测：单个坏行不该否认
/// 其余可用的端点目录。缺名的名字回退到采用的 id。
fn read_listing(body: &Value) -> Result<Vec<DiscoveredModel>, DiscoveryError> {
    enum Row<'a> {
        Array(&'a Value),
        Mapped { key: &'a str, raw: &'a Value },
    }
    let rows: Vec<Row> = if let Some(data) = body.get("data").and_then(Value::as_array) {
        data.iter().map(Row::Array).collect()
    } else if let Some(models) = body.get("models").and_then(Value::as_object) {
        models
            .iter()
            .filter(|(_, raw)| raw.is_object())
            .map(|(key, raw)| Row::Mapped { key, raw })
            .collect()
    } else {
        return Err(DiscoveryError {
            message: "the endpoint's model listing has neither a \"data\" array nor a \"models\" object; enter this provider's models by hand".into(),
            code: DISCOVERY_FAILED,
        });
    };

    let mut models = Vec::new();
    for row in rows {
        let (key, entry) = match row {
            Row::Array(raw) => (None, raw),
            Row::Mapped { key, raw } => (Some(key), raw),
        };
        let entry_id = entry.get("id").and_then(Value::as_str);
        let Some(id) = label(&[key, entry_id]) else { continue };
        let name = label(&[
            entry.get("name").and_then(Value::as_str),
            entry.get("display_name").and_then(Value::as_str),
            entry.get("displayName").and_then(Value::as_str),
        ])
        .unwrap_or_else(|| id.clone());
        let context_window = capacity(&[
            entry.get("contextWindow"),
            entry.get("context_window"),
            entry.get("context_length"),
            entry.get("max_input_tokens"),
            field(entry, &["limit", "context"]),
        ]);
        let max_tokens = capacity(&[
            entry.get("maxOutputTokens"),
            entry.get("max_output_tokens"),
            entry.get("maxTokens"),
            entry.get("max_tokens"),
            field(entry, &["limit", "output"]),
            field(entry, &["top_provider", "max_completion_tokens"]),
        ]);
        models.push(DiscoveredModel { id, name, context_window, max_tokens });
    }
    Ok(models)
}

/// 探测一个草稿提供方端点所advertise的模型（上游 `discoverModels`，无目录短路）。
///
/// `api` 未选时按 OpenAI Chat Completions 问——网关最可能说的形状；拒绝等到
/// 字段填好会把动作从它存在的场景里扣掉。`api_key` 为 `None` 时匿名探测。
pub async fn discover_models(
    base_url: &str,
    api: Option<&str>,
    api_key: Option<&str>,
) -> Result<Vec<DiscoveredModel>, DiscoveryError> {
    if base_url.is_empty() {
        return Err(DiscoveryError {
            message: "set a baseURL, or enter this provider's models by hand".into(),
            code: DISCOVERY_FAILED,
        });
    }
    // 上游默认按 OpenAI Chat Completions 问；"openai" 是 rustdsh 存储层对
    // 同一形状的名字。
    let api = api.unwrap_or("openai-completions");
    let api = match api {
        "openai" => "openai-completions",
        other => other,
    };
    if !listable(api) {
        return Err(DiscoveryError {
            message: format!("protocol \"{api}\" has no model listing this build can read; enter this provider's models by hand"),
            code: DISCOVERY_UNSUPPORTED,
        });
    }
    let url = listing_url(base_url, api);
    let key = api_key.map(usable_probe_key).transpose()?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("dsh-rust/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| DiscoveryError { message: format!("could not reach {url}: {e}"), code: DISCOVERY_FAILED })?;
    let mut request = client.get(&url).header("accept", "application/json");
    if api == "anthropic-messages" {
        request = request.header("anthropic-version", ANTHROPIC_VERSION);
        if let Some(key) = &key {
            request = request.header("x-api-key", key);
        }
    } else if let Some(key) = &key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let response = request.send().await.map_err(|_| DiscoveryError {
        message: format!("could not reach {url}"),
        code: DISCOVERY_FAILED,
    })?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let hint = if status == 401 || status == 403 { "; check the API key" } else { "" };
        return Err(DiscoveryError {
            message: format!("{url} answered {status}{hint}"),
            code: DISCOVERY_FAILED,
        });
    }
    let text = read_bounded(response, &url).await?;
    let body: Value = serde_json::from_str(&text).map_err(|_| DiscoveryError {
        message: format!("{url} did not answer with JSON"),
        code: DISCOVERY_FAILED,
    })?;
    read_listing(&body)
}

/// 读回应体，拒绝超出上限的一份。声明的 content-length 先查，诚实的服务端
/// 不用传输就被拒；累计总量才是真正执行的上限，因为少报（或流式）的服务端
/// 事先什么都不说。
async fn read_bounded(response: reqwest::Response, url: &str) -> Result<String, DiscoveryError> {
    let oversized = || DiscoveryError {
        message: format!("{url} answered with more than {MAX_RESPONSE_BYTES} bytes"),
        code: DISCOVERY_FAILED,
    };
    if let Some(declared) = response.content_length()
        && declared > MAX_RESPONSE_BYTES as u64 {
            return Err(oversized());
        }
    let mut response = response;
    let mut total = 0usize;
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| DiscoveryError {
        message: format!("could not reach {url}"),
        code: DISCOVERY_FAILED,
    })? {
        total += chunk.len();
        if total > MAX_RESPONSE_BYTES {
            return Err(oversized());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(models: &[DiscoveredModel]) -> Vec<&str> {
        models.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn listing_url_normalizes_trailing_slash_and_v1() {
        assert_eq!(listing_url("https://api.openai.com/v1/", "openai-completions"), "https://api.openai.com/v1/models");
        assert_eq!(listing_url("https://gw.example/v1", "anthropic-messages"), "https://gw.example/v1/models?limit=1000");
        assert_eq!(listing_url("https://gw.example", "anthropic-messages"), "https://gw.example/v1/models?limit=1000");
        assert_eq!(listing_url("https://gw.example/anthropic/v1/", "anthropic-messages"), "https://gw.example/anthropic/v1/models?limit=1000");
    }

    #[test]
    fn data_array_parses_with_capacity_aliases() {
        let body = json!({
            "data": [
                { "id": "m-1", "name": "Model One", "context_window": 128000, "max_output_tokens": 8192 },
                { "id": "m-2", "contextWindow": 256.0, "maxTokens": "4096" },
                { "id": "m-3", "max_input_tokens": 32000, "limit": { "output": 4096 } },
                { "context_length": 4096 },
            ],
        });
        let models = read_listing(&body).unwrap();
        assert_eq!(ids(&models), ["m-1", "m-2", "m-3"]);
        assert_eq!(models[0].name, "Model One");
        assert_eq!(models[0].context_window, Some(128000));
        assert_eq!(models[0].max_tokens, Some(8192));
        // 整值浮点（256.0）算整数；字符串容量不算；名字回退到 id。
        assert_eq!(models[1].name, "m-2");
        assert_eq!(models[1].context_window, Some(256));
        assert_eq!(models[1].max_tokens, None);
        assert_eq!(models[2].context_window, Some(32000));
        assert_eq!(models[2].max_tokens, Some(4096));
        // 缺可用 id 的行跳过，不失败整次解析。
        assert_eq!(models.len(), 3);
    }

    #[test]
    fn enriched_models_map_uses_property_keys() {
        let body = json!({
            "models": {
                "alias-a": { "id": "canonical/a", "name": "A", "limit": { "context": 8192 } },
                "alias-b": { "contextWindow": 4096, "top_provider": { "max_completion_tokens": 1024 } },
                "metadata": "not a model",
                "": { "id": "fallback-id" },
            },
        });
        let models = read_listing(&body).unwrap();
        assert_eq!(ids(&models), ["alias-a", "alias-b", "fallback-id"]);
        // 键为空时嵌套 id 兜底；嵌套 id 不覆盖非空键（网关放的可能是请求
        // 不接受的规范身份）。
        assert_eq!(models[0].context_window, Some(8192));
        assert_eq!(models[1].max_tokens, Some(1024));
    }

    #[test]
    fn neither_data_nor_models_is_a_coded_refusal() {
        let err = read_listing(&json!({ "models": [1, 2] })).unwrap_err();
        assert_eq!(err.code, DISCOVERY_FAILED);
        assert!(err.message.contains("neither a \"data\" array nor a \"models\" object"));
        // models 数组不是映射：同一条拒绝路径。
        let err = read_listing(&json!({ "other": {} })).unwrap_err();
        assert_eq!(err.code, DISCOVERY_FAILED);
    }

    #[test]
    fn openrouter_shape_reads_nested_capacities() {
        // tests 记录里的 openrouter-2026-09-02.json 形状。
        let body = json!({
            "data": [{
                "id": "deepseek/deepseek-chat",
                "name": "DeepSeek Chat",
                "context_length": 65536,
                "top_provider": { "max_completion_tokens": 8192 },
            }],
        });
        let models = read_listing(&body).unwrap();
        assert_eq!(models[0].context_window, Some(65536));
        assert_eq!(models[0].max_tokens, Some(8192));
    }

    #[test]
    fn probe_key_checks_visible_ascii() {
        assert_eq!(usable_probe_key("  sk-abc123 ").unwrap(), "sk-abc123");
        let e = usable_probe_key("sk abc").unwrap_err();
        assert_eq!(e.code, INVALID_CREDENTIAL);
        let e = usable_probe_key("密钥").unwrap_err();
        assert_eq!(e.code, INVALID_CREDENTIAL);
        let e = usable_probe_key("   ").unwrap_err();
        assert_eq!(e.code, INVALID_CREDENTIAL);
    }
}
