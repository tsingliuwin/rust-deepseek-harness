//! Content-block projection helpers for file attachments.
//!
//! Mirrors the file half of upstream
//! [`packages/llm/llm/src/content.ts`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/llm/llm/src/content.ts):
//! no provider ever receives a file block natively, so request assembly
//! replaces every file occurrence — including nested tool results — with
//! deterministic handle text naming the file, its size, and its read-only
//! stored address.

use crate::message::Message;
use crate::types::{ContentBlock, FileAttachmentRef};

/// JSON-stringify a value the way `quoted()` does upstream (compact, escaped).
fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// First 8 hex chars of the digest behind a `sha256:<hex>` attachment id.
fn digest_prefix(attachment_id: &str) -> &str {
    let hex = attachment_id.strip_prefix("sha256:").unwrap_or(attachment_id);
    &hex[..hex.len().min(8)]
}

/// Stable model-facing handle for one durable file reference: the address of
/// the verbatim stored copy plus the instruction to read it on demand. This is
/// the only representation a provider ever receives for a file.
pub fn file_handle_text(ref_: &FileAttachmentRef, readonly_path: Option<&str>) -> String {
    let identity = format!(
        "File {} ({} bytes, sha256:{})",
        quoted(&ref_.name),
        ref_.bytes,
        digest_prefix(&ref_.attachment_id)
    );
    match readonly_path {
        None => format!(
            "[{identity} was uploaded, but the current execution environment cannot access a \
             readable path. Report that limitation if its contents are needed; do not claim to \
             have read it.]"
        ),
        Some(path) => format!(
            "[{identity}: verbatim read-only copy saved at {}. Read that path with your file \
             tools when its contents are needed; copy it to a writable location before modifying \
             it. When delegating file work, include this saved path in the delegation prompt; \
             only subagents sharing this execution environment can read it.]",
            quoted(path)
        ),
    }
}

/// True when typed model content contains a file block, walking nested
/// tool-result content on the same recursion every file policy shares.
pub fn content_has_file(content: &[ContentBlock]) -> bool {
    content.iter().any(|block| match block {
        ContentBlock::File { .. } => true,
        ContentBlock::ToolResult { content, .. } => content_has_file(content),
        _ => false,
    })
}

/// Resolve one reference's current execution-world read path.
pub type FilePathResolver<'a> = dyn Fn(&FileAttachmentRef) -> Option<String> + 'a;

fn replace_files_with_handles(
    blocks: Vec<ContentBlock>,
    resolve_path: &FilePathResolver<'_>,
) -> Vec<ContentBlock> {
    blocks
        .into_iter()
        .map(|block| match block {
            ContentBlock::File { attachment } => ContentBlock::text(file_handle_text(
                &attachment,
                resolve_path(&attachment).as_deref(),
            )),
            ContentBlock::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => ContentBlock::ToolResult {
                tool_call_id,
                content: replace_files_with_handles(content, resolve_path),
                is_error,
            },
            other => other,
        })
        .collect()
}

/// Project durable file history into deterministic handle text for the model
/// request. Messages without file blocks pass through untouched.
pub fn project_files_to_text(
    messages: Vec<Message>,
    resolve_path: &FilePathResolver<'_>,
) -> Vec<Message> {
    if !messages.iter().any(|m| content_has_file(&m.content)) {
        return messages;
    }
    messages
        .into_iter()
        .map(|mut message| {
            message.content = replace_files_with_handles(message.content, resolve_path);
            message
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MessageSource;
    use crate::types::CallId;

    fn file_ref(name: &str, bytes: u64) -> FileAttachmentRef {
        FileAttachmentRef {
            attachment_id: "sha256:0011223344556677889900aabbccddeeff00112233445566778899aabbccddee"
                .into(),
            name: name.into(),
            bytes,
        }
    }

    #[test]
    fn handle_text_matches_upstream_templates() {
        let rf = file_ref("report.pdf", 2048);
        let with_path = file_handle_text(&rf, Some("/home/u/.dsh/attachments/v1/files/00/…/report.pdf"));
        assert_eq!(
            with_path,
            "[File \"report.pdf\" (2048 bytes, sha256:00112233): verbatim read-only copy saved at \
             \"/home/u/.dsh/attachments/v1/files/00/…/report.pdf\". Read that path with your file \
             tools when its contents are needed; copy it to a writable location before modifying \
             it. When delegating file work, include this saved path in the delegation prompt; \
             only subagents sharing this execution environment can read it.]"
        );
        let without = file_handle_text(&rf, None);
        assert_eq!(
            without,
            "[File \"report.pdf\" (2048 bytes, sha256:00112233) was uploaded, but the current \
             execution environment cannot access a readable path. Report that limitation if its \
             contents are needed; do not claim to have read it.]"
        );
    }

    #[test]
    fn projection_replaces_files_in_place_including_tool_results() {
        let rf = file_ref("data.csv", 10);
        let mut message = Message::user(vec![
            ContentBlock::text("请看附件"),
            ContentBlock::File { attachment: rf.clone() },
        ]);
        message.source = MessageSource::User;
        let nested = ContentBlock::ToolResult {
            tool_call_id: CallId("t1".into()),
            content: vec![ContentBlock::File { attachment: rf.clone() }],
            is_error: Some(false),
        };
        let mut assistant = Message::assistant(vec![nested], "p", "m");

        let resolver = |_rf: &FileAttachmentRef| Some("/stored/path".to_string());
        let out = project_files_to_text(vec![message, assistant], &resolver);
        assert!(matches!(&out[0].content[0], ContentBlock::Text { .. }));
        match &out[0].content[1] {
            ContentBlock::Text { text } => {
                assert!(text.contains("saved at \"/stored/path\""), "{text}")
            }
            other => panic!("file block must become handle text: {other:?}"),
        }
        match &out[1].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert!(matches!(&content[0], ContentBlock::Text { .. }));
            }
            other => panic!("tool-result must survive: {other:?}"),
        }
    }

    #[test]
    fn projection_is_noop_without_files() {
        let message = Message::user_text("plain");
        let resolver = |_: &FileAttachmentRef| -> Option<String> { None };
        let out = project_files_to_text(vec![message.clone()], &resolver);
        assert_eq!(out[0], message);
    }
}
