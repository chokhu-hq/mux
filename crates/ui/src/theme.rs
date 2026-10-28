use std::sync::{Arc, RwLock};

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Margin, Shadow, Stroke, Style,
    TextStyle, Theme, Vec2,
};

pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
}

// one radius per kind of thing, so shapes of the same role always match
pub mod radius {
    // small marks inside other things, like keys, swatches, chart bars and a segmented control's thumb
    pub const SM: u8 = 4;
    // controls and the rows of a menu
    pub const MD: u8 = 6;
    // cards, menus, tooltips, tiles and sidebar rows
    pub const LG: u8 = 10;
    // large floating surfaces like the palette, the composer and message bubbles
    pub const XL: u8 = 14;
    // pills and progress bars
    pub const FULL: u8 = u8::MAX;
}

pub mod text {
    pub const SM: f32 = 11.0;
    pub const MD: f32 = 13.0;
    pub const LG: f32 = 15.0;
    pub const TITLE: f32 = 20.0;
    pub const MONO: f32 = 12.5;
    pub const MONO_SM: f32 = 12.0;
}

pub mod icon {
    pub const SM: f32 = 12.0;
    pub const MD: f32 = 14.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 32.0;
}

pub mod font {
    use egui::{FontFamily, FontId};

    pub const MEDIUM: &str = "inter-medium";
    pub const SEMIBOLD: &str = "inter-semibold";
    pub const MONO_BOLD: &str = "jetbrains-mono-bold";
    pub const MONO_ITALIC: &str = "jetbrains-mono-italic";
    pub const MONO_BOLD_ITALIC: &str = "jetbrains-mono-bold-italic";
    pub const ICON: &str = "phosphor";
    pub const ICON_FILL: &str = "phosphor-fill";

    pub fn regular(size: f32) -> FontId {
        FontId::proportional(size)
    }

    pub fn medium(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(MEDIUM.into()))
    }

    pub fn semibold(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
    }

    pub fn mono(size: f32) -> FontId {
        FontId::monospace(size)
    }

    pub fn mono_bold(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(MONO_BOLD.into()))
    }

    pub fn mono_italic(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(MONO_ITALIC.into()))
    }

    pub fn mono_bold_italic(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(MONO_BOLD_ITALIC.into()))
    }

    pub fn icon(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(ICON.into()))
    }

    pub fn icon_fill(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(ICON_FILL.into()))
    }
}

pub mod control {
    pub const SM: f32 = 24.0;
    pub const MD: f32 = 28.0;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    // the sidebar sits a shade apart from the content so the window reads as two areas
    pub sidebar: Color32,
    pub elevated: Color32,
    pub hover: Color32,
    pub active: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub agent: Color32,
    pub shell: Color32,
    pub task: Color32,
    pub ansi: [Color32; 16],
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const DARK: Palette = Palette {
    bg: hex(0x0E0E10),
    surface: hex(0x161618),
    sidebar: hex(0x151518),
    elevated: hex(0x1C1C1F),
    hover: hex(0x232326),
    active: hex(0x2C2C30),
    border: hex(0x26262A),
    border_strong: hex(0x38383D),
    text: hex(0xEDEDEF),
    text_muted: hex(0xA1A1A6),
    text_faint: hex(0x6E6E73),
    accent: hex(0x0A84FF),
    accent_hover: hex(0x409CFF),
    on_accent: hex(0xFFFFFF),
    success: hex(0x30D158),
    warning: hex(0xFF9F0A),
    danger: hex(0xFF453A),
    agent: hex(0xBF5AF2),
    shell: hex(0x40C8E0),
    task: hex(0x5E5CE6),
    ansi: [
        hex(0x2C2C30),
        hex(0xFF6961),
        hex(0x5BD778),
        hex(0xF5C451),
        hex(0x5AA7FF),
        hex(0xD07EF2),
        hex(0x5BD2E6),
        hex(0xD8D8DC),
        hex(0x6E6E73),
        hex(0xFF8A82),
        hex(0x7EE69A),
        hex(0xFFD978),
        hex(0x82BCFF),
        hex(0xE0A0FF),
        hex(0x8AE3F0),
        hex(0xF5F5F7),
    ],
};

pub const LIGHT: Palette = Palette {
    bg: hex(0xF5F5F7),
    surface: hex(0xFFFFFF),
    sidebar: hex(0xEBEBEE),
    elevated: hex(0xFFFFFF),
    hover: hex(0xE3E3E7),
    active: hex(0xDADADF),
    border: hex(0xE3E3E8),
    border_strong: hex(0xCFCFD4),
    text: hex(0x1D1D1F),
    text_muted: hex(0x6E6E73),
    text_faint: hex(0xA1A1A6),
    accent: hex(0x007AFF),
    accent_hover: hex(0x0062CC),
    on_accent: hex(0xFFFFFF),
    success: hex(0x28A745),
    warning: hex(0xE08600),
    danger: hex(0xFF3B30),
    agent: hex(0xAF52DE),
    shell: hex(0x0FA5BF),
    task: hex(0x5856D6),
    ansi: [
        hex(0x1D1D1F),
        hex(0xD70015),
        hex(0x248A3D),
        hex(0xA05A00),
        hex(0x0058D0),
        hex(0x8944AB),
        hex(0x0071A4),
        hex(0xA1A1A6),
        hex(0x6E6E73),
        hex(0xFF3B30),
        hex(0x2DA44E),
        hex(0xC77C02),
        hex(0x007AFF),
        hex(0xAF52DE),
        hex(0x0A9AB8),
        hex(0xD1D1D6),
    ],
};

// every card sitting in the page, from chat tool calls to settings sections, starts from this frame
pub fn card(ui: &egui::Ui) -> egui::Frame {
    let p = palette(ui);
    egui::Frame::new()
        .fill(tint(p, fill::CARD))
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(radius::LG))
}

// everything that floats above the window, like menus, tooltips and dragged rows, wears the same frame
pub fn floating(ui: &egui::Ui) -> egui::Frame {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.elevated)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(radius::LG))
        .shadow(ui.visuals().popup_shadow)
}

// how strong each kind of tint is, so the same job looks the same everywhere
pub mod fill {
    pub const CARD: f32 = 0.03;
    pub const HOVER: f32 = 0.05;
    // the block behind a tag, a tile or a placeholder
    pub const QUIET: f32 = 0.06;
    // the empty part of a bar or ring
    pub const TRACK: f32 = 0.10;
    pub const SELECTED: f32 = 0.10;
    pub const DISABLED: f32 = 0.12;
}

// a wash of the text colour, which reads the same on solid and see-through backgrounds
pub fn tint(p: &Palette, amount: f32) -> Color32 {
    p.text.gamma_multiply(amount)
}

// the palettes in use for dark and light mode, swapped when another theme is picked
static ACTIVE: RwLock<(&'static Palette, &'static Palette)> = RwLock::new((&DARK, &LIGHT));

pub fn palette(ui: &egui::Ui) -> &'static Palette {
    let active = *ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    if ui.visuals().dark_mode {
        active.0
    } else {
        active.1
    }
}

// a picked palette is leaked because everything borrows it for the frame, and themes change rarely enough not to matter
pub fn set_palettes(ctx: &egui::Context, dark: Palette, light: Palette) {
    let mut active = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
    if *active.0 == dark && *active.1 == light {
        return;
    }
    if *active.0 != dark {
        active.0 = Box::leak(Box::new(dark));
    }
    if *active.1 != light {
        active.1 = Box::leak(Box::new(light));
    }
    let (dark, light) = *active;
    drop(active);
    ctx.style_mut_of(Theme::Dark, |style| apply(style, dark, true));
    ctx.style_mut_of(Theme::Light, |style| apply(style, light, false));
}

pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    // pictures in chat are read from disk by path
    egui_extras::install_image_loaders(ctx);
    // egui's 40 points a wheel notch crawls through long chats and files
    ctx.options_mut(|o| o.input_options.line_scroll_speed = 90.0);
    let (dark, light) = *ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    ctx.style_mut_of(Theme::Dark, |style| apply(style, dark, true));
    ctx.style_mut_of(Theme::Light, |style| apply(style, light, false));
}

fn fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let files: [(&str, &'static [u8]); 9] = [
        ("inter", include_bytes!("../assets/fonts/Inter-Regular.ttf")),
        (
            font::MEDIUM,
            include_bytes!("../assets/fonts/Inter-Medium.ttf"),
        ),
        (
            font::SEMIBOLD,
            include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
        ),
        (
            "jetbrains-mono",
            include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
        ),
        (
            font::MONO_BOLD,
            include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
        ),
        (
            font::MONO_ITALIC,
            include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
        ),
        (
            font::MONO_BOLD_ITALIC,
            include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
        ),
        (font::ICON, include_bytes!("../assets/fonts/Phosphor.ttf")),
        (
            font::ICON_FILL,
            include_bytes!("../assets/fonts/Phosphor-Fill.ttf"),
        ),
    ];
    for (name, bytes) in files {
        defs.font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }

    // icons are drawn with the icon font directly because inter has its own glyphs at some of phosphor's codepoints
    let sans_fallback = defs.families[&FontFamily::Proportional].clone();
    let mono_fallback = defs.families[&FontFamily::Monospace].clone();
    for (family, primary, fallback) in [
        (FontFamily::Proportional, "inter", &sans_fallback),
        (
            FontFamily::Name(font::MEDIUM.into()),
            font::MEDIUM,
            &sans_fallback,
        ),
        (
            FontFamily::Name(font::SEMIBOLD.into()),
            font::SEMIBOLD,
            &sans_fallback,
        ),
        (FontFamily::Monospace, "jetbrains-mono", &mono_fallback),
        (
            FontFamily::Name(font::MONO_BOLD.into()),
            font::MONO_BOLD,
            &mono_fallback,
        ),
        (
            FontFamily::Name(font::MONO_ITALIC.into()),
            font::MONO_ITALIC,
            &mono_fallback,
        ),
        (
            FontFamily::Name(font::MONO_BOLD_ITALIC.into()),
            font::MONO_BOLD_ITALIC,
            &mono_fallback,
        ),
        (FontFamily::Name(font::ICON.into()), font::ICON, &vec![]),
        (
            FontFamily::Name(font::ICON_FILL.into()),
            font::ICON_FILL,
            &vec![],
        ),
    ] {
        let mut list = vec![primary.to_owned()];
        list.extend(fallback.iter().cloned());
        defs.families.insert(family, list);
    }
    defs
}

fn apply(style: &mut Style, p: &Palette, dark: bool) {
    style.text_styles = [
        (TextStyle::Small, font::regular(text::SM)),
        (TextStyle::Body, font::regular(text::MD)),
        (TextStyle::Button, font::medium(text::MD)),
        (TextStyle::Heading, font::semibold(text::TITLE)),
        (TextStyle::Monospace, font::mono(text::MONO)),
    ]
    .into();

    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(space::SM, 6.0);
    s.button_padding = Vec2::new(10.0, 4.0);
    s.interact_size.y = control::MD;
    s.window_margin = Margin::same(space::MD as i8);
    s.menu_margin = Margin::same(space::XS as i8);

    let v = &mut style.visuals;
    v.dark_mode = dark;
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.text_muted);
    v.panel_fill = p.bg;
    v.window_fill = p.elevated;
    v.window_stroke = Stroke::new(1.0, p.border_strong);
    v.window_corner_radius = CornerRadius::same(radius::LG);
    v.menu_corner_radius = CornerRadius::same(radius::LG);
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.surface;
    v.code_bg_color = p.surface;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    // egui paints selected text in this colour, and the accent of some themes is unreadable on its own tint
    v.selection.stroke = Stroke::new(1.0, p.text);

    let shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if v.dark_mode { 120 } else { 30 }),
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;

    let w = &mut v.widgets;
    for (state, fill, stroke) in [
        (&mut w.noninteractive, p.bg, p.border),
        (&mut w.inactive, p.surface, p.border),
        (&mut w.hovered, p.hover, p.border_strong),
        (&mut w.active, p.active, p.border_strong),
        (&mut w.open, p.active, p.border_strong),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, stroke);
        state.fg_stroke = Stroke::new(1.0, p.text);
        state.corner_radius = CornerRadius::same(radius::MD);
        state.expansion = 0.0;
    }
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text_muted);
}
