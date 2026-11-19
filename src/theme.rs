use eframe::egui::{self, Color32, Stroke, Visuals};

struct Palette {
    id: &'static str,
    label: &'static str,
    bg: &'static str,
    surface: &'static str,
    surface2: &'static str,
    accent: &'static str,
    accent2: &'static str,
    text: &'static str,
    border: &'static str,
    input_bg: &'static str,
    selected: &'static str,
    light: bool,
}
const THEMES: &[Palette] = &[
    Palette {
        id: "github_dark",
        label: "GitHub Dark (기본)",
        bg: "0D1117",
        surface: "161B22",
        surface2: "21262D",
        accent: "238636",
        accent2: "1F6FEB",
        text: "E6EDF3",
        border: "30363D",
        input_bg: "010409",
        selected: "1C4A7E",
        light: false,
    },
    Palette {
        id: "github_light",
        label: "GitHub Light",
        bg: "FFFFFF",
        surface: "F6F8FA",
        surface2: "EAEEF2",
        accent: "1F883D",
        accent2: "0969DA",
        text: "1F2328",
        border: "D0D7DE",
        input_bg: "FFFFFF",
        selected: "C8E1FF",
        light: true,
    },
    Palette {
        id: "catppuccin_mocha",
        label: "Catppuccin Mocha",
        bg: "1E1E2E",
        surface: "181825",
        surface2: "313244",
        accent: "A6E3A1",
        accent2: "89B4FA",
        text: "CDD6F4",
        border: "45475A",
        input_bg: "11111B",
        selected: "45475A",
        light: false,
    },
    Palette {
        id: "dracula",
        label: "Dracula",
        bg: "282A36",
        surface: "21222C",
        surface2: "343746",
        accent: "50FA7B",
        accent2: "BD93F9",
        text: "F8F8F2",
        border: "44475A",
        input_bg: "191A21",
        selected: "44475A",
        light: false,
    },
    Palette {
        id: "one_dark",
        label: "One Dark",
        bg: "21252B",
        surface: "282C34",
        surface2: "2C313C",
        accent: "98C379",
        accent2: "61AFEF",
        text: "DCDFE4",
        border: "3E4451",
        input_bg: "1B1F25",
        selected: "3E4451",
        light: false,
    },
    Palette {
        id: "tokyo_night",
        label: "Tokyo Night",
        bg: "1A1B26",
        surface: "16161E",
        surface2: "24283B",
        accent: "9ECE6A",
        accent2: "7AA2F7",
        text: "C0CAF5",
        border: "3B4261",
        input_bg: "0F0F17",
        selected: "283457",
        light: false,
    },
    Palette {
        id: "nord",
        label: "Nord",
        bg: "2E3440",
        surface: "3B4252",
        surface2: "434C5E",
        accent: "A3BE8C",
        accent2: "88C0D0",
        text: "ECEFF4",
        border: "4C566A",
        input_bg: "232831",
        selected: "5E81AC",
        light: false,
    },
    Palette {
        id: "solarized_dark",
        label: "Solarized Dark",
        bg: "002B36",
        surface: "073642",
        surface2: "0D3D4E",
        accent: "859900",
        accent2: "268BD2",
        text: "EEE8D5",
        border: "586E75",
        input_bg: "001F27",
        selected: "1A5276",
        light: false,
    },
    Palette {
        id: "gruvbox",
        label: "Gruvbox Dark",
        bg: "282828",
        surface: "32302F",
        surface2: "3C3836",
        accent: "B8BB26",
        accent2: "83A598",
        text: "EBDBB2",
        border: "504945",
        input_bg: "1D2021",
        selected: "458588",
        light: false,
    },
    Palette {
        id: "ayu_dark",
        label: "Ayu Dark",
        bg: "0D1017",
        surface: "131721",
        surface2: "1A2130",
        accent: "91B362",
        accent2: "39BAE6",
        text: "BFBDB6",
        border: "2D3640",
        input_bg: "05080D",
        selected: "273949",
        light: false,
    },
    Palette {
        id: "monokai",
        label: "Monokai",
        bg: "272822",
        surface: "1E1F1C",
        surface2: "3E3D32",
        accent: "A6E22E",
        accent2: "66D9EF",
        text: "F8F8F2",
        border: "49483E",
        input_bg: "16170F",
        selected: "49483E",
        light: false,
    },
    Palette {
        id: "cyberpunk",
        label: "Cyberpunk Neon",
        bg: "0A0E27",
        surface: "151933",
        surface2: "1F2547",
        accent: "FF2CD4",
        accent2: "00F0FF",
        text: "E0E3FF",
        border: "2D3563",
        input_bg: "04061A",
        selected: "2D2D6E",
        light: false,
    },
    Palette {
        id: "rose_pine",
        label: "Rosé Pine",
        bg: "191724",
        surface: "1F1D2E",
        surface2: "26233A",
        accent: "9CCFD8",
        accent2: "C4A7E7",
        text: "E0DEF4",
        border: "403D52",
        input_bg: "0F0E17",
        selected: "403D52",
        light: false,
    },
    Palette {
        id: "material_ocean",
        label: "Material Ocean",
        bg: "0F111A",
        surface: "1A1C25",
        surface2: "232530",
        accent: "C3E88D",
        accent2: "82AAFF",
        text: "C0C5CE",
        border: "2E3142",
        input_bg: "070810",
        selected: "1F2233",
        light: false,
    },
];
pub fn entries() -> impl Iterator<Item = (&'static str, &'static str)> {
    THEMES.iter().map(|t| (t.id, t.label))
}
pub fn swatches(id: &str) -> (Color32, Color32) {
    let palette = THEMES
        .iter()
        .find(|theme| theme.id == id)
        .or_else(|| THEMES.first())
        .expect("theme palette list is not empty");
    (color(palette.surface), color(palette.accent2))
}
fn color(hex: &str) -> Color32 {
    let v = u32::from_str_radix(hex, 16).unwrap_or(0);
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}
pub fn apply(ctx: &egui::Context, id: &str) {
    let Some(t) = THEMES
        .iter()
        .find(|x| x.id == id)
        .or_else(|| THEMES.first())
    else {
        return;
    };
    let mut v = if t.light {
        Visuals::light()
    } else {
        Visuals::dark()
    };
    let bg = color(t.bg);
    let surface = color(t.surface);
    let accent = color(t.accent);
    let accent2 = color(t.accent2);
    let text = color(t.text);
    let border = color(t.border);
    let surface2 = color(t.surface2);
    v.panel_fill = bg;
    v.window_fill = surface;
    v.extreme_bg_color = color(t.input_bg);
    v.faint_bg_color = surface2;
    v.hyperlink_color = accent2;
    v.selection.bg_fill = color(t.selected);
    v.selection.stroke = Stroke::new(1.0_f32, text);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.fg_stroke = Stroke::new(1.0_f32, text);
        w.bg_stroke = Stroke::new(1.0_f32, border);
    }
    v.widgets.noninteractive.bg_fill = bg;
    v.widgets.inactive.bg_fill = surface;
    v.widgets.hovered.bg_fill = surface2;
    v.widgets.active.bg_fill = accent;
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;

    let mut style = (*ctx.style()).clone();
    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(10.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.interact_size.y = 24.0;
    style.spacing.window_margin = egui::Margin::symmetric(10.0, 8.0);
    style.spacing.menu_margin = egui::Margin::same(8.0);
    ctx.set_style(style);
}
