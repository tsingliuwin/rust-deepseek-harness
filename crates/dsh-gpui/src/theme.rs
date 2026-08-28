//! 设计 token —— 1:1 对齐参考 web 版 `packages/client/ui-theme` 的暗色主题。
//!
//! 数值出处（deepseek-harness）：
//! - 静态色板：`ui-theme/src/styles/design-platform.css` 的
//!   `body[data-ds-dark-theme]` 段（neutral-bluish / deepseek / red / green 系）
//! - 别名层：同文件 `--dsw-alias-*`（bg-base、bubble、input-major、sidebar…）
//! - 字号标尺：`ui-theme/src/styles/gradient-shadow-text.css`（`--dsw-font-*`）
//! - 字体栈：`ui-theme/src/styles/base.css`

use gpui::{hsla, Hsla, Rgba};
use gpui_component::highlighter::HighlightTheme;
use gpui_component::theme::Theme;

/// `rgb()` 非 const，这里提供一个 const 版本（Rgba 字段为 pub f32）。
const fn rgb_const(hex: u32) -> Rgba {
    let [_, r, g, b] = hex.to_be_bytes();
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

/// 同理，`hsla()` 也非 const。
const fn hsla_const(h: f32, s: f32, l: f32, a: f32) -> Hsla {
    Hsla { h, s, l, a }
}

// --- 静态色板（dark） ---------------------------------------------------------

/// `--dsw-static-neutral-bluish-950` — `--dsw-alias-bg-base` 页面底色。
pub const BG_BASE: Rgba = rgb_const(0x151517);
/// `--dsw-static-neutral-bluish-900` — `--dsw-specific-sidebar-fill` 侧栏。
pub const SIDEBAR_BG: Rgba = rgb_const(0x1b1b1c);
/// `--dsw-static-neutral-bluish-875` — `--dsw-alias-bg-layer-1`。
pub const LAYER1: Rgba = rgb_const(0x232324);
/// `--dsw-static-neutral-bluish-850` — 气泡（`--dsw-specific-bubble`）、
/// 输入卡（`--dsw-specific-input-major`）、`bg-layer-2`。
pub const SURFACE: Rgba = rgb_const(0x2c2c2e);
/// `--dsw-static-neutral-bluish-800` — 选择器（`--dsw-specific-selector`）、
/// 浮起按钮 hover、`bg-layer-3`。
pub const SURFACE_2: Rgba = rgb_const(0x353638);
/// `--dsw-static-neutral-bluish-750` — 激活项、elevated 按钮、气泡高亮。
pub const ELEVATED: Rgba = rgb_const(0x43454a);

/// `--dsw-alias-label-primary`（neutral-bluish-50）。
pub const TEXT: Rgba = rgb_const(0xf9fafb);
/// `--dsw-alias-label-secondary`（neutral-bluish-300）。
pub const TEXT_2: Rgba = rgb_const(0xcfd3d6);
/// `--dsw-alias-label-tertiary`（neutral-bluish-400）。
pub const TEXT_3: Rgba = rgb_const(0xadb2b8);
/// `--dsw-alias-label-caption`（neutral-bluish-600）。
pub const CAPTION: Rgba = rgb_const(0x81858c);

/// `--dsw-alias-state-business-primary` = `deepseek-400`（暗色品牌蓝，
/// 发送按钮 `--dsw-alias-button-info-fill`、链接、激活 tab 同源）。
pub const ACCENT: Rgba = rgb_const(0x679efe);
/// `deepseek-500` — 蓝 hover（`--dsw-alias-button-info-hover`）。
pub const ACCENT_HOVER: Rgba = rgb_const(0x4176e6);
/// `--dsw-alias-state-error-primary` = `red-400`（暗色）。
pub const ERROR: Rgba = rgb_const(0xf25a5a);
/// `--dsw-alias-state-success-primary` = `green-500`。
pub const GREEN: Rgba = rgb_const(0x22c55e);

/// markdown 代码块底（`--dsw-alias-markdown-code-block` = bluish-900）。
pub const CODE_BG: Rgba = rgb_const(0x1b1b1c);
/// 代码块横幅底（`--dsw-alias-markdown-code-block-banner` = bluish-850）。
#[allow(dead_code)] // 设计 token 预留，后续细节优化会用到
pub const CODE_BANNER: Rgba = rgb_const(0x2c2c2e);

// --- 边框 / 交互（暗色 = 白色低透明） -----------------------------------------

/// `--dsw-alias-border-l1` / `l2-darkmode-thin`：白 6%。
pub const BORDER_L1: Hsla = hsla_const(0.0, 0.0, 1.0, 0.06);
/// `--dsw-alias-border-l2`：白 12%。
pub const BORDER_L2: Hsla = hsla_const(0.0, 0.0, 1.0, 0.12);
/// `--dsw-alias-border-l3`：白 16%。
#[allow(dead_code)] // 设计 token 预留，后续细节优化会用到
pub const BORDER_L3: Hsla = hsla_const(0.0, 0.0, 1.0, 0.16);
/// `--dsw-alias-interactive-bg-hover`：白 8%（列表行 hover / 选中底）。
pub const HOVER: Hsla = hsla_const(0.0, 0.0, 1.0, 0.08);
/// `--dsw-alias-interactive-bg-active`：白 14%。
#[allow(dead_code)] // 设计 token 预留，后续细节优化会用到
pub const ACTIVE: Hsla = hsla_const(0.0, 0.0, 1.0, 0.14);

// --- 字号标尺（`--dsw-font-*`） -----------------------------------------------
// 仅列出本应用用到的；命名直接沿用 web 版。

#[allow(dead_code)] // 设计 token 预留，后续细节优化会用到
pub const FONT_MARKDOWN_BASE: f32 = 16.0; // 16/28 正文
#[allow(dead_code)] // 设计 token 预留，后续细节优化会用到
pub const FONT_MARKDOWN_BASE_LEADING: f32 = 28.0;
/// 用户气泡 16/24。
pub const FONT_BUBBLE: f32 = 16.0;
pub const FONT_BUBBLE_LEADING: f32 = 24.0;
/// Think / 工具行 summary 14/24。
pub const FONT_ROW: f32 = 14.0;
pub const FONT_ROW_LEADING: f32 = 24.0;
/// 面包屑 / tab 13/16、13/20。
pub const FONT_TAB: f32 = 13.0;
/// caption 12/18。
pub const FONT_CAPTION: f32 = 12.0;
pub const FONT_CAPTION_LEADING: f32 = 18.0;
/// hero 标题 26/32 wt500。
pub const FONT_HERO: f32 = 26.0;
pub const FONT_HERO_LEADING: f32 = 32.0;
/// 品牌字 18/24 wt600。
pub const FONT_BRAND: f32 = 18.0;

/// 装配 gpui-component 全局主题：暗色 + DeepSeek 色板。
pub fn init(cx: &mut gpui::App) {
    Theme::change(gpui_component::theme::ThemeMode::Dark, None, cx);
    let theme = Theme::global_mut(cx);
    theme.highlight_theme = HighlightTheme::default_dark();

    let c = &mut theme.colors;
    c.background = BG_BASE.into();
    c.foreground = TEXT.into();
    c.border = BORDER_L2;
    c.input = SURFACE.into();
    c.overlay = SURFACE_2.into();

    c.sidebar = SIDEBAR_BG.into();
    c.sidebar_foreground = TEXT.into();
    c.sidebar_border = BORDER_L1;
    c.sidebar_accent = HOVER;
    c.sidebar_accent_foreground = TEXT.into();

    c.primary = ACCENT.into();
    c.primary_foreground = rgb_const(0xffffff).into();
    c.primary_hover = ACCENT_HOVER.into();
    c.primary_active = ACCENT_HOVER.into();
    c.secondary = SURFACE_2.into();
    c.secondary_foreground = TEXT_2.into();
    c.secondary_hover = ELEVATED.into();
    c.muted = LAYER1.into();
    c.muted_foreground = TEXT_3.into();

    c.link = ACCENT.into();
    c.link_hover = ACCENT_HOVER.into();
    c.link_active = ACCENT_HOVER.into();
    c.danger = ERROR.into();
    c.danger_foreground = rgb_const(0xffffff).into();
    c.success = GREEN.into();

    c.accent = HOVER;
    c.accent_foreground = TEXT.into();
    c.caret = ACCENT.into();
    c.ring = ACCENT.into();
    c.selection = hsla(0.61, 0.98, 0.7, 0.35);
    // 滚动条（design-platform 暗色 l2 对：thumb neutral-600 / hover neutral-550）
    c.scrollbar = gpui::transparent_black();
    c.scrollbar_thumb = rgb_const(0x545557).into();
    c.scrollbar_thumb_hover = rgb_const(0x65676b).into();

    c.title_bar = BG_BASE.into();
    c.title_bar_border = BORDER_L1;
    c.tab_bar = BG_BASE.into();
    c.tab = BG_BASE.into();
    c.tab_active = BG_BASE.into();
    c.tab_foreground = TEXT_3.into();
    c.tab_active_foreground = ACCENT.into();
    c.window_border = BG_BASE.into();
}
