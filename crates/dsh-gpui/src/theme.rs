//! 设计 token —— 1:1 对齐参考 web 版 `packages/client/ui-theme` 的双主题。
//!
//! 数值出处（deepseek-harness）：
//! - 静态色板：`ui-theme/src/styles/design-platform.css`（亮色 `body` 段 +
//!   暗色 `body[data-ds-dark-theme]` 段）
//! - 别名层：同文件 `--dsw-alias-*`（bg-base、bubble、input-major、sidebar…）
//! - 字号标尺：`ui-theme/src/styles/gradient-shadow-text.css`（`--dsw-font-*`）
//!
//! 运行时经 [`t()`] 取当前主题（亮/暗切换由 settings 页驱动，[`apply`]
//! 同步 gpui-component 全局主题与高亮主题）。

use std::sync::RwLock;

use gpui::{px, hsla, Hsla, Rgba};use gpui_component::highlighter::HighlightTheme;
use gpui_component::theme::{Theme, ThemeMode};

/// `rgb()`/`hsla()` 非 const，这里提供 const 版本。
const fn rgb_const(hex: u32) -> Rgba {
    let [_, r, g, b] = hex.to_be_bytes();
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

const fn hsla_const(h: f32, s: f32, l: f32, a: f32) -> Hsla {
    Hsla { h, s, l, a }
}

/// 一套主题 token（字段名沿用 web 别名层语义；预留字段供后续细节取用）。
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct Tokens {
    /// `--dsw-alias-bg-base` 页面底色。
    pub bg_base: Rgba,
    /// `--dsw-specific-sidebar-fill` 侧栏。
    pub sidebar_bg: Rgba,
    /// `--dsw-alias-bg-layer-1`。
    pub layer1: Rgba,
    /// 输入卡 / 代码卡表面（暗=input-major 850；亮=00 白）。
    pub surface: Rgba,
    /// 选择器 / 浮起按钮 hover（暗=bluish-800；亮=bluish-60）。
    pub surface_2: Rgba,
    /// 激活项 / elevated（暗=bluish-750；亮=bluish-75）。
    pub elevated: Rgba,
    /// `--dsw-specific-bubble` 用户气泡（暗=850；亮=deepseek-50 淡蓝）。
    pub bubble: Rgba,
    /// `--dsw-alias-label-primary`。
    pub text: Rgba,
    /// `--dsw-alias-label-secondary`。
    pub text_2: Rgba,
    /// `--dsw-alias-label-tertiary`。
    pub text_3: Rgba,
    /// `--dsw-alias-label-caption`。
    pub caption: Rgba,
    /// `--dsw-alias-state-business-primary`（发送按钮/链接/激活 tab）。
    pub accent: Rgba,
    /// 蓝 hover。
    pub accent_hover: Rgba,
    /// `--dsw-alias-state-error-primary`。
    pub error: Rgba,
    /// `--dsw-alias-state-success-primary`。
    pub green: Rgba,
    /// `--dsw-alias-state-business-tertiary`（hero 预览版 badge 底）。
    pub business_tertiary: Rgba,
    /// `--dsw-alias-label-primary-bluish`（hero 预览版 badge 字）。
    pub text_bluish: Rgba,
    /// `--dsw-specific-menu`（下拉菜单卡底，亮=00 白 / 暗=bluish-800）。
    pub menu: Rgba,
    /// `--dsw-alias-border-inverted`（菜单卡描边，亮=透明 / 暗=白 6%）。
    pub border_inverted: Hsla,
    /// markdown 代码块底。
    pub code_bg: Rgba,
    /// 代码块横幅底。
    pub code_banner: Rgba,
    /// `--dsw-alias-border-l1`。
    pub border_l1: Hsla,
    /// `--dsw-alias-border-l2`。
    pub border_l2: Hsla,
    /// `--dsw-alias-border-l3`。
    pub border_l3: Hsla,
    /// `--dsw-alias-border-l4`（轮次导航梯常态 tick）。
    pub border_l4: Hsla,
    /// `--dsw-alias-interactive-bg-hover`。
    pub hover: Hsla,
    /// `--dsw-alias-interactive-bg-active`。
    pub active: Hsla,
    /// `--dsw-alias-bg-mask-1`（设置弹层遮罩）。
    pub mask: Hsla,
    /// 滚动条 thumb（scrollbar-bg-l2 对）。
    pub scrollbar_thumb: Rgba,
    pub scrollbar_thumb_hover: Rgba,
    /// `--dsw-alias-bg-layer-3`（轨迹 cell 卡底 / 菜单卡底同值）。
    pub layer3: Rgba,
    /// `--dsw-alias-button-ghost-active-fill`（轨迹轮次吸顶条底）。
    pub ghost_active: Rgba,
    /// `--dsw-alias-bg-module-platform`（轨迹 system tag 底）。
    pub module_platform: Rgba,
    /// `--dsw-alias-state-warn-label`（轨迹 tool tag 字）。
    pub warn_label: Rgba,
    /// `--dsw-alias-state-warn-tertiary`（轨迹 tool tag 底）。
    pub warn_tertiary: Rgba,
    /// `--dsw-alias-state-success-tertiary`（轨迹 user/context tag 底）。
    pub success_tertiary: Rgba,
    /// 轨迹 message tag 字（color-mix(brand-new 60%, error-secondary) 实值）。
    pub tag_message_fg: Rgba,
    /// 轨迹 message tag 底（color-mix(…55%…15%, bg-layer-1) 实值）。
    pub tag_message_bg: Rgba,
    /// 轨迹 context tag 字（color-mix(success 68%, label-secondary) 实值）。
    pub tag_context_fg: Rgba,
    /// 轨迹 subtool tag 字（color-mix(warn-label 62%, label-tertiary) 实值）。
    pub tag_subtool_fg: Rgba,
    /// 轨迹 subtool tag 底（color-mix(warn-tertiary 58%, bg-layer-1) 实值）。
    pub tag_subtool_bg: Rgba,
}

/// 暗色主题（`body[data-ds-dark-theme]`）。
pub const DARK: Tokens = Tokens {
    bg_base: rgb_const(0x151517),        // neutral-bluish-950
    sidebar_bg: rgb_const(0x1b1b1c),     // bluish-900
    layer1: rgb_const(0x232324),         // bluish-875
    surface: rgb_const(0x2c2c2e),        // bluish-850 (input-major)
    surface_2: rgb_const(0x353638),      // bluish-800
    elevated: rgb_const(0x43454a),       // bluish-750
    bubble: rgb_const(0x2c2c2e),         // bluish-850
    text: rgb_const(0xf9fafb),           // bluish-50
    text_2: rgb_const(0xcfd3d6),         // bluish-300
    text_3: rgb_const(0xadb2b8),         // bluish-400
    caption: rgb_const(0x81858c),        // bluish-600
    accent: rgb_const(0x679efe),         // deepseek-400
    accent_hover: rgb_const(0x4176e6),   // deepseek-500
    error: rgb_const(0xf25a5a),          // red-400
    green: rgb_const(0x22c55e),          // green-500
    business_tertiary: rgb_const(0x34415b), // deepseek-800
    text_bluish: rgb_const(0xf9fafb),    // neutral-bluish-50
    menu: rgb_const(0x353638),           // neutral-bluish-800 (bg-layer-3)
    border_inverted: hsla_const(0.0, 0.0, 1.0, 0.06),
    code_bg: rgb_const(0x1b1b1c),        // bluish-900
    code_banner: rgb_const(0x2c2c2e),    // bluish-850
    border_l1: hsla_const(0.0, 0.0, 1.0, 0.06),
    border_l2: hsla_const(0.0, 0.0, 1.0, 0.12),
    border_l3: hsla_const(0.0, 0.0, 1.0, 0.16),
    border_l4: hsla_const(0.0, 0.0, 1.0, 0.20),
    hover: hsla_const(0.0, 0.0, 1.0, 0.08),
    active: hsla_const(0.0, 0.0, 1.0, 0.14),
    mask: hsla_const(0.0, 0.0, 0.0, 0.5),
    scrollbar_thumb: rgb_const(0x545557),       // neutral-600
    scrollbar_thumb_hover: rgb_const(0x65676b), // neutral-550
    layer3: rgb_const(0x353638),                // neutral-bluish-800
    ghost_active: rgb_const(0x43454a),          // neutral-bluish-750
    module_platform: rgb_const(0x353638),       // neutral-bluish-800
    warn_label: rgb_const(0xdd8629),            // amber-600
    warn_tertiary: rgb_const(0x27241f),         // amber-900
    success_tertiary: rgb_const(0x233c2c),      // green-900
    tag_message_fg: rgb_const(0x9474bc),        // mix(brand-new 60%, red-400)
    tag_message_bg: rgb_const(0x352f3a),        // mix(…55%… 15%, layer-1)
    tag_context_fg: rgb_const(0x59c984),        // mix(green-500 68%, label-2)
    tag_subtool_fg: rgb_const(0xcb975f),        // mix(amber-600 62%, label-3)
    tag_subtool_bg: rgb_const(0x252421),        // mix(amber-900 58%, layer-1)
};

/// 亮色主题（`body` 默认段）。
pub const LIGHT: Tokens = Tokens {
    bg_base: rgb_const(0xffffff),        // bluish-00
    sidebar_bg: rgb_const(0xf9fafb),     // bluish-50
    layer1: rgb_const(0xffffff),         // bluish-00
    surface: rgb_const(0xffffff),        // input-major 00
    surface_2: rgb_const(0xf5f6f7),      // bluish-60 (selector)
    elevated: rgb_const(0xf1f3f5),       // bluish-75 (hover-solid)
    bubble: rgb_const(0xedf3fe),         // deepseek-50
    text: rgb_const(0x0f1115),           // bluish-1000
    text_2: rgb_const(0x61666b),         // bluish-700
    text_3: rgb_const(0x81858c),         // bluish-600
    caption: rgb_const(0xadb2b8),        // bluish-400
    accent: rgb_const(0x4176e6),         // deepseek-500
    accent_hover: rgb_const(0x679efe),   // deepseek-400
    error: rgb_const(0xec1313),          // red-600
    green: rgb_const(0x22c55e),          // green-500
    business_tertiary: rgb_const(0xe4edfd), // deepseek-100
    text_bluish: rgb_const(0x0e3074),    // blue-900
    menu: rgb_const(0xffffff),           // neutral-bluish-00 (bg-layer-3)
    border_inverted: hsla_const(0.0, 0.0, 0.0, 0.0),
    code_bg: rgb_const(0xf9fafb),        // bluish-50
    code_banner: rgb_const(0xf9fafb),    // bluish-50
    border_l1: hsla_const(0.0, 0.0, 0.0, 0.04),
    border_l2: hsla_const(0.0, 0.0, 0.0, 0.10),
    border_l3: hsla_const(0.0, 0.0, 0.0, 0.12),
    border_l4: hsla_const(0.0, 0.0, 0.0, 0.16),
    hover: hsla_const(0.625, 0.31, 0.216, 0.06),  // rgba(38,49,72,.06)
    active: hsla_const(0.625, 0.31, 0.216, 0.10), // rgba(38,49,72,.10)
    mask: hsla_const(0.0, 0.0, 0.0, 0.24),
    scrollbar_thumb: rgb_const(0xe5e5e5),       // neutral-200
    scrollbar_thumb_hover: rgb_const(0xd4d4d4), // neutral-300
    layer3: rgb_const(0xffffff),                // neutral-bluish-00
    ghost_active: rgb_const(0xebeef2),          // neutral-bluish-100
    module_platform: rgb_const(0xf5f6f7),       // neutral-bluish-60
    warn_label: rgb_const(0xdd8629),            // amber-600
    warn_tertiary: rgb_const(0xfef5e7),         // amber-100
    success_tertiary: rgb_const(0xe6faed),      // green-100
    tag_message_fg: rgb_const(0x886bae),        // mix(brand-new 60%, red-400)
    tag_message_bg: rgb_const(0xeee8f2),        // mix(…55%… 15%, layer-1)
    tag_context_fg: rgb_const(0x36a762),        // mix(green-500 68%, label-2)
    tag_subtool_fg: rgb_const(0xba864f),        // mix(amber-600 62%, label-3)
    tag_subtool_bg: rgb_const(0xfef9f1),        // mix(amber-100 58%, layer-1)
};

static ACTIVE: RwLock<Tokens> = RwLock::new(DARK);

/// 当前主题 token（Copy，读锁极短）。
pub fn t() -> Tokens {
    *ACTIVE.read().unwrap()
}

/// 当前生效主题是否为暗色。
pub fn is_dark() -> bool {
    t().bg_base == DARK.bg_base
}

fn set_tokens(tokens: Tokens) {
    *ACTIVE.write().unwrap() = tokens;
}

/// 装配 gpui-component 全局主题 + 本模块 token。
pub fn apply(mode: ThemeMode, cx: &mut gpui::App) {
    let dark = mode.is_dark();
    Theme::change(mode, None, cx);
    set_tokens(if dark { DARK } else { LIGHT });

    let theme = Theme::global_mut(cx);
    theme.highlight_theme = if dark {
        HighlightTheme::default_dark()
    } else {
        HighlightTheme::default_light()
    };

    let tk = t();
    let c = &mut theme.colors;
    c.background = tk.bg_base.into();
    c.foreground = tk.text.into();
    c.border = tk.border_l2;
    c.input = tk.surface.into();
    c.overlay = tk.surface_2.into();

    c.sidebar = tk.sidebar_bg.into();
    c.sidebar_foreground = tk.text.into();
    c.sidebar_border = tk.border_l1;
    c.sidebar_accent = tk.hover;
    c.sidebar_accent_foreground = tk.text.into();

    c.primary = tk.accent.into();
    c.primary_foreground = rgb_const(0xffffff).into();
    c.primary_hover = tk.accent_hover.into();
    c.primary_active = tk.accent_hover.into();
    c.secondary = tk.surface_2.into();
    c.secondary_foreground = tk.text_2.into();
    c.secondary_hover = tk.elevated.into();
    c.muted = tk.layer1.into();
    c.muted_foreground = tk.text_3.into();

    c.link = tk.accent.into();
    c.link_hover = tk.accent_hover.into();
    c.link_active = tk.accent_hover.into();
    c.danger = tk.error.into();
    c.danger_foreground = rgb_const(0xffffff).into();
    c.success = tk.green.into();

    // inline code 底色：上游 0.1.3-alpha.1 从 neutral-bluish 换
    // neutral-50（亮 rgb(250,250,250)）/ neutral-800（暗 rgb(41,41,41)）；
    // gpui-component 以 accent 色承接口内代码高亮（vendor TextView node.rs）
    c.accent = if dark {
        rgb_const(0x292929).into()
    } else {
        rgb_const(0xFAFAFA).into()
    };
    c.accent_foreground = tk.text.into();
    c.caret = tk.accent.into();
    c.ring = tk.accent.into();
    c.selection = if dark {
        hsla(0.61, 0.98, 0.7, 0.35)
    } else {
        hsla(0.61, 0.85, 0.55, 0.25)
    };
    c.scrollbar = gpui::transparent_black();
    c.scrollbar_thumb = tk.scrollbar_thumb.into();
    c.scrollbar_thumb_hover = tk.scrollbar_thumb_hover.into();

    c.title_bar = tk.bg_base.into();
    c.title_bar_border = tk.border_l1;
    c.tab_bar = tk.bg_base.into();
    c.tab = tk.bg_base.into();
    c.tab_active = tk.bg_base.into();
    c.tab_foreground = tk.text_3.into();
    c.tab_active_foreground = tk.accent.into();
    c.window_border = tk.bg_base.into();
}

// --- Elevation（web gradient-shadow-text.css 的 --dsw-elevation-* 三件套） ---
//
// web 0.1.2-alpha.4 起浮层一律 border:0，改用 box-shadow 画 0.5px 发丝描边
// （不占布局）+ 两层极淡柔光；描边色经 --dsw-elevation-stroke-color 逐组件
// 重绑（菜单面 l1、输入卡 l2、悬浮件默认 l4）。GPUI 用 spread 圆环等价
// `0 0 0 0.5px color`；柔光暗色下几乎不可见（web 注释同），数值照抄。

const fn elevation_stroke(stroke_color: Hsla) -> gpui::BoxShadow {
    gpui::BoxShadow {
        color: stroke_color,
        offset: gpui::point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(0.5),
    }
}

const fn elevation_glow(x: f32, y: f32, blur: f32, alpha: f32) -> gpui::BoxShadow {
    gpui::BoxShadow {
        color: hsla_const(0.0, 0.0, 0.0, alpha),
        offset: gpui::point(px(x), px(y)),
        blur_radius: px(blur),
        spread_radius: px(0.0),
    }
}

/// `--dsw-elevation-panel`（描边取默认 l4）：悬浮按钮、轮次预览卡、tooltip。
pub fn elevation_panel() -> Vec<gpui::BoxShadow> {
    elevation_panel_with(t().border_l4)
}

/// `--dsw-elevation-panel` + 重绑描边色（web `.scroll` 重绑 l3）。
pub fn elevation_panel_with(stroke_color: Hsla) -> Vec<gpui::BoxShadow> {
    vec![
        elevation_stroke(stroke_color),
        elevation_glow(0.0, 3.0, 8.0, 0.03),
        elevation_glow(0.0, 0.0, 16.0, 0.02),
    ]
}

/// `--dsw-elevation-prominent`（描边重绑 l1）：菜单/弹窗等浮层面板。
pub fn elevation_prominent() -> Vec<gpui::BoxShadow> {
    let tk = t();
    vec![
        elevation_stroke(tk.border_l1),
        elevation_glow(0.0, 3.0, 8.0, 0.04),
        elevation_glow(0.0, 0.0, 20.0, 0.05),
    ]
}

/// `--dsw-elevation-soft`（描边重绑 l2）：输入卡。
pub fn elevation_soft() -> Vec<gpui::BoxShadow> {
    let tk = t();
    vec![
        elevation_stroke(tk.border_l2),
        elevation_glow(0.0, 4.0, 16.0, 0.03),
        elevation_glow(0.0, 0.0, 24.0, 0.03),
    ]
}

/// `--dsw-elevation-soft` + workspace-trigger 态（web `.cardWorkspaceTrigger`）：
/// 描边置 transparent，只保留柔光。
pub fn elevation_soft_inert() -> Vec<gpui::BoxShadow> {
    vec![
        elevation_stroke(hsla_const(0.0, 0.0, 0.0, 0.0)),
        elevation_glow(0.0, 4.0, 16.0, 0.03),
        elevation_glow(0.0, 0.0, 24.0, 0.03),
    ]
}

// --- 字号标尺（`--dsw-font-*`，主题无关） -----------------------------------

#[allow(dead_code)]
pub const FONT_MARKDOWN_BASE: f32 = 14.0; // 14/24 正文（web --dsw-font-markdown-base）
#[allow(dead_code)]
pub const FONT_MARKDOWN_BASE_LEADING: f32 = 24.0;
/// 用户气泡 14/22（web .bubble：--dsh-content-font-size 14px + 22px 行高，
/// Figma 单行气泡 42px = 22 + 上下 10px；曾误用 16/24 偏大一号）。
pub const FONT_BUBBLE: f32 = 14.0;
pub const FONT_BUBBLE_LEADING: f32 = 22.0;
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
