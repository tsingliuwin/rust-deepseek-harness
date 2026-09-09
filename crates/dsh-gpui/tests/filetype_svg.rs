//! 生成的 filetype glyph 必须能被 gpui 同款 svg 管线（usvg 0.45 + resvg）
//! 渲染出非空 alpha——gpui 只取 alpha 通道再运行时 tint，空白即资产问题。
use std::fs;

#[test]
fn filetype_glyphs_render_nonempty_alpha() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/filetype");
    for stem in [
        "code", "excel", "html", "image", "markdown", "other", "pdf", "ppt", "video", "word",
    ] {
        let parts: &[&str] = if stem == "other" { &["body"] } else { &["body", "mark"] };
        for part in parts {
            let path = format!("{dir}/{stem}-{part}.svg");
            let bytes = fs::read(&path).unwrap_or_else(|e| panic!("{path} read: {e}"));
            let tree = usvg::Tree::from_data(&bytes, &usvg::Options::default())
                .unwrap_or_else(|e| panic!("{path} parse: {e}"));
            let mut pixmap = resvg::tiny_skia::Pixmap::new(56, 56).unwrap();
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(2.0, 2.0),
                &mut pixmap.as_mut(),
            );
            let opaque = pixmap.pixels().iter().filter(|p| p.alpha() > 0).count();
            assert!(opaque > 40, "{path} rendered almost empty ({opaque} px)");
        }
    }
}
