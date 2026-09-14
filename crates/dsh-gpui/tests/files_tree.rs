//! 工作区文件树 + 文本预览纯逻辑（dsh_gpui files 面）回归：
//! 自然序/目录优先排序、childPath 键、工作区围栏、分页与失败面。

use dsh_gpui::{
    DirEntry, DirEntryKind, FilesErrorKind, PreviewErrorKind, child_path, files_failure_line,
    list_dir, natural_cmp, order_entries, preview_failure_line, read_text_page, within_root,
};
use std::fs;
use std::path::PathBuf;

fn entry(name: &str, kind: DirEntryKind) -> DirEntry {
    DirEntry { name: name.into(), kind, size: None }
}

#[test]
fn natural_order_numeric_and_case_insensitive() {
    assert_eq!(natural_cmp("file2", "file10"), std::cmp::Ordering::Less);
    assert_eq!(natural_cmp("File2", "file10"), std::cmp::Ordering::Less);
    assert_eq!(natural_cmp("a2b", "a10b"), std::cmp::Ordering::Less);
    assert_eq!(natural_cmp("abc", "abd"), std::cmp::Ordering::Less);
    assert_eq!(natural_cmp("a", "a1"), std::cmp::Ordering::Less);
}

#[test]
fn order_entries_directories_first() {
    let mut es = vec![
        entry("zeta.txt", DirEntryKind::File),
        entry("alpha", DirEntryKind::Directory),
        entry("Beta.md", DirEntryKind::File),
        entry("10dir", DirEntryKind::Directory),
    ];
    es = order_entries(&es);
    let names: Vec<&str> = es.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["10dir", "alpha", "Beta.md", "zeta.txt"]);
}

#[test]
fn child_path_joins_with_slash_and_trims_trailing() {
    assert_eq!(child_path("E:/ws/", ".cargo"), "E:/ws/.cargo");
    assert_eq!(child_path("E:\\ws\\", "crates"), "E:\\ws/crates");
}

#[test]
fn within_root_boundary_and_separator_forms() {
    assert!(within_root("E:/ws", "E:/ws"));
    assert!(within_root("E:/ws", "E:/ws/sub/file.rs"));
    assert!(within_root("E:\\WS", "e:/ws/x"));
    assert!(!within_root("E:/ws", "E:/ws-evil/x"));
    assert!(!within_root("E:/ws", "E:/other"));
}

#[test]
fn failure_lines_match_upstream_vocab() {
    assert_eq!(
        files_failure_line(FilesErrorKind::NotFound, ""),
        "这个目录不在了。可能已被移动或删除。"
    );
    assert_eq!(
        files_failure_line(FilesErrorKind::OutsideWorkspace, ""),
        "这个目录在工作区之外，侧栏不会读取它。"
    );
    assert_eq!(files_failure_line(FilesErrorKind::NotDirectory, ""), "这不是一个目录。");
    assert_eq!(
        files_failure_line(FilesErrorKind::Unavailable, "boom"),
        "读取失败：boom"
    );
    assert_eq!(
        preview_failure_line(&PreviewErrorKind::TooLarge { limit: 2 * 1024 * 1024 }),
        "单页内容超过 2 MB 上限，无法读取。"
    );
    assert_eq!(
        preview_failure_line(&PreviewErrorKind::NotText),
        "非文本文件，暂时无法预览。"
    );
    assert_eq!(
        preview_failure_line(&PreviewErrorKind::NotFound),
        "文件不存在，可能已被移动或删除。"
    );
}

fn temp_ws(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustdsh-files-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("crates/dsh-gpui")).unwrap();
    fs::write(d.join("README.md"), "# hi\n").unwrap();
    fs::write(d.join("crates/dsh-gpui/lib.rs"), "line1\nline2\n").unwrap();
    d
}

#[test]
fn list_dir_orders_and_reports_entries() {
    let ws = temp_ws("list");
    let level = list_dir(ws.to_str().unwrap(), ws.to_str().unwrap()).unwrap();
    let ordered = order_entries(&level.entries);
    let names: Vec<&str> = ordered.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["crates", "README.md"]);
    assert!(!level.truncated);
    let sub = list_dir(ws.to_str().unwrap(), ws.join("crates/dsh-gpui").to_str().unwrap())
        .unwrap();
    assert_eq!(sub.entries.len(), 1);
    assert_eq!(sub.entries[0].name, "lib.rs");
    assert_eq!(sub.entries[0].size, Some("line1\nline2\n".len() as u64));
}

#[test]
fn list_dir_failures_outside_notfound_notdir() {
    let ws = temp_ws("fail");
    let root = ws.to_str().unwrap().to_string();
    assert_eq!(
        list_dir(&root, ws.join("nope").to_str().unwrap()).unwrap_err().0,
        FilesErrorKind::NotFound
    );
    assert_eq!(
        list_dir(&root, ws.join("README.md").to_str().unwrap()).unwrap_err().0,
        FilesErrorKind::NotDirectory
    );
    // 工作区外（含 .. 逃逸）拒绝
    let outside = ws.parent().unwrap().to_str().unwrap().to_string();
    assert_eq!(list_dir(&root, &outside).unwrap_err().0, FilesErrorKind::OutsideWorkspace);
}

#[test]
fn read_text_page_pages_and_detects_binary() {
    let ws = temp_ws("page");
    let root = ws.to_str().unwrap().to_string();
    let p = ws.join("crates/dsh-gpui/lib.rs");
    let page = read_text_page(&root, p.to_str().unwrap(), 0).unwrap();
    assert_eq!(page.text, "line1\nline2\n");
    assert!(page.eof);
    // 大偏移 = 空页但 eof
    let tail = read_text_page(&root, p.to_str().unwrap(), 100).unwrap();
    assert_eq!(tail.lines, 0);
    assert!(tail.eof);
    // NUL 判非文本
    let bin = ws.join("blob.bin");
    fs::write(&bin, [0x61u8, 0x00, 0x62]).unwrap();
    assert_eq!(
        read_text_page(&root, bin.to_str().unwrap(), 0).unwrap_err(),
        PreviewErrorKind::NotText
    );
    // 工作区外拒绝（根外文件）
    let outside_file = ws
        .parent()
        .unwrap()
        .join(format!("rustdsh-outside-{}.txt", std::process::id()));
    fs::write(&outside_file, "x").unwrap();
    assert_eq!(
        read_text_page(&root, outside_file.to_str().unwrap(), 0).unwrap_err(),
        PreviewErrorKind::OutsideWorkspace
    );
    let _ = fs::remove_file(&outside_file);
    let _ = fs::remove_dir_all(&ws);
}