//! 通用文件附件的内容寻址本地存储。
//!
//! 镜像 `packages/attachment/attachment-local`（file-store.ts / store.ts）：
//! - 字节按 sha256 原样存一份规范对象
//!   `attachments/v1/file-objects/<hex[0..2]>/<hex>`；
//! - 每个模型可见的只读路径
//!   `attachments/v1/files/<hex[0..2]>/<hex>/<name>` 是同一字节的硬链接，
//!   路径以真实文件名结尾（模型可用现有文件工具直接读）；
//! - 上传字节永不删除、无大小上限（GC 与图片保留问题一同挂起——上游同）。
//!
//! 与上游的偏差：不做传输并发/进度（本地存储在附加时一次完成）、图片
//! 规范化流水线不复刻（rustdsh 无 composer 图片捕获面）。

use dsh_llm::FileAttachmentRef;
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

/// 由会话根（`DSH_HOME/sessions`）推导附件根（`DSH_HOME/attachments/v1`）。
pub fn attachments_root_from_sessions_root(sessions_root: &Path) -> PathBuf {
    sessions_root
        .parent()
        .unwrap_or(sessions_root)
        .join("attachments")
        .join("v1")
}

const FILE_ID_PATTERN_LEN: usize = 64; // sha256 hex 长度

/// `DSH_HOME/attachments/v1` 根。
pub struct AttachmentStore {
    root: PathBuf,
}

impl AttachmentStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// 提交一份字节（原样、字节级精确）：发布规范对象 + 显示名硬链接别名。
    /// 相同字节的重复提交收敛到同一对象（EEXIST 校验通过后静默成功）。
    pub fn save_file_verbatim(
        &self,
        bytes: &[u8],
        display_name: Option<&str>,
    ) -> io::Result<FileAttachmentRef> {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let hex = format!("{:x}", hasher.finalize());
        let ref_ = FileAttachmentRef {
            attachment_id: format!("sha256:{hex}"),
            name: file_leaf_name(display_name),
            bytes: bytes.len() as u64,
        };
        let object = self.object_path(&hex);
        publish_immutable_object(&object, bytes, &hex)?;
        let alias = self.stored_file_path(&ref_)?;
        publish_immutable_alias(&object, &alias, &hex)?;
        Ok(ref_)
    }

    /// 规范对象路径：`file-objects/<hex[0..2]>/<hex>`。
    pub fn object_path(&self, hex: &str) -> PathBuf {
        self.root.join("file-objects").join(&hex[..2]).join(hex)
    }

    /// 只读别名路径：`files/<hex[0..2]>/<hex>/<name>`（名称必须是清洗后的
    /// 叶名——引用校验同上游 ensureFileReference）。
    pub fn stored_file_path(&self, ref_: &FileAttachmentRef) -> io::Result<PathBuf> {
        let hex = ensure_file_reference(ref_)?;
        Ok(self.root.join("files").join(&hex[..2]).join(hex).join(&ref_.name))
    }

    /// 模型可见的执行世界只读路径（无法解析时 None——投影 handle 文本用）。
    pub fn file_host_path(&self, ref_: &FileAttachmentRef) -> Option<PathBuf> {
        let path = self.stored_file_path(ref_).ok()?;
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }
}

/// 引用校验：attachmentId 必须是 `sha256:<64hex>`，名称必须是清洗后的叶名。
fn ensure_file_reference(ref_: &FileAttachmentRef) -> io::Result<String> {
    let hex = ref_
        .attachment_id
        .strip_prefix("sha256:")
        .filter(|h| h.len() == FILE_ID_PATTERN_LEN && h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "File attachment reference is invalid.",
            )
        })?;
    if ref_.name.is_empty() || ref_.name != file_leaf_name(Some(&ref_.name)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "File attachment reference is invalid.",
        ));
    }
    Ok(hex.to_string())
}

/// 原子发布规范对象；已存在则校验字节一致（内容寻址的幂等发布）。
fn publish_immutable_object(path: &Path, bytes: &[u8], hex: &str) -> io::Result<()> {
    if path.exists() {
        return verify_digest(path, hex);
    }
    std::fs::create_dir_all(path.parent().expect("object has parent"))?;
    let tmp = path.with_extension(format!(
        "tmp-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists || path.exists() => {
            let _ = std::fs::remove_file(&tmp);
            verify_digest(path, hex)?;
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    make_read_only(path);
    Ok(())
}

/// 发布硬链接别名（EEXIST = 已存在，校验字节一致即可）。
fn publish_immutable_alias(object: &Path, alias: &Path, hex: &str) -> io::Result<()> {
    std::fs::create_dir_all(alias.parent().expect("alias has parent"))?;
    match std::fs::hard_link(object, alias) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            verify_digest(alias, hex)?;
        }
        // 跨卷/不支持硬链接的文件系统：退回复制（别名语义=只读副本，上游
        // 无此分支——Node 在不支持 link 的场景直接抛错；Windows 跨卷场景
        // rustdsh 优先保证可用性）
        Err(_) if !alias.exists() => {
            std::fs::copy(object, alias)?;
            verify_digest(alias, hex)?;
        }
        Err(e) => return Err(e),
    }
    make_read_only(alias);
    Ok(())
}

fn verify_digest(path: &Path, hex: &str) -> io::Result<()> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = format!("{:x}", hasher.finalize());
    if actual != hex {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Stored attachment failed integrity verification.",
        ));
    }
    Ok(())
}

fn make_read_only(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(true);
        let _ = std::fs::set_permissions(path, perms);
    }
}

fn is_windows_device_name(name: &str) -> bool {
    let stem: String = match name.find('.') {
        Some(dot) => name[..dot].to_string(),
        None => name.to_string(),
    };
    let stem = stem.trim_end_matches(['.', ' ']).to_ascii_lowercase();
    matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul"
            | "com1" | "com2" | "com3" | "com4" | "com5" | "com6" | "com7" | "com8" | "com9"
            | "lpt1" | "lpt2" | "lpt3" | "lpt4" | "lpt5" | "lpt6" | "lpt7" | "lpt8" | "lpt9"
    )
}

/// UTF-8 字节预算内的最长前缀（不劈开字符）。
fn utf8_prefix(value: &str, max_bytes: usize) -> String {
    let mut bytes = 0usize;
    let mut prefix = String::new();
    for ch in value.chars() {
        let len = ch.len_utf8();
        if bytes + len > max_bytes {
            break;
        }
        prefix.push(ch);
        bytes += len;
    }
    prefix
}

/// 把调用方显示名清洗为安全的存储叶名（上游 `fileLeafName` 同款）：
/// 两种分隔符都手工剥离（POSIX 主机不能让 `\` 把客户端完整路径泄进日志）、
/// 控制字符删除、Windows 非法字符替换为 `_`、尾点/空格剥离、设备名前缀
/// `_`、UTF-8 255 字节截断、空名回落 `file`。
pub fn file_leaf_name(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "file".into();
    };
    let leaf_start = value
        .rfind(['/', '\\'])
        .map(|i| i + 1)
        .unwrap_or(0);
    let leaf = &value[leaf_start..];
    let mut clean: String = leaf
        .chars()
        .filter(|c| !matches!(c, '\u{0000}'..='\u{001f}' | '\u{007f}'))
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    clean = clean.trim().to_string();
    clean = clean.trim_end_matches(['.', ' ']).to_string();
    if is_windows_device_name(&clean) {
        clean = format!("_{clean}");
    }
    clean = utf8_prefix(&clean, 255);
    clean = clean.trim_end_matches(['.', ' ']).to_string();
    if clean.is_empty() || clean == "." || clean == ".." {
        return "file".into();
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_name_sanitization_matches_upstream() {
        assert_eq!(file_leaf_name(None), "file");
        assert_eq!(file_leaf_name(Some("")), "file");
        assert_eq!(file_leaf_name(Some(".")), "file");
        assert_eq!(file_leaf_name(Some("..")), "file");
        assert_eq!(file_leaf_name(Some("C:\\Users\\lyq\\report final.pdf")), "report final.pdf");
        assert_eq!(file_leaf_name(Some("/tmp/data/表1.csv")), "表1.csv");
        assert_eq!(file_leaf_name(Some("a<b>:c?.txt")), "a_b__c_.txt");
        assert_eq!(file_leaf_name(Some("name....")), "name");
        assert_eq!(file_leaf_name(Some("CON")), "_CON");
        assert_eq!(file_leaf_name(Some("com1.txt")), "_com1.txt");
        assert_eq!(file_leaf_name(Some("prn. ")), "_prn");
        assert_eq!(file_leaf_name(Some("\u{0007}bell")), "bell");
        // 255 UTF-8 字节截断不劈字符（'汉'=3 字节）
        let long = "汉".repeat(100);
        let cut = file_leaf_name(Some(&long));
        assert_eq!(cut.chars().count(), 85);
        assert_eq!(cut.len(), 255);
    }

    #[test]
    fn verbatim_save_is_deduped_and_aliased() {
        let dir = std::env::temp_dir().join(format!("dsh-attach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = AttachmentStore::new(dir.join("attachments").join("v1"));
        let a = store.save_file_verbatim(b"hello world", Some("notes.txt")).unwrap();
        assert_eq!(a.name, "notes.txt");
        assert_eq!(a.bytes, 11);
        assert!(a.attachment_id.starts_with("sha256:"));
        // 相同字节不同名：同一对象、不同别名
        let b = store.save_file_verbatim(b"hello world", Some("别名.md")).unwrap();
        assert_eq!(a.attachment_id, b.attachment_id);
        assert_ne!(a.name, b.name);
        // 对象与别名都在，内容一致
        let obj = store.object_path(&a.attachment_id["sha256:".len()..]);
        assert!(obj.exists());
        assert_eq!(std::fs::read(&obj).unwrap(), b"hello world");
        let alias = store.stored_file_path(&b).unwrap();
        assert!(alias.exists());
        assert_eq!(std::fs::read(&alias).unwrap(), b"hello world");
        assert!(store.file_host_path(&a).is_some());
        // 引用校验：篡改 id / 名称被拒
        let mut bad = a.clone();
        bad.attachment_id = "sha256:zz".into();
        assert!(store.stored_file_path(&bad).is_err());
        bad = a.clone();
        bad.name = "../escape".into();
        assert!(store.stored_file_path(&bad).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
