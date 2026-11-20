//! The Sound/Video page written with `hp2-ui`. The stock page's controls stay alive but hidden:
//! they remain the source of truth and keep doing the real work (every setting still flows
//! through the stock `Notify` handlers and the `stock_*` modules); this module is the view and the
//! input handling on top of them.
use std::{cell::RefCell, collections::HashMap, ffi::c_void, rc::Rc};

use hp2_abi::engine::generated::{UFunction, UObject};
use hp2_core::{core_access::{CoreAccess, RuntimeError, RuntimeResult}, name_owner::FindName};
use hp2_ui::{
    Align, BlendMode, Color, Edges, Fit, Font, Justify, Key, PointerButton, PopupMetrics, Signal, Size, Skin, SkinPart, SliderMetrics, Store, Style, Stylesheet, TextAlign, TextureRef, Ui,
    button, checkbox, col, image, label, row, select, slider,
};

use crate::{
    stock_graphics_menu::{boolean, children, field, live, object, prop, read_boolean, set_text, text, Weak},
    ui_console_views::declared,
    ui_host::{class_name_of, find_class, find_texture, name_of, read_string, HostMetrics, Surface},
    ui_vm::{invoke, invoke_resolved, V},
};

const PAGE_CLASS: &str = "FESoundVideoPage";
/// Hogwarts antique gold & warm parchment palette matching the main menu book frames.
const GOLD_TITLE: Color = Color(252, 224, 144, 255);
const GOLD_SECTION: Color = Color(244, 212, 128, 255);
const PARCHMENT: Color = Color(238, 228, 198, 255);
const IVORY: Color = Color(255, 248, 226, 255);
const HINT_PARCHMENT: Color = Color(188, 176, 148, 255);
const VELVET_HEADER: Color = Color(245, 245, 255, 255);
const VELVET_CARD: Color = Color(186, 192, 220, 255);
const RULE_GOLD: Color = Color(235, 220, 180, 255);
const FALLBACK_VELVET: Color = Color(26, 33, 68, 255);
const FALLBACK_GOLD: Color = Color(196, 152, 66, 255);
const FALLBACK_GOLD_DARK: Color = Color(98, 70, 22, 255);

const TEX_BAR_LEFT: &str = "HP2_Menu.Icons.HP2TopToolbarLeft";
const TEX_BAR_MID: &str = "HP2_Menu.Icons.HP2TopToolbarMiddle";
const TEX_BAR_RIGHT: &str = "HP2_Menu.Icons.HP2TopToolbarRight";
const TEX_EYE_ICON: &str = "HP2_Menu.Icons.HP2MenuSoundOptions";
const TEX_SCROLL_ICON: &str = "HP2_Menu.Icons.HP2Options";
const TEX_GRYF_CREST: &str = "HP2_Menu.Icons.HP2GriffindorCrestSm";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctl {
    Music,
    Sound,
    Brightness,
    ObjectDetail,
    Scale,
    Resolution,
    Textures,
    Display,
    ShowFps,
    Advanced(usize),
    Quality(usize),
}

enum Command {
    Slider(Ctl, f32),
    PickIndex(Ctl, usize),
    PickText(Ctl, String),
    Check(Ctl, bool),
    /// Quality feature `index` (`render_device_owner::QUALITY_KEYS`) on or off.
    Quality(usize, bool),
    /// "More options" entry `index` set to a value (0/1 for toggles, choice index, or a number).
    Extra(usize, f32),
}

/// How a "More options" entry is edited.
pub(crate) enum ExtraKind {
    Toggle,
    Range { min: f32, max: f32, step: f32, unit: &'static str },
    Choice(Vec<String>),
}

/// A setting on the "More options" panel. `get` reads the saved value; `set` saves and applies it.
pub(crate) struct ExtraOption {
    pub caption: &'static str,
    pub kind: ExtraKind,
    pub get: fn(CoreAccess<'_>) -> f32,
    pub set: unsafe fn(CoreAccess<'_>, f32) -> RuntimeResult<()>,
}

/// Every "More options" entry, in display order (two columns, first half on the left).
fn extra_options() -> Vec<ExtraOption> {
    use crate::{audio_owner as audio, display_options as display, input_options as input, ui_accessibility as access};
    vec![
        ExtraOption {
            caption: "HD Textures",
            kind: ExtraKind::Toggle,
            get: |a| f32::from(u8::from(crate::render_device_owner::hd_textures_enabled(a))),
            set: |a, v| unsafe { crate::render_device_owner::set_hd_textures_enabled(a, v >= 0.5) },
        },
        ExtraOption {
            caption: "Draw Distance",
            kind: ExtraKind::Choice(["Original", "2x", "4x"].into_iter().map(str::to_owned).collect()),
            get: |a| f32::from(crate::render_device_owner::draw_distance(a)),
            set: |a, v| crate::render_device_owner::set_draw_distance(a, v as u8),
        },
        ExtraOption {
            caption: "Voice Volume",
            kind: ExtraKind::Range { min: 0., max: 100., step: 1., unit: "" },
            get: |a| (audio::voice_volume(a) / 2.55).round(),
            set: |a, v| audio::set_voice_volume(a, v * 2.55),
        },
        ExtraOption {
            caption: "Sound in background",
            kind: ExtraKind::Toggle,
            get: |a| f32::from(u8::from(audio::play_in_background(a))),
            set: |a, v| audio::set_play_in_background(a, v >= 0.5),
        },
        ExtraOption {
            caption: "Mouse left/right",
            kind: ExtraKind::Range { min: 10., max: 300., step: 5., unit: "%" },
            get: |a| (input::mouse_sensitivity_x(a) * 100.).round(),
            set: |a, v| input::set_mouse_sensitivity_x(a, v / 100.),
        },
        ExtraOption {
            caption: "Mouse up/down",
            kind: ExtraKind::Range { min: 10., max: 300., step: 5., unit: "%" },
            get: |a| (input::mouse_sensitivity_y(a) * 100.).round(),
            set: |a, v| input::set_mouse_sensitivity_y(a, v / 100.),
        },
        ExtraOption {
            caption: "Raw mouse",
            kind: ExtraKind::Toggle,
            get: |a| f32::from(u8::from(input::raw_mouse(a))),
            set: |a, v| input::set_raw_mouse(a, v >= 0.5),
        },
        ExtraOption {
            caption: "Gamepad",
            kind: ExtraKind::Toggle,
            get: |a| f32::from(u8::from(input::use_game_controller(a))),
            set: |a, v| input::set_use_game_controller(a, v >= 0.5),
        },
        ExtraOption {
            caption: "Gamepad look",
            kind: ExtraKind::Range { min: 10., max: 300., step: 5., unit: "%" },
            get: |a| (input::gamepad_look_sensitivity(a) * 100.).round(),
            set: |a, v| input::set_gamepad_look_sensitivity(a, v / 100.),
        },
        ExtraOption {
            caption: "HUD size",
            kind: ExtraKind::Range { min: display::HUD_SCALE_MIN_PERCENT, max: display::HUD_SCALE_MAX_PERCENT, step: 5., unit: "%" },
            get: display::hud_scale_percent,
            set: |a, v| display::set_hud_scale_percent(a, v),
        },
        ExtraOption {
            caption: "Fullscreen style",
            kind: ExtraKind::Choice(display::FullscreenMode::ALL.iter().map(|m| m.caption().to_owned()).collect()),
            get: |a| display::fullscreen_mode(a).index() as f32,
            // Spaces changes apply at the next start (the returned flag); the choice is saved either way.
            set: |a, v| display::set_fullscreen_mode(a, display::FullscreenMode::from_index(v as usize)).map(|_| ()),
        },
        ExtraOption {
            caption: "Monitor",
            kind: ExtraKind::Choice((1..=display::display_count().max(1)).map(|n| format!("Display {n}")).collect()),
            get: |a| display::display_index(a) as f32,
            set: |a, v| display::set_display_index(a, v as i32),
        },
        ExtraOption {
            caption: "Subtitle size",
            kind: ExtraKind::Range { min: 100., max: 200., step: 10., unit: "%" },
            get: access::subtitle_scale,
            set: |a, v| access::set_subtitle_scale(a, v),
        },
        ExtraOption {
            caption: "Always subtitles",
            kind: ExtraKind::Toggle,
            get: |a| f32::from(u8::from(access::always_subtitles(a))),
            set: |a, v| access::set_always_subtitles(a, v >= 0.5),
        },
        ExtraOption {
            caption: "Colour-blind HUD",
            kind: ExtraKind::Toggle,
            get: |a| f32::from(u8::from(access::color_blind_hud(a))),
            set: |a, v| access::set_color_blind_hud(a, v >= 0.5),
        },
    ]
}

// ---- language ----------------------------------------------------------------------------------

/// Whether `[Engine.Engine] Language` is Polish (`pol`, any case).
fn polish_language(a: CoreAccess<'_>) -> bool {
    a.language_text().is_ok_and(|units| {
        units.iter().take_while(|&&u| u != 0).filter_map(|&u| char::from_u32(u)).collect::<String>().eq_ignore_ascii_case("pol")
    })
}

/// The Polish wording of every caption and value this view draws, in the register of the
/// Polish *Harry Potter i Komnata Tajemnic* menus. Each fits its column: captions <= 18
/// characters (141-unit caption column), combo values <= 12 (the 114-unit value field of a
/// 133-unit box), checkbox captions <= 14 (125-unit cells). Words that read the same in Polish
/// ("HDR", "Gamepad", "Monitor", "4x", "60 FPS") have no entry.
fn polish(english: &str) -> Option<&'static str> {
    Some(match english {
        // Pages, panels and navigation.
        "Sound & Video" => "Dźwięk i obraz",
        "More Options" => "Więcej opcji",
        "More Options >" => "Więcej opcji >",
        "< Sound & Video" => "< Dźwięk i obraz",
        "Video" => "Obraz",
        "Audio & Display" => "Dźwięk i ekran",
        "Advanced Graphics" => "Zaawansowana grafika",
        "World & Controls" => "Świat i sterowanie",
        "Display & Accessibility" => "Ekran i dostępność",
        "Hogwarts Graphics & Accessibility" => "Grafika i dostępność",
        "HD Textures and enhancements apply immediately as you toggle them." => "Tekstury HD i efekty działają od razu po przełączeniu.",
        // Sound & Video captions.
        "Logical res." => "Rozdzielczość",
        "Textures" => "Szczegóły tekstur",
        "Object Detail" => "Szczegóły obiektów",
        "Brightness" => "Jasność",
        "3D Scale" => "Skala 3D",
        "Music" => "Muzyka",
        "Sound" => "Dźwięk",
        "Display" => "Tryb ekranu",
        "Show FPS" => "Pokaż FPS",
        "Low" => "Niska",
        "High" => "Wysoka",
        // `stock_graphics_menu` rows and `QUALITY_CAPTIONS`.
        "Frame limit" => "Limit klatek",
        "VSync" => "Synchr. pionowa",
        "Anti-alias" => "Antyaliasing",
        "Anisotropy" => "Filtr anizotropowy",
        "UI scale" => "Skala interfejsu",
        "Vertical FOV" => "Pionowy FOV",
        "Flashes" => "Błyski",
        "Mipmaps" => "Mipmapy",
        "Edge AA" => "AA krawędzi",
        "Light filter" => "Filtr światła",
        "Bloom" => "Poświata",
        "Soft shadows" => "Miękkie cienie",
        "Rich color" => "Żywe kolory",
        "Sharpen" => "Wyostrzanie",
        "Smooth edges" => "Wygładzanie",
        "Full detail" => "Pełne detale",
        "Fog" => "Mgła",
        // More Options captions.
        "HD Textures" => "Tekstury HD",
        "Draw Distance" => "Zasięg rysowania",
        "Voice Volume" => "Głośność mowy",
        "Sound in background" => "Dźwięk w tle",
        "Mouse left/right" => "Mysz poziomo",
        "Mouse up/down" => "Mysz pionowo",
        "Raw mouse" => "Surowa mysz",
        "Gamepad look" => "Czułość pada",
        "HUD size" => "Rozmiar HUD",
        "Fullscreen style" => "Pełny ekran",
        "Subtitle size" => "Rozmiar napisów",
        "Always subtitles" => "Zawsze napisy",
        "Colour-blind HUD" => "Tryb daltonistów",
        // Values.
        "On" => "Wł.",
        "Off" => "Wył.",
        "Unlimited" => "Bez limitu",
        "Adaptive" => "Adaptacyjna",
        "Original" => "Oryginalny",
        "Windowed" => "Okno",
        "Fullscreen (exclusive)" => "Pełny ekran",
        "Borderless Fullscreen" => "Bez ramki",
        "Exclusive" => "Wyłączny",
        "Borderless" => "Bez ramki",
        _ => return None,
    })
}

/// How a caption or value reads on screen. The canonical English strings stay the internal
/// values (stock combo text, `Command::PickText`, config); only the drawn text changes. The two
/// long display-mode names are shortened in English too so they fit their combo box.
fn shown(polish_ui: bool, english: &str) -> String {
    if polish_ui {
        if let Some(text) = polish(english) {
            return text.to_owned();
        }
        if let Some(n) = english.strip_prefix("Display ") {
            return format!("Ekran {n}");
        }
    }
    match english {
        "Fullscreen (exclusive)" => "Fullscreen",
        "Borderless Fullscreen" => "Borderless",
        other => other,
    }
    .to_owned()
}

// ---- state -------------------------------------------------------------------------------------

struct AdvancedRow {
    caption: &'static str,
    options: Vec<String>,
    current: Signal<String>,
}

/// Which `HP2_Menu` art is resident, texture by texture, so one missing piece only drops itself:
/// the velvet toolbar needs its three slices, each icon stands alone.
#[derive(Clone, Copy, Default, PartialEq)]
struct MenuArt {
    toolbar: bool,
    eye: bool,
    scroll: bool,
    crest: bool,
}

/// The view's state: signals mirrored from the stock controls plus the pending commands.
struct Data {
    music: Signal<f32>,
    sound: Signal<f32>,
    brightness: Signal<f32>,
    object_detail: Signal<f32>,
    scale: Signal<f32>,
    resolution: Signal<String>,
    resolution_options: Signal<Vec<String>>,
    textures: Signal<String>,
    texture_options: Signal<Vec<String>>,
    display: Signal<String>,
    display_options: Vec<String>,
    show_fps: Signal<bool>,
    quality: [Signal<bool>; crate::render_device_owner::QUALITY_COUNT],
    advanced: Vec<AdvancedRow>,
    /// The "More options" panel is shown instead of the Video/Audio panel.
    more: Signal<bool>,
    /// The main-menu `HP2_Menu` art that is loaded (checked every paint).
    art: Signal<MenuArt>,
    /// `[Engine.Engine] Language=pol`: draw Polish captions and values.
    polish: bool,
    extras: Vec<(ExtraOption, Signal<f32>)>,
    queue: Rc<RefCell<Vec<Command>>>,
}

impl Data {
    fn new(store: &Store, polish: bool) -> Rc<Self> {
        let advanced = crate::stock_graphics_menu::advanced_rows()
            .into_iter()
            .map(|(caption, options)| AdvancedRow { caption, options: options.into_iter().map(str::to_owned).collect(), current: store.signal(String::new()) })
            .collect();
        Rc::new(Self {
            music: store.signal(0.),
            sound: store.signal(0.),
            brightness: store.signal(0.),
            object_detail: store.signal(0.),
            scale: store.signal(0.),
            resolution: store.signal(String::new()),
            resolution_options: store.signal(Vec::new()),
            textures: store.signal(String::new()),
            texture_options: store.signal(Vec::new()),
            display: store.signal(String::new()),
            display_options: display_choices(),
            show_fps: store.signal(false),
            quality: std::array::from_fn(|_| store.signal(false)),
            advanced,
            more: store.signal(false),
            art: store.signal(MenuArt::default()),
            polish,
            extras: extra_options().into_iter().map(|o| (o, store.signal(0.))).collect(),
            queue: Rc::new(RefCell::new(Vec::new())),
        })
    }

    /// The on-screen wording of `english` (see [`shown`]).
    fn tr(&self, english: &str) -> String {
        shown(self.polish, english)
    }
}

fn stylesheet() -> Stylesheet {
    Stylesheet::new()
        .with("page-title", Style { color: GOLD_TITLE, font: Font::Slot(4), ..Style::default() })
        .with("header", Style { color: GOLD_SECTION, font: Font::Slot(5), ..Style::default() })
        .with("caption", Style { color: PARCHMENT, font: Font::Bold, ..Style::default() })
        .with("hint", Style { color: HINT_PARCHMENT, font: Font::Normal, ..Style::default() })
        .with("nav-btn", Style { color: IVORY, font: Font::Bold, ..Style::default() })
        .with("footer-title", Style { color: GOLD_SECTION, font: Font::Bold, ..Style::default() })
        .with("footer-note", Style { color: HINT_PARCHMENT, font: Font::Normal, ..Style::default() })
}

fn skin() -> Skin {
    const ICONS: &str = "HGame.Icons.";
    let named = |n: &str| format!("{ICONS}{n}");
    Skin::new()
        .with_region(SkinPart::ButtonIdle, &named("FEComboIdleTexture"), (0., 0., 180., 24.))
        .with_region(SkinPart::ButtonHover, &named("FEComboOverTexture"), (0., 0., 180., 24.))
        .with_region(SkinPart::ButtonDown, &named("FEOverOptionTexture"), (0., 0., 180., 24.))
        .with_region(SkinPart::BoxIdle, &named("FEComboIdleTexture"), (0., 0., 180., 24.))
        .with_region(SkinPart::BoxHover, &named("FEComboOverTexture"), (0., 0., 180., 24.))
        .with_native(SkinPart::SliderTrack, &named("FESliderTexture"))
        .with_native(SkinPart::SliderTrackHover, &named("FEOverSliderTexture"))
        .with_native(SkinPart::SliderThumb, &named("FESliderKnobTexture"))
        .with_native(SkinPart::CheckOff, &named("FEOptionTickUncheckedTex"))
        .with_native(SkinPart::CheckOffHover, &named("FEOptionTickUncheckedOverTex"))
        .with_native(SkinPart::CheckOn, &named("FEOptionTickCheckedTex"))
        .with_native(SkinPart::CheckOnHover, &named("FEOptionTickCheckedOverTex"))
        .with_region(SkinPart::ListBackground, &named("FEComboListLarge"), (0., 0., 180., 86.))
        .with(SkinPart::ListRowSelected, &named("FEComboListBox"))
        .with_slider(SliderMetrics { track_dx: -1., thumb_start: 9., travel: 99., thumb_width: 10. })
        .with_check_size(12.)
        .with_box(1., 11.)
        .with_popup(PopupMetrics { row_height: 20.5, top: 3., bottom: 4., inset: 2., text_dx: 11. })
}

// Optional extensions exist only in builds with their features.
#[cfg(feature = "sdl")]
fn display_choices() -> Vec<String> { crate::stock_display_control::choice_labels().into_iter().map(str::to_owned).collect() }
#[cfg(not(feature = "sdl"))]
fn display_choices() -> Vec<String> { Vec::new() }
#[cfg(feature = "sdl")]
fn display_combo(page: *mut UObject) -> Option<*mut UObject> { crate::stock_display_control::combo_of(page) }
#[cfg(not(feature = "sdl"))]
fn display_combo(_: *mut UObject) -> Option<*mut UObject> { None }
#[cfg(feature = "renderer-wgpu")]
fn scale_slider(page: *mut UObject) -> Option<*mut UObject> { crate::stock_render_scale_control::slider_of(page) }
#[cfg(not(feature = "renderer-wgpu"))]
fn scale_slider(_: *mut UObject) -> Option<*mut UObject> { None }

// ---- the view ----------------------------------------------------------------------------------
//
// One grid on the 640x480 page (page units). The stock Back button stays at (582, 422, 48, 48),
// and the engine's FPS counter sits in the screen's top-right corner, so the header and footer
// strips stop at x = 574 and every card ends at y <= 412:
//
//   header   x 12..574  y  10..48    (gilded velvet bar: icon, title, panel switch)
//   cards    x 12..316 / 324..628    y  54..244   (Sound & Video)  |  y 54..412 (More Options)
//   advanced x 12..628  y 250..412
//   footer   x 12..574  y 418..472   (Back button: x 582..630, y 422..470)
//
// Inside a card: 4-unit gilded rail, title at y 5..22, gold rule block 22..30, rows from y 30.
// Video rows 25 + 25 + 38 + 38 + 25 = 151 -> 30..181 (inner edge 186); Audio rows
// 38 + 38 + 8 + 25 + 24 + 4 gaps of 4 = 149 -> 30..179; Advanced 5 x 25 = 125 -> 30..155
// (inner edge 158); More Options 8 x 39 = 312 -> 30..342 (inner edge 354). A slider row is the
// 24-unit slider, a 1-unit gap and a 12-unit hint line (37 of 38), so hints never reach the
// next control.

const MARGIN: f32 = 12.;
/// Width of the header and footer strips (x 12..574, clear of the Back button and FPS counter).
const STRIP_W: f32 = 562.;
const HEADER_Y: f32 = 10.;
const HEADER_H: f32 = 38.;
const CARD_Y: f32 = 54.;
const CARD_W: f32 = 304.;
const RIGHT_CARD_X: f32 = 324.;
const WIDE_W: f32 = 616.;
const TOP_CARD_H: f32 = 190.;
const ADVANCED_Y: f32 = 250.;
const ADVANCED_H: f32 = 162.;
const TALL_CARD_H: f32 = 358.;
const FOOTER_Y: f32 = 418.;
const FOOTER_H: f32 = 54.;
const RAIL: f32 = 4.;
/// Inner horizontal padding of a card; leaves `COLUMN_W` for a 304-unit card.
const CARD_PAD: f32 = 12.;
const COLUMN_W: f32 = 280.;
/// Every combo box and slider shares the slider art's width so controls lock onto one grid.
const ART_W: f32 = 133.;
/// Caption column left of a control, with a 6-unit gutter.
const CAPTION_W: f32 = COLUMN_W - ART_W - 6.;
const WIDGET_H: f32 = 24.;
const HINT_H: f32 = 12.;
const COMBO_ROW_H: f32 = 25.;
const SLIDER_ROW_H: f32 = WIDGET_H + 1. + HINT_H + 1.;
const EXTRA_ROW_H: f32 = 39.;
const CHECK_ROW_H: f32 = 20.;
const CREST_W: f32 = 26.;
/// Footer content width (inside the 12-unit padding) and the 4-column checkbox grid beside the crest.
const FOOTER_INNER_W: f32 = STRIP_W - 2. * CARD_PAD;
const EFFECT_W: f32 = (FOOTER_INNER_W - CREST_W - 10.) / 4.;

/// Captions for `render_device_owner::QUALITY_KEYS`, in that order. The first three are
/// On/Off rows under Advanced; the rest are the footer checkboxes.
const QUALITY_CAPTIONS: [&str; crate::render_device_owner::QUALITY_COUNT] =
    ["Mipmaps", "Edge AA", "Light filter", "Bloom", "Soft shadows", "Rich color", "Sharpen", "Smooth edges", "Full detail", "HDR", "Fog"];
const QUALITY_ROWS: usize = 3;

fn px(v: f32) -> Size {
    Size::Px(v)
}

/// 9-slice royal-indigo velvet panel framed with the main menu's gilded antique-gold bevel
/// (`HP2TopToolbar*`: rails are rows 0..11 and 86..97, sides columns 0..11 and 117..128).
fn gilded_panel(w: f32, h: f32, border: f32, tint: Color, has_art: bool) -> hp2_ui::Node {
    let panel = col().at(0., 0.).width(px(w)).height(px(h));
    if !has_art {
        return panel
            .background(FALLBACK_GOLD_DARK)
            .child(col().at(1., 1.).width(px(w - 2.)).height(px(h - 2.)).background(FALLBACK_GOLD))
            .child(col().at(border, border).width(px((w - 2. * border).max(0.))).height(px((h - 2. * border).max(0.))).background(FALLBACK_VELVET));
    }
    let b = border;
    let iw = (w - 2. * b).max(0.);
    let ih = (h - 2. * b).max(0.);
    let piece = |tex: &str, region: (f32, f32, f32, f32), at: (f32, f32), size: (f32, f32), color: Color| {
        image(TextureRef(tex.to_owned()), Fit::Region(region.0, region.1, region.2, region.3))
            .at(at.0, at.1)
            .width(px(size.0))
            .height(px(size.1))
            .color(color)
            .blend(BlendMode::Masked)
    };
    panel
        // Velvet interior, then the four rails and four corners.
        .child(piece(TEX_BAR_MID, (0., 11., 256., 75.), (b, b), (iw, ih), tint))
        .child(piece(TEX_BAR_MID, (0., 0., 256., 11.), (b, 0.), (iw, b), Color::WHITE))
        .child(piece(TEX_BAR_MID, (0., 86., 256., 11.), (b, h - b), (iw, b), Color::WHITE))
        .child(piece(TEX_BAR_LEFT, (0., 11., 11., 75.), (0., b), (b, ih), Color::WHITE))
        .child(piece(TEX_BAR_RIGHT, (117., 11., 11., 75.), (w - b, b), (b, ih), Color::WHITE))
        .child(piece(TEX_BAR_LEFT, (0., 0., 11., 11.), (0., 0.), (b, b), Color::WHITE))
        .child(piece(TEX_BAR_LEFT, (0., 86., 11., 11.), (0., h - b), (b, b), Color::WHITE))
        .child(piece(TEX_BAR_RIGHT, (117., 0., 11., 11.), (w - b, 0.), (b, b), Color::WHITE))
        .child(piece(TEX_BAR_RIGHT, (117., 86., 11., 11.), (w - b, h - b), (b, b), Color::WHITE))
}

/// Slender burnished-gold rule (the toolbar's own gilded rail when the art is loaded).
fn gold_rule(w: f32, has_art: bool) -> hp2_ui::Node {
    if has_art {
        image(TextureRef(TEX_BAR_MID.to_owned()), Fit::Region(0., 0., 256., 11.))
            .width(px(w))
            .height(px(2.))
            .color(RULE_GOLD)
            .blend(BlendMode::Masked)
    } else {
        col().width(px(w)).height(px(2.)).background(FALLBACK_GOLD)
    }
}

/// A 3-unit gap, the rule, a 3-unit gap: 8 units.
fn rule_block(w: f32, has_art: bool) -> hp2_ui::Node {
    col().padding(Edges { top: 3., bottom: 3., ..Edges::default() }).child(gold_rule(w, has_art))
}

/// A gilded card at `(x, y)`: title, rule, then `body` from y = 30.
fn card(d: &Data, (x, y, w, h): (f32, f32, f32, f32), title: &str, body: hp2_ui::Node) -> hp2_ui::Node {
    let toolbar = d.art.get().toolbar;
    let inner = w - 2. * CARD_PAD;
    col()
        .at(x, y)
        .width(px(w))
        .height(px(h))
        .child(gilded_panel(w, h, RAIL, VELVET_CARD, toolbar))
        .child(
            col()
                .at(CARD_PAD, 5.)
                .width(px(inner))
                .child(label(&d.tr(title)).class("header").width(px(inner)).height(px(17.)))
                .child(rule_block(inner, toolbar))
                .child(body),
        )
}

/// A gilded strip (header or footer) at `(x, y)` holding `content` inside its 12-unit padding.
fn strip(d: &Data, y: f32, h: f32, tint: Color, content: hp2_ui::Node) -> hp2_ui::Node {
    col()
        .at(MARGIN, y)
        .width(px(STRIP_W))
        .height(px(h))
        .child(gilded_panel(STRIP_W, h, 5., tint, d.art.get().toolbar))
        .child(content.at(CARD_PAD, 5.).width(px(FOOTER_INNER_W)).height(px(h - 10.)))
}

/// The page's crest/icon art (`Fit::Region` of the visible part of a 64x64 icon).
fn icon(texture: &str, region: (f32, f32, f32, f32), size: (f32, f32)) -> hp2_ui::Node {
    image(TextureRef(texture.to_owned()), Fit::Region(region.0, region.1, region.2, region.3))
        .width(px(size.0))
        .height(px(size.1))
        .color(Color::WHITE)
        .blend(BlendMode::Masked)
}

/// Ornate header bar matching the main menu's gilded toolbar: icon and title left, the
/// panel switch right.
fn header_bar(d: &Rc<Data>, title: &str, art_icon: Option<hp2_ui::Node>) -> hp2_ui::Node {
    let left = row()
        .align(Align::Center)
        .gap(8.)
        .children(art_icon)
        .child(label(&d.tr(title)).class("page-title").height(px(28.)));
    strip(
        d,
        HEADER_Y,
        HEADER_H,
        VELVET_HEADER,
        row().align(Align::Center).justify(Justify::SpaceBetween).child(left).child(panel_switch(d)),
    )
}

fn key_of(ctl: Ctl) -> String {
    format!("{ctl:?}")
}

fn slider_node(d: &Rc<Data>, ctl: Ctl, min: f32, max: f32, signal: &Signal<f32>) -> hp2_ui::Node {
    let (queue, signal_for_change) = (d.queue.clone(), signal.clone());
    slider(min, max, 1., signal.get(), move |v| {
        signal_for_change.set(v);
        queue.borrow_mut().push(Command::Slider(ctl, v));
    })
    .key(&key_of(ctl))
}

/// A combo over `options` (canonical strings). The box shows translated text; every pick
/// still sends the canonical string or index, so the stock handlers see exactly what they expect.
fn select_node(d: &Rc<Data>, ctl: Ctl, current: &Signal<String>, options: &[String], by_index: bool) -> hp2_ui::Node {
    let now = current.get();
    let mut canonical: Vec<String> = options.to_vec();
    // A value the stock page reports that is not one of the choices (for example "Custom") is shown, not hidden.
    let selected = canonical.iter().position(|o| *o == now).unwrap_or_else(|| {
        canonical.push(now.clone());
        canonical.len() - 1
    });
    let labels: Vec<String> = canonical.iter().map(|o| d.tr(o)).collect();
    let (queue, signal, choices) = (d.queue.clone(), current.clone(), options.to_vec());
    select(&labels, selected, move |i| {
        let Some(choice) = choices.get(i) else { return };
        signal.set(choice.clone());
        queue.borrow_mut().push(if by_index { Command::PickIndex(ctl, i) } else { Command::PickText(ctl, choice.clone()) });
    })
    .key(&key_of(ctl))
    .width(px(ART_W))
    .color(IVORY)
    .font(Font::Bold)
}

/// A caption in the left column; its box is as tall as a control so the text centres on it.
fn caption(text: &str) -> hp2_ui::Node {
    label(text).class("caption").width(px(CAPTION_W)).height(px(WIDGET_H))
}

/// Caption left, combo box right, both on the row's centre line.
fn combo_row(text: &str, select_node: hp2_ui::Node, height: f32) -> hp2_ui::Node {
    row()
        .width(px(COLUMN_W))
        .height(px(height))
        .align(Align::Center)
        .justify(Justify::SpaceBetween)
        .child(caption(text))
        .child(select_node)
}

/// A control column: the 24-unit control with a 12-unit hint line under it.
fn with_hints(control: hp2_ui::Node, left: hp2_ui::Node, right: hp2_ui::Node) -> hp2_ui::Node {
    col()
        .width(px(ART_W))
        .gap(1.)
        .child(control)
        .child(row().width(px(ART_W)).height(px(HINT_H)).child(left).child(right))
}

fn hint(text: &str, width: f32, align: TextAlign) -> hp2_ui::Node {
    label(text).class("hint").width(px(width)).height(px(HINT_H)).text_align(align)
}

/// Caption left (centred on the slider), slider right with its Low/High hints under the ends.
fn slider_row(d: &Data, text: &str, slider_node: hp2_ui::Node) -> hp2_ui::Node {
    let half = ART_W / 2.;
    row()
        .width(px(COLUMN_W))
        .height(px(SLIDER_ROW_H))
        .justify(Justify::SpaceBetween)
        .child(caption(text))
        .child(with_hints(slider_node, hint(&d.tr("Low"), half, TextAlign::Left), hint(&d.tr("High"), half, TextAlign::Right)))
}

/// The gilded bronze button that flips between the Sound & Video and More Options panels.
fn panel_switch(d: &Rc<Data>) -> hp2_ui::Node {
    let more = d.more.clone();
    let caption = d.tr(if more.get() { "< Sound & Video" } else { "More Options >" });
    button(&caption)
        .key("PanelSwitch")
        .class("nav-btn")
        .width(px(148.))
        .height(px(WIDGET_H))
        .on_click(move || more.set(!more.get()))
}

fn extras_view(d: &Rc<Data>) -> hp2_ui::Node {
    let art = d.art.get();
    let mut rows: Vec<hp2_ui::Node> = d.extras.iter().enumerate().map(|(i, (option, value))| {
        let (queue, signal) = (d.queue.clone(), value.clone());
        let key = format!("Extra{i}");
        let control = match &option.kind {
            ExtraKind::Toggle => {
                let labels = [d.tr("On"), d.tr("Off")];
                select(&labels, usize::from(value.get() < 0.5), move |choice| {
                    let v = if choice == 0 { 1. } else { 0. };
                    signal.set(v);
                    queue.borrow_mut().push(Command::Extra(i, v));
                })
                .key(&key)
                .color(IVORY)
                .font(Font::Bold)
                .width(px(ART_W))
            }
            ExtraKind::Choice(options) => {
                let labels: Vec<String> = options.iter().map(|o| d.tr(o)).collect();
                let selected = (value.get().max(0.) as usize).min(options.len().saturating_sub(1));
                select(&labels, selected, move |choice| {
                    signal.set(choice as f32);
                    queue.borrow_mut().push(Command::Extra(i, choice as f32));
                })
                .key(&key)
                .color(IVORY)
                .font(Font::Bold)
                .width(px(ART_W))
            }
            // The value sits centred under the slider, so long captions never collide with it.
            ExtraKind::Range { min, max, step, unit } => with_hints(
                slider(*min, *max, *step, value.get(), move |v| {
                    signal.set(v);
                    queue.borrow_mut().push(Command::Extra(i, v));
                })
                .key(&key),
                hint(&format!("{}{unit}", value.get().round() as i32), ART_W, TextAlign::Center),
                row(),
            ),
        };
        // Every control starts 1 unit below the row top, so captions and controls share one line.
        row()
            .width(px(COLUMN_W))
            .height(px(EXTRA_ROW_H))
            .padding(Edges { top: 1., ..Edges::default() })
            .justify(Justify::SpaceBetween)
            .child(caption(&d.tr(option.caption)))
            .child(control)
    }).collect();
    let right = rows.split_off(rows.len().div_ceil(2));

    let column = |rows: Vec<hp2_ui::Node>| col().width(px(COLUMN_W)).children(rows);
    let left_card = card(d, (MARGIN, CARD_Y, CARD_W, TALL_CARD_H), "World & Controls", column(rows));
    let right_card = card(d, (RIGHT_CARD_X, CARD_Y, CARD_W, TALL_CARD_H), "Display & Accessibility", column(right));

    let note = col()
        .gap(2.)
        .child(label(&d.tr("Hogwarts Graphics & Accessibility")).class("footer-title").height(px(17.)))
        .child(label(&d.tr("HD Textures and enhancements apply immediately as you toggle them.")).class("footer-note").height(px(16.)));
    let footer_content = row()
        .align(Align::Center)
        .gap(10.)
        .child(crest(art))
        .child(note);

    col()
        .width(px(640.))
        .height(px(480.))
        .child(header_bar(d, "More Options", art.scroll.then(|| icon(TEX_SCROLL_ICON, (2., 2., 43., 44.), (28., 29.)))))
        .child(left_card)
        .child(right_card)
        .child(strip(d, FOOTER_Y, FOOTER_H, VELVET_HEADER, footer_content))
}

/// The small Gryffindor crest of the footers, or the same space left empty without its art.
fn crest(art: MenuArt) -> hp2_ui::Node {
    if art.crest {
        icon(TEX_GRYF_CREST, (10., 8., 44., 50.), (CREST_W, 30.))
    } else {
        col().width(px(CREST_W)).height(px(30.))
    }
}

fn view(d: &Rc<Data>) -> hp2_ui::Node {
    if d.more.get() {
        return extras_view(d);
    }
    let art = d.art.get();
    let resolution = select_node(d, Ctl::Resolution, &d.resolution, &d.resolution_options.get(), true);
    let textures = select_node(d, Ctl::Textures, &d.textures, &d.texture_options.get(), true);
    let scale_row = row()
        .width(px(COLUMN_W))
        .height(px(COMBO_ROW_H))
        .align(Align::Center)
        .justify(Justify::SpaceBetween)
        .child(caption(&format!("{}: {}%", d.tr("3D Scale"), scale_percent(d.scale.get()))))
        .child(col().width(px(ART_W)).child(slider_node(d, Ctl::Scale, 0., 15., &d.scale)));
    let video_body = col()
        .child(combo_row(&d.tr("Logical res."), resolution, COMBO_ROW_H))
        .child(combo_row(&d.tr("Textures"), textures, COMBO_ROW_H))
        .child(slider_row(d, &d.tr("Object Detail"), slider_node(d, Ctl::ObjectDetail, 0., 4., &d.object_detail)))
        .child(slider_row(d, &d.tr("Brightness"), slider_node(d, Ctl::Brightness, 2., 10., &d.brightness)))
        .child(scale_row);
    let video_card = card(d, (MARGIN, CARD_Y, CARD_W, TOP_CARD_H), "Video", video_body);

    let display = select_node(d, Ctl::Display, &d.display, &d.display_options, false);
    let fps = {
        let (queue, signal) = (d.queue.clone(), d.show_fps.clone());
        checkbox(&d.tr("Show FPS"), d.show_fps.get(), move |on| {
            signal.set(on);
            queue.borrow_mut().push(Command::Check(Ctl::ShowFps, on));
        })
        .key(&key_of(Ctl::ShowFps))
        .class("caption")
        .height(px(WIDGET_H))
    };
    let audio_body = col()
        .gap(4.)
        .child(slider_row(d, &format!("{}: {}%", d.tr("Music"), d.music.get() as i32), slider_node(d, Ctl::Music, 0., 100., &d.music)))
        .child(slider_row(d, &format!("{}: {}%", d.tr("Sound"), d.sound.get() as i32), slider_node(d, Ctl::Sound, 0., 100., &d.sound)))
        .child(rule_block(COLUMN_W, art.toolbar))
        .child(combo_row(&d.tr("Display"), display, COMBO_ROW_H))
        // The tick box lines up with the combo box above it.
        .child(row().width(px(COLUMN_W)).height(px(WIDGET_H)).justify(Justify::End).child(col().width(px(ART_W)).child(fps)));
    let audio_card = card(d, (RIGHT_CARD_X, CARD_Y, CARD_W, TOP_CARD_H), "Audio & Display", audio_body);

    // Advanced: the GPU and display rows, then the first image-quality features; five per column.
    let mut rows: Vec<hp2_ui::Node> = d.advanced.iter().enumerate().map(|(i, r)| combo_row(&d.tr(r.caption), select_node(d, Ctl::Advanced(i), &r.current, &r.options, false), COMBO_ROW_H)).collect();
    for (i, caption) in QUALITY_CAPTIONS.iter().enumerate().take(QUALITY_ROWS) {
        let labels = [d.tr("On"), d.tr("Off")];
        let (queue, signal) = (d.queue.clone(), d.quality[i].clone());
        let selected = usize::from(!signal.get());
        let node = select(&labels, selected, move |choice| {
            signal.set(choice == 0);
            queue.borrow_mut().push(Command::Quality(i, choice == 0));
        })
        .key(&key_of(Ctl::Quality(i)))
        .width(px(ART_W))
        .color(IVORY)
        .font(Font::Bold);
        rows.push(combo_row(&d.tr(caption), node, COMBO_ROW_H));
    }
    let right: Vec<_> = rows.split_off(rows.len().div_ceil(2));
    let column = |rows: Vec<hp2_ui::Node>| col().width(px(COLUMN_W)).children(rows);
    let advanced_body = row().gap(WIDE_W - 2. * CARD_PAD - 2. * COLUMN_W).child(column(rows)).child(column(right));
    let advanced_card = card(d, (MARGIN, ADVANCED_Y, WIDE_W, ADVANCED_H), "Advanced Graphics", advanced_body);

    // Footer: the crest, then the remaining image-quality features as a 4 x 2 checkbox grid.
    let effects: Vec<hp2_ui::Node> = (QUALITY_ROWS..QUALITY_CAPTIONS.len()).map(|i| {
        let (queue, signal) = (d.queue.clone(), d.quality[i].clone());
        checkbox(&d.tr(QUALITY_CAPTIONS[i]), signal.get(), move |on| {
            signal.set(on);
            queue.borrow_mut().push(Command::Quality(i, on));
        })
        .key(&key_of(Ctl::Quality(i)))
        .class("caption")
        .width(px(EFFECT_W))
        .height(px(CHECK_ROW_H))
    }).collect();
    let mut effects = effects.into_iter().peekable();
    let mut grid = col();
    while effects.peek().is_some() {
        grid = grid.child(row().children(effects.by_ref().take(4).collect::<Vec<_>>()));
    }
    let footer_content = row().align(Align::Center).gap(10.).child(crest(art)).child(grid);

    col()
        .width(px(640.))
        .height(px(480.))
        .child(header_bar(d, "Sound & Video", art.eye.then(|| icon(TEX_EYE_ICON, (1., 5., 47., 42.), (32., 28.)))))
        .child(video_card)
        .child(audio_card)
        .child(advanced_card)
        .child(strip(d, FOOTER_Y, FOOTER_H, VELVET_HEADER, footer_content))
}

/// The stock 12-step render-scale slider as a percent.
fn scale_percent(step: f32) -> i32 {
    const SCALES: [i32; 16] = [50, 55, 60, 65, 67, 70, 75, 80, 85, 90, 95, 100, 125, 150, 175, 200];
    SCALES.get(step.round().max(0.) as usize).copied().unwrap_or(100)
}

// ---- art -----------------------------------------------------------------------------------------

/// Whether the texture `path` is resident; when `load`, a missing one is loaded first with
/// `DynamicLoadObject` (the `HP2_Menu` package is not loaded by the Sound/Video page itself).
unsafe fn texture_ready(a: CoreAccess<'_>, page: *mut UObject, path: &str, load: bool) -> bool {
    if unsafe { find_texture(a, path) }.is_ok() {
        return true;
    }
    if !load {
        return false;
    }
    let loaded = (|| -> RuntimeResult<()> {
        let f = unsafe { declared(a, "Core.Object.DynamicLoadObject")? };
        let Some(class) = (unsafe { find_class(a, "Engine.Texture")? }) else { return Ok(()) };
        if f.is_null() {
            return Ok(());
        }
        let mut texture: *mut UObject = std::ptr::null_mut();
        unsafe { invoke_resolved(a, page, f, &[("ObjectName", V::S(path)), ("ObjectClass", V::Obj(class.cast()))], &[], (&raw mut texture).cast())? };
        Ok(())
    })();
    if let Err(error) = loaded {
        warn(a, &error);
    }
    unsafe { find_texture(a, path) }.is_ok()
}

/// Which menu art can be drawn now. Every texture is checked on its own.
unsafe fn menu_art(a: CoreAccess<'_>, page: *mut UObject, load: bool) -> MenuArt {
    let ready = |path: &str| unsafe { texture_ready(a, page, path, load) };
    let bar = [ready(TEX_BAR_LEFT), ready(TEX_BAR_MID), ready(TEX_BAR_RIGHT)];
    MenuArt {
        toolbar: bar.iter().all(|&loaded| loaded),
        eye: ready(TEX_EYE_ICON),
        scroll: ready(TEX_SCROLL_ICON),
        crest: ready(TEX_GRYF_CREST),
    }
}

// ---- page state -------------------------------------------------------------------------------

struct PageUi {
    page: Weak,
    ui: Ui,
    data: Rc<Data>,
    metrics: HostMetrics,
    hidden: bool,
    /// Missing menu art was already asked for once (`DynamicLoadObject`); later paints only look.
    art_requested: bool,
    /// Canvas `SizeX`/`SizeY` the text widths in `metrics` were measured at.
    measured_at: (i32, i32),
    /// Re-read stock controls and INI keys on the next paint (set after executing commands or resizing).
    needs_refresh: bool,
    /// Timestamp of the last stock-control refresh (periodic 250ms sync catches external console changes).
    last_refresh: std::time::Instant,
}

thread_local! {
    static PAGES: RefCell<Vec<PageUi>> = const { RefCell::new(Vec::new()) };
    static TEXTURES: Rc<RefCell<HashMap<String, (*mut UObject, i32)>>> = Rc::new(RefCell::new(HashMap::new()));
}

fn warn(a: CoreAccess<'_>, error: &impl std::fmt::Display) {
    if let Ok(category) = a.intern_str("Warning", FindName::Add) {
        let _ = a.emit_log(category, &format!("Rust menu: {error}").chars().map(u32::from).collect::<Vec<_>>());
    }
}

fn is_page(a: CoreAccess<'_>, receiver: *mut UObject) -> bool {
    !receiver.is_null() && live(a, receiver) && class_name_of(a, receiver) == PAGE_CLASS
}

/// The stock controls the view mirrors and drives.
struct Handles {
    music: *mut UObject,
    sound: *mut UObject,
    brightness: *mut UObject,
    object_detail: *mut UObject,
    scale: Option<*mut UObject>,
    resolution: *mut UObject,
    textures: *mut UObject,
    display: Option<*mut UObject>,
    fps: Option<*mut UObject>,
    advanced: Vec<*mut UObject>,
}

impl Handles {
    unsafe fn resolve(a: CoreAccess<'_>, page: *mut UObject) -> RuntimeResult<Self> {
        let named = |name: &str| -> RuntimeResult<*mut UObject> {
            let o = unsafe { object(a, page, name)? };
            if live(a, o) { Ok(o) } else { Err(RuntimeError::new(&format!("Sound/Video page lacks {name}"))) }
        };
        Ok(Self {
            music: named("MusicVolumeSlider")?,
            sound: named("SoundVolumeSlider")?,
            brightness: named("BrightnessSlider")?,
            object_detail: named("ObjectDetailSlider")?,
            scale: scale_slider(page),
            resolution: named("ResolutionCombo")?,
            textures: named("TextureDetailCombo")?,
            display: display_combo(page),
            fps: crate::stock_fps_menu::checkbox_of(a, page),
            advanced: crate::stock_graphics_menu::advanced_combos(page),
        })
    }
}

unsafe fn slider_value(a: CoreAccess<'_>, slider: *mut UObject) -> RuntimeResult<f32> {
    Ok(unsafe { prop(a, slider, "Value", "Core.FloatProperty", 4)?.cast::<f32>().read_unaligned() })
}

/// The choices of a stock combo (`List.Items` is a sentinel followed by the items).
unsafe fn combo_items(a: CoreAccess<'_>, combo: *mut UObject) -> RuntimeResult<Vec<String>> {
    let list = unsafe { object(a, combo, "List")? };
    let mut item = unsafe { object(a, object(a, list, "Items")?, "Next")? };
    let mut out = Vec::new();
    while live(a, item) && out.len() < 64 {
        out.push(unsafe { read_string(a, item, "Value")? });
        item = unsafe { object(a, item, "Next")? };
    }
    Ok(out)
}

/// Copies the stock controls' current state into the view's signals.
unsafe fn refresh(a: CoreAccess<'_>, data: &Data, h: &Handles) -> RuntimeResult<()> {
    unsafe {
        data.music.set(slider_value(a, h.music)?);
        data.sound.set(slider_value(a, h.sound)?);
        data.brightness.set(slider_value(a, h.brightness)?);
        data.object_detail.set(slider_value(a, h.object_detail)?);
        if let Some(scale) = h.scale {
            data.scale.set(slider_value(a, scale)?);
        }
        data.resolution_options.set(combo_items(a, h.resolution)?);
        data.resolution.set(text(a, h.resolution)?);
        data.texture_options.set(combo_items(a, h.textures)?);
        data.textures.set(text(a, h.textures)?);
        if let Some(display) = h.display {
            data.display.set(text(a, display)?);
        }
        if let Some(fps) = h.fps {
            data.show_fps.set(read_boolean(a, fps, "bChecked")?);
        }
        let quality = crate::render_device_owner::quality_settings(a);
        for (signal, on) in data.quality.iter().zip(quality) {
            signal.set(on);
        }
        for (option, signal) in &data.extras {
            signal.set((option.get)(a));
        }
        for (row, combo) in data.advanced.iter().zip(&h.advanced) {
            row.current.set(text(a, *combo)?);
        }
    }
    Ok(())
}

unsafe fn param(a: CoreAccess<'_>, f: *mut UFunction, params: *const u8, size: usize, name: &str, kind: &str, width: usize) -> Option<*const u8> {
    let expected = usize::from(unsafe { (*f).parms_size });
    if params.is_null() || size < expected {
        return None;
    }
    let offset = unsafe { field(a, f.cast(), name, kind, width, expected, true) }.ok()?;
    Some(unsafe { params.add(offset) })
}

unsafe fn float_param(a: CoreAccess<'_>, f: *mut UFunction, params: *const u8, size: usize, name: &str) -> Option<f32> {
    unsafe { param(a, f, params, size, name, "Core.FloatProperty", 4) }.map(|p| unsafe { p.cast::<f32>().read_unaligned() })
}

unsafe fn int_param(a: CoreAccess<'_>, f: *mut UFunction, params: *const u8, size: usize, name: &str) -> Option<i32> {
    unsafe { param(a, f, params, size, name, "Core.IntProperty", 4) }.map(|p| unsafe { p.cast::<i32>().read_unaligned() })
}

/// Builds the page's UI the first time it is painted (the stock controls exist by then).
unsafe fn create(a: CoreAccess<'_>, page: *mut UObject) -> RuntimeResult<PageUi> {
    let store = Store::new();
    let data = Data::new(&store, polish_language(a));
    let metrics = HostMetrics::default();
    let view_data = data.clone();
    let mut ui = Ui::new(store, move || view(&view_data), metrics.clone()).with_stylesheet(stylesheet()).with_skin(skin());
    ui.resize(640., 480.);
    let handles = unsafe { Handles::resolve(a, page)? };
    unsafe { refresh(a, &data, &handles)? };
    Ok(PageUi {
        page: unsafe { Weak::new(page) },
        ui,
        data,
        metrics,
        hidden: false,
        art_requested: false,
        measured_at: (0, 0),
        needs_refresh: false,
        last_refresh: std::time::Instant::now(),
    })
}

/// Removes every stock control except the Back button from the page's window list.
unsafe fn hide_stock_controls(a: CoreAccess<'_>, page: *mut UObject) -> RuntimeResult<()> {
    let back = unsafe { object(a, page, "BackPageButton")? };
    for (child, _) in unsafe { children(a, page) } {
        if child != back {
            unsafe { invoke(a, child, "HideWindow", &[], &[], std::ptr::null_mut())? };
        }
    }
    Ok(())
}

unsafe fn paint_callback(a: CoreAccess<'_>, receiver: *mut UObject, function: *mut UFunction, params: *const u8, size: usize) -> RuntimeResult<()> {
    if !is_page(a, receiver) || name_of(a, function.cast()) != "Paint" {
        return Ok(());
    }
    let result = (|| -> RuntimeResult<()> {
        let canvas_slot = unsafe { param(a, function, params, size, "C", "Core.ObjectProperty", size_of::<*mut c_void>()) };
        let Some(canvas) = canvas_slot.map(|p| unsafe { p.cast::<*mut UObject>().read_unaligned() }) else { return Ok(()) };
        let surface = unsafe { Surface::new(a, receiver, canvas, TEXTURES.with(Rc::clone))? };
        PAGES.with(|pages| -> RuntimeResult<()> {
            let Ok(mut pages) = pages.try_borrow_mut() else { return Ok(()) };
            pages.retain(|p| p.page.live(a));
            if !pages.iter().any(|p| p.page.p == receiver) {
                pages.push(unsafe { create(a, receiver)? });
            }
            let Some(entry) = pages.iter_mut().find(|p| p.page.p == receiver) else { return Ok(()) };
            if !entry.hidden {
                unsafe { hide_stock_controls(a, receiver)? };
                entry.hidden = true;
            }
            // Checked every paint: a texture that is gone (garbage collected) must never be drawn.
            let load = !entry.art_requested;
            entry.art_requested = true;
            entry.data.art.set(unsafe { menu_art(a, receiver, load) });
            // This page changes the resolution and UI scale itself: widths measured at another
            // canvas size would misplace centred and right-aligned text, so measure again.
            let size_of_canvas = |name: &str| unsafe { prop(a, canvas, name, "Core.IntProperty", 4).map(|p| p.cast::<i32>().read_unaligned()) };
            let canvas_size = (size_of_canvas("SizeX")?, size_of_canvas("SizeY")?);
            if entry.measured_at != canvas_size {
                entry.measured_at = canvas_size;
                entry.metrics.clear();
                entry.needs_refresh = true;
            }
            // While a command is waiting for the next tick, the stock controls are stale: do not overwrite the view.
            // Avoid re-walking stock combo lists and re-reading 11 INI keys on every paint when nothing changed.
            if entry.data.queue.borrow().is_empty()
                && (entry.needs_refresh || entry.last_refresh.elapsed() >= std::time::Duration::from_millis(250))
            {
                let handles = unsafe { Handles::resolve(a, receiver)? };
                unsafe { refresh(a, &entry.data, &handles)? };
                entry.needs_refresh = false;
                entry.last_refresh = std::time::Instant::now();
            }
            entry.ui.update();
            let mut learned = false;
            for (text_value, font) in entry.ui.text_requests() {
                if !entry.metrics.knows(&text_value, font) {
                    let size = unsafe { surface.text_size(a, &text_value, font)? };
                    entry.metrics.learn(&text_value, font, size);
                    learned = true;
                }
            }
            if learned {
                entry.ui.relayout();
            }
            let commands = entry.ui.draw();
            unsafe { surface.render(a, &commands, &entry.metrics) }
        })
    })();
    if let Err(error) = &result {
        warn(a, error);
    }
    Ok(())
}

fn with_page(a: CoreAccess<'_>, receiver: *mut UObject, change: impl FnOnce(&mut PageUi)) {
    PAGES.with(|pages| {
        if let Ok(mut pages) = pages.try_borrow_mut()
            && let Some(entry) = pages.iter_mut().find(|p| p.page.p == receiver && p.page.live(a))
        {
            change(entry);
        }
    });
}

fn map_key(code: i32) -> Option<Key> {
    Some(match code {
        8 => Key::Backspace,
        9 => Key::Tab,
        13 => Key::Enter,
        27 => Key::Escape,
        32 => Key::Space,
        37 => Key::Left,
        38 => Key::Up,
        39 => Key::Right,
        40 => Key::Down,
        46 => Key::Delete,
        _ => return None,
    })
}

unsafe fn input_callback(a: CoreAccess<'_>, receiver: *mut UObject, function: *mut UFunction, params: *const u8, size: usize) -> RuntimeResult<()> {
    if !is_page(a, receiver) {
        return Ok(());
    }
    // An explicit `Super.X()` call reaches the observer too; only the receiver's own dispatch counts.
    let name = name_of(a, function.cast());
    let name_id = a.intern_str(&name, FindName::Find)?;
    if unsafe { a.find_object_function_by_name(receiver, name_id, false)? } != function {
        return Ok(());
    }
    let (x, y) = unsafe { (float_param(a, function, params, size, "X"), float_param(a, function, params, size, "Y")) };
    if debug() && name != "MouseMove" {
        log(a, &format!("HP2_UI_INPUT {name} {x:?} {y:?}"));
    }
    match (name.as_str(), x, y) {
        ("MouseMove", Some(x), Some(y)) => with_page(a, receiver, |p| p.ui.pointer_move(x, y)),
        ("LMouseDown", Some(x), Some(y)) => with_page(a, receiver, |p| p.ui.pointer_down(x, y, PointerButton::Left)),
        ("LMouseUp", Some(x), Some(y)) => with_page(a, receiver, |p| p.ui.pointer_up(x, y, PointerButton::Left)),
        ("MouseLeave", _, _) => with_page(a, receiver, |p| p.ui.pointer_move(-1000., -1000.)),
        ("KeyDown", _, _) => {
            if let Some(key) = unsafe { int_param(a, function, params, size, "Key") }.and_then(map_key) {
                with_page(a, receiver, |p| p.ui.key(key));
            }
        }
        ("KeyType", _, _) => {
            if let Some(ch) = unsafe { int_param(a, function, params, size, "Key") }.and_then(|k| u32::try_from(k).ok()).and_then(char::from_u32) {
                with_page(a, receiver, |p| p.ui.text(&ch.to_string()));
            }
        }
        _ => {}
    }
    Ok(())
}

unsafe fn page_functions(a: CoreAccess<'_>, names: &[&str]) -> RuntimeResult<Option<Vec<*mut UFunction>>> {
    // `HP2_STOCK_MENUS=1` keeps the stock UnrealScript page (for comparison and as a fallback).
    if std::env::var_os("HP2_STOCK_MENUS").is_some() {
        return Ok(None);
    }
    let Some(page) = (unsafe { find_class(a, &format!("HGame.{PAGE_CLASS}"))? }) else { return Ok(None) };
    let mut out = Vec::new();
    for name in names {
        let f = unsafe { a.find_field(page.cast(), a.intern_str(name, FindName::Find)?, a.native_class("Core.Function")?) }.cast::<UFunction>();
        if f.is_null() {
            return Ok(None);
        }
        out.push(f);
    }
    Ok(Some(out))
}

/// Observe page painting for one draw.
pub(crate) unsafe fn install_paint(a: CoreAccess<'_>) -> RuntimeResult<Option<impl Drop + '_>> {
    let Some(functions) = (unsafe { page_functions(a, &["Paint"])? }) else { return Ok(None) };
    Ok(Some(unsafe { a.install_script_observer("ui-pages-paint", &functions, paint_callback)? }))
}

/// Observe pointer and keyboard input for one tick.
pub(crate) unsafe fn install_input(a: CoreAccess<'_>) -> RuntimeResult<Option<impl Drop + '_>> {
    let Some(functions) = (unsafe { page_functions(a, &["MouseMove", "LMouseDown", "LMouseUp", "MouseLeave", "KeyDown", "KeyType"])? }) else { return Ok(None) };
    Ok(Some(unsafe { a.install_script_observer("ui-pages-input", &functions, input_callback)? }))
}

/// Raises `DE_Change` on a stock control. The control's own script forwards it to the page's
/// `Notify`, exactly as a click would, so script-call hooks and observers see it.
unsafe fn notify(a: CoreAccess<'_>, _page: *mut UObject, control: *mut UObject) -> RuntimeResult<()> {
    const DE_CHANGE: u8 = 1;
    unsafe { invoke(a, control, "Notify", &[("E", V::U8(DE_CHANGE))], &[], std::ptr::null_mut())? };
    Ok(())
}

fn debug() -> bool {
    std::env::var_os("HP2_UI_DEBUG").is_some()
}

fn log(a: CoreAccess<'_>, line: &str) {
    if let Ok(category) = a.intern_str("Log", FindName::Add) {
        let _ = a.emit_log(category, &line.chars().map(u32::from).collect::<Vec<_>>());
    }
}

unsafe fn execute(a: CoreAccess<'_>, page: *mut UObject, h: &Handles, command: Command) -> RuntimeResult<()> {
    if debug() {
        let what = match &command {
            Command::Slider(c, v) => format!("slider {c:?} {v}"),
            Command::PickIndex(c, i) => format!("index {c:?} {i}"),
            Command::PickText(c, t) => format!("text {c:?} {t}"),
            Command::Check(c, on) => format!("check {c:?} {on}"),
            Command::Quality(i, on) => format!("quality {i} {on}"),
            Command::Extra(i, v) => format!("extra {i} {v}"),
        };
        log(a, &format!("HP2_UI_EXEC {what} active={}", crate::stock_menu_extensions::active()));
    }
    match command {
        Command::Slider(ctl, value) => {
            let slider = match ctl {
                Ctl::Music => Some(h.music),
                Ctl::Sound => Some(h.sound),
                Ctl::Brightness => Some(h.brightness),
                Ctl::ObjectDetail => Some(h.object_detail),
                Ctl::Scale => h.scale,
                _ => None,
            };
            if let Some(slider) = slider {
                unsafe { invoke(a, slider, "SetValue", &[("NewValue", V::F(value))], &[], std::ptr::null_mut())? };
            }
        }
        Command::PickIndex(ctl, index) => {
            let combo = if ctl == Ctl::Resolution { h.resolution } else { h.textures };
            unsafe { invoke(a, combo, "SetSelectedIndex", &[("Index", V::I(i32::try_from(index).unwrap_or(0)))], &[], std::ptr::null_mut())? };
        }
        Command::PickText(ctl, choice) => {
            let combo = match ctl {
                Ctl::Display => h.display,
                Ctl::Advanced(i) => h.advanced.get(i).copied(),
                _ => None,
            };
            if let Some(combo) = combo {
                unsafe { set_text(a, combo, &choice)? };
                unsafe { notify(a, page, combo)? };
            }
        }
        Command::Quality(index, on) => {
            let units = |t: &str| t.chars().map(u32::from).collect::<Vec<_>>();
            let key = crate::render_device_owner::QUALITY_KEYS.get(index).ok_or_else(|| RuntimeError::new("Unknown quality setting"))?;
            a.config_update_system_staged(&[(units(crate::render_device_owner::QUALITY_SECTION), units(key), units(if on { "True" } else { "False" }))])?;
            let quality = crate::render_device_owner::quality_settings(a);
            #[cfg(feature = "renderer-wgpu")]
            unsafe { crate::render_device_owner::set_global_quality(a, quality)? };
            #[cfg(not(feature = "renderer-wgpu"))]
            let _ = quality;
        }
        Command::Extra(index, value) => {
            let set = PAGES.with(|pages| pages.borrow().iter().find(|p| p.page.p == page).and_then(|p| p.data.extras.get(index).map(|(o, _)| o.set)));
            let set = set.ok_or_else(|| RuntimeError::new("Unknown extra setting"))?;
            unsafe { set(a, value)? };
        }
        Command::Check(_, on) => {
            if let Some(fps) = h.fps {
                unsafe { boolean(a, fps, "bChecked", on)? };
                unsafe { notify(a, page, fps)? };
            }
        }
    }
    Ok(())
}

/// Runs the commands the view queued. Called inside the engine tick, where the stock hook that
/// drives the `stock_*` observers is active, so the same `Notify` path as a stock click runs.
pub(crate) unsafe fn tick(a: CoreAccess<'_>) -> RuntimeResult<()> {
    let work: Vec<(*mut UObject, Vec<Command>)> = PAGES.with(|pages| {
        let Ok(mut pages) = pages.try_borrow_mut() else { return Vec::new() };
        pages.retain(|p| p.page.live(a));
        pages.iter().map(|p| (p.page.p, std::mem::take(&mut *p.data.queue.borrow_mut()))).filter(|(_, c)| !c.is_empty()).collect()
    });
    for (page, commands) in work {
        let handles = unsafe { Handles::resolve(a, page)? };
        for command in commands {
            unsafe { execute(a, page, &handles, command)? };
        }
        with_page(a, page, |p| p.needs_refresh = true);
    }
    Ok(())
}
