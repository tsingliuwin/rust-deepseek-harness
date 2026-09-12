//! 资源源组合：图标基座来自官方 `gpui-component-assets` crate（与
//! gpui-component 0.5.1 同源同版本，随依赖更新走），本地只维护自有
//! 资产（brands 与 3 个官方目录没有的 svg）。此前 88 个图标逐个
//! include_bytes! 手抄进仓库，升级依赖时无感漂移。

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// 本地自有资产（官方 icons 目录之外的全部）。
const LOCAL: &[(&str, &[u8])] = &[
    ("brands/fish.svg", include_bytes!("../assets/brands/fish.svg")),
    ("brands/hero-glow.png", include_bytes!("../assets/brands/hero-glow.png")),
    ("icons/context-injection.svg", include_bytes!("../assets/icons/context-injection.svg")),
    ("icons/paperclip.svg", include_bytes!("../assets/icons/paperclip.svg")),
    ("icons/document-file.svg", include_bytes!("../assets/icons/document-file.svg")),
    ("icons/clock.svg", include_bytes!("../assets/icons/clock.svg")),
    ("icons/database.svg", include_bytes!("../assets/icons/database.svg")),
    // 权限预设盾标（上游 PermissionSelect permissionGlyphs 三态 + 纯轮廓）
    ("icons/shield.svg", include_bytes!("../assets/icons/shield.svg")),
    (
        "icons/permission-readonly.svg",
        include_bytes!("../assets/icons/permission-readonly.svg"),
    ),
    (
        "icons/permission-workspace.svg",
        include_bytes!("../assets/icons/permission-workspace.svg"),
    ),
    (
        "icons/permission-full.svg",
        include_bytes!("../assets/icons/permission-full.svg"),
    ),
    ("filetype/code-body.svg", include_bytes!("../assets/filetype/code-body.svg")),
    ("filetype/excel-body.svg", include_bytes!("../assets/filetype/excel-body.svg")),
    ("filetype/html-body.svg", include_bytes!("../assets/filetype/html-body.svg")),
    ("filetype/image-body.svg", include_bytes!("../assets/filetype/image-body.svg")),
    ("filetype/markdown-body.svg", include_bytes!("../assets/filetype/markdown-body.svg")),
    ("filetype/other-body.svg", include_bytes!("../assets/filetype/other-body.svg")),
    ("filetype/pdf-body.svg", include_bytes!("../assets/filetype/pdf-body.svg")),
    ("filetype/ppt-body.svg", include_bytes!("../assets/filetype/ppt-body.svg")),
    ("filetype/video-body.svg", include_bytes!("../assets/filetype/video-body.svg")),
    ("filetype/word-body.svg", include_bytes!("../assets/filetype/word-body.svg")),
    ("filetype/code-mark.svg", include_bytes!("../assets/filetype/code-mark.svg")),
    ("filetype/excel-mark.svg", include_bytes!("../assets/filetype/excel-mark.svg")),
    ("filetype/html-mark.svg", include_bytes!("../assets/filetype/html-mark.svg")),
    ("filetype/image-mark.svg", include_bytes!("../assets/filetype/image-mark.svg")),
    ("filetype/markdown-mark.svg", include_bytes!("../assets/filetype/markdown-mark.svg")),
    ("filetype/pdf-mark.svg", include_bytes!("../assets/filetype/pdf-mark.svg")),
    ("filetype/ppt-mark.svg", include_bytes!("../assets/filetype/ppt-mark.svg")),
    ("filetype/video-mark.svg", include_bytes!("../assets/filetype/video-mark.svg")),
    ("filetype/word-mark.svg", include_bytes!("../assets/filetype/word-mark.svg")),
];

pub struct AppAssets {
    icons: gpui_component_assets::Assets,
}

impl AppAssets {
    pub fn new() -> Self {
        Self { icons: gpui_component_assets::Assets }
    }
}

impl Default for AppAssets {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        if let Some((_, bytes)) = LOCAL.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        // 官方源 miss 时返回 Err（anyhow "could not find asset"）——
        // 组合语义里等价于 None
        match self.icons.load(path) {
            Ok(found) => Ok(found),
            Err(_) => Ok(None),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = self.icons.list(path).unwrap_or_default();
        for (p, _) in LOCAL {
            if p.starts_with(path) {
                out.push((*p).into());
            }
        }
        Ok(out)
    }
}
