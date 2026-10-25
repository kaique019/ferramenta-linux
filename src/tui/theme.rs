//! Tema da TUI: cores, estilos de texto, bordas e símbolos, ajustáveis por um arquivo JSON.
//!
//! O tipo de letra em si é do emulador de terminal e o programa não consegue trocá-lo; aqui se controla tudo o que é
//! do programa: cores (nome, `#rrggbb` ou número 0-255), negrito, itálico, sublinhado, bordas e símbolos.
//!
//! `~/.config/resetexplode/theme.json` pode ter `"preset"` (base) e qualquer campo para sobrescrever. Campo
//! desconhecido ou cor inválida não derruba o programa: ele avisa e usa o tema padrão.

use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType};
use serde::{Deserialize, Serialize};

pub const PRESETS: [&str; 5] = ["padrao", "claro", "contraste", "retro", "neon"];

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub accent: Color,
    pub muted: Color,
    pub ok: Color,
    pub warn: Color,
    pub err: Color,
    pub sel_fg: Color,
    pub sel_bg: Color,
    pub border: Color,
    pub border_focus: Color,
    pub gauge_bg: Color,
    pub bold_headings: bool,
    pub italic_muted: bool,
    pub underline_selection: bool,
    pub rounded: bool,
    pub pointer: String,
    pub check_on: String,
    pub check_off: String,
    pub divider: String,
}

/// Formato do arquivo: tudo opcional, e campos desconhecidos são erro (pega erros de digitação).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeFile {
    pub preset: Option<String>,
    pub accent: Option<String>,
    pub muted: Option<String>,
    pub ok: Option<String>,
    pub warn: Option<String>,
    pub err: Option<String>,
    pub selection_fg: Option<String>,
    pub selection_bg: Option<String>,
    pub border: Option<String>,
    pub border_focus: Option<String>,
    pub gauge_bg: Option<String>,
    pub bold_headings: Option<bool>,
    pub italic_muted: Option<bool>,
    pub underline_selection: Option<bool>,
    pub rounded_borders: Option<bool>,
    pub pointer: Option<String>,
    pub check_on: Option<String>,
    pub check_off: Option<String>,
    pub divider: Option<String>,
}

fn rgb(h: u32) -> Color {
    Color::Rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)
}

pub fn preset(name: &str) -> Option<Theme> {
    let base = Theme {
        name: "padrao".into(),
        accent: Color::Cyan,
        muted: Color::DarkGray,
        ok: Color::Green,
        warn: Color::Yellow,
        err: Color::Red,
        sel_fg: Color::Black,
        sel_bg: Color::Cyan,
        border: Color::Reset,
        border_focus: Color::Cyan,
        gauge_bg: Color::Black,
        bold_headings: true,
        italic_muted: false,
        underline_selection: false,
        rounded: false,
        pointer: "▶ ".into(),
        check_on: "[✓]".into(),
        check_off: "[ ]".into(),
        divider: "│".into(),
    };
    Some(match name {
        "padrao" => base,
        "claro" => Theme { name: "claro".into(), accent: Color::Blue, muted: Color::Gray, sel_fg: Color::White, sel_bg: Color::Blue, border: Color::Gray, border_focus: Color::Blue, gauge_bg: Color::Gray, ok: rgb(0x1a7f37), warn: rgb(0x9a6700), err: rgb(0xcf222e), ..base },
        "contraste" => Theme { name: "contraste".into(), accent: Color::White, muted: Color::Gray, ok: Color::LightGreen, warn: Color::LightYellow, err: Color::LightRed, sel_fg: Color::Black, sel_bg: Color::White, border: Color::White, border_focus: Color::LightYellow, gauge_bg: Color::DarkGray, underline_selection: true, pointer: ">> ".into(), ..base },
        "retro" => Theme { name: "retro".into(), accent: rgb(0x33ff33), muted: rgb(0x1a9c1a), ok: rgb(0x33ff33), warn: rgb(0xb8ff33), err: rgb(0xff5555), sel_fg: Color::Black, sel_bg: rgb(0x33ff33), border: rgb(0x1a9c1a), border_focus: rgb(0x33ff33), gauge_bg: rgb(0x062406), pointer: "> ".into(), check_on: "[x]".into(), divider: "|".into(), ..base },
        "neon" => Theme { name: "neon".into(), accent: rgb(0xff4fd8), muted: rgb(0x7a7aa0), ok: rgb(0x39ff88), warn: rgb(0xffd23f), err: rgb(0xff4d6d), sel_fg: Color::Black, sel_bg: rgb(0xff4fd8), border: rgb(0x5b5b8c), border_focus: rgb(0x00e5ff), gauge_bg: rgb(0x1b1b2f), rounded: true, italic_muted: true, ..base },
        _ => return None,
    })
}

impl Default for Theme {
    fn default() -> Self {
        preset("padrao").expect("preset padrão existe")
    }
}

/// Aceita nomes (`red`, `light-blue`, `dark_gray`), `#rrggbb`, `#rgb`, números 0-255 e `reset`.
pub fn parse_color(s: &str) -> Result<Color, String> {
    let t = s.trim().to_lowercase();
    if let Some(h) = t.strip_prefix('#') {
        let ok = h.chars().all(|c| c.is_ascii_hexdigit());
        let v = match (ok, h.len()) {
            (true, 6) => u32::from_str_radix(h, 16).ok(),
            (true, 3) => u32::from_str_radix(h, 16).ok().map(|v| ((v >> 8 & 0xf) * 0x11) << 16 | ((v >> 4 & 0xf) * 0x11) << 8 | (v & 0xf) * 0x11),
            _ => None,
        };
        return v.map(rgb).ok_or_else(|| format!("cor hexadecimal inválida: {s:?} (use #rrggbb)"));
    }
    if let Ok(n) = t.parse::<u16>() {
        return u8::try_from(n).map(Color::Indexed).map_err(|_| format!("número de cor fora de 0-255: {s:?}"));
    }
    let key: String = t.chars().filter(|c| !matches!(c, '-' | '_' | ' ')).collect();
    Ok(match key.as_str() {
        "reset" | "default" | "none" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return Err(format!("cor desconhecida: {s:?} (nome, #rrggbb ou número 0-255)")),
    })
}

pub fn color_name(c: Color) -> String {
    match c {
        Color::Reset => "reset".into(),
        Color::Black => "black".into(),
        Color::Red => "red".into(),
        Color::Green => "green".into(),
        Color::Yellow => "yellow".into(),
        Color::Blue => "blue".into(),
        Color::Magenta => "magenta".into(),
        Color::Cyan => "cyan".into(),
        Color::Gray => "gray".into(),
        Color::DarkGray => "dark-gray".into(),
        Color::LightRed => "light-red".into(),
        Color::LightGreen => "light-green".into(),
        Color::LightYellow => "light-yellow".into(),
        Color::LightBlue => "light-blue".into(),
        Color::LightMagenta => "light-magenta".into(),
        Color::LightCyan => "light-cyan".into(),
        Color::White => "white".into(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(n) => n.to_string(),
    }
}

/// Símbolos são texto curto e sem caracteres de controle (um símbolo com ESC poderia mexer no terminal).
fn symbol(s: &str, what: &str) -> Result<String, String> {
    if s.chars().count() > 6 || s.chars().any(|c| c.is_control()) {
        return Err(format!("{what} inválido: use até 6 caracteres, sem caracteres de controle"));
    }
    Ok(s.to_string())
}

impl Theme {
    pub fn from_file(f: &ThemeFile) -> Result<Theme, String> {
        let base = f.preset.as_deref().unwrap_or("padrao");
        let mut t = preset(base).ok_or_else(|| format!("preset desconhecido: {base:?} (opções: {})", PRESETS.join(", ")))?;
        let color = |v: &Option<String>, field: &str, dst: &mut Color| -> Result<(), String> {
            if let Some(s) = v {
                *dst = parse_color(s).map_err(|e| format!("{field}: {e}"))?;
            }
            Ok(())
        };
        color(&f.accent, "accent", &mut t.accent)?;
        color(&f.muted, "muted", &mut t.muted)?;
        color(&f.ok, "ok", &mut t.ok)?;
        color(&f.warn, "warn", &mut t.warn)?;
        color(&f.err, "err", &mut t.err)?;
        color(&f.selection_fg, "selection_fg", &mut t.sel_fg)?;
        color(&f.selection_bg, "selection_bg", &mut t.sel_bg)?;
        color(&f.border, "border", &mut t.border)?;
        color(&f.border_focus, "border_focus", &mut t.border_focus)?;
        color(&f.gauge_bg, "gauge_bg", &mut t.gauge_bg)?;
        if let Some(b) = f.bold_headings { t.bold_headings = b }
        if let Some(b) = f.italic_muted { t.italic_muted = b }
        if let Some(b) = f.underline_selection { t.underline_selection = b }
        if let Some(b) = f.rounded_borders { t.rounded = b }
        if let Some(s) = &f.pointer { t.pointer = symbol(s, "pointer")? }
        if let Some(s) = &f.check_on { t.check_on = symbol(s, "check_on")? }
        if let Some(s) = &f.check_off { t.check_off = symbol(s, "check_off")? }
        if let Some(s) = &f.divider { t.divider = symbol(s, "divider")? }
        // O nome diz se o resultado ainda é o preset puro (inclusive um arquivo completo gravado por `theme --write`).
        t.name = base.to_string();
        if preset(base).as_ref() != Some(&t) {
            t.name = if f.preset.is_some() { format!("{base}+") } else { "personalizado".into() };
        }
        Ok(t)
    }

    /// Arquivo completo (todos os campos) para o usuário copiar e editar.
    pub fn to_file(&self) -> ThemeFile {
        ThemeFile {
            preset: PRESETS.contains(&self.name.as_str()).then(|| self.name.clone()),
            accent: Some(color_name(self.accent)),
            muted: Some(color_name(self.muted)),
            ok: Some(color_name(self.ok)),
            warn: Some(color_name(self.warn)),
            err: Some(color_name(self.err)),
            selection_fg: Some(color_name(self.sel_fg)),
            selection_bg: Some(color_name(self.sel_bg)),
            border: Some(color_name(self.border)),
            border_focus: Some(color_name(self.border_focus)),
            gauge_bg: Some(color_name(self.gauge_bg)),
            bold_headings: Some(self.bold_headings),
            italic_muted: Some(self.italic_muted),
            underline_selection: Some(self.underline_selection),
            rounded_borders: Some(self.rounded),
            pointer: Some(self.pointer.clone()),
            check_on: Some(self.check_on.clone()),
            check_off: Some(self.check_off.clone()),
            divider: Some(self.divider.clone()),
        }
    }

    pub fn muted(&self) -> Style {
        let s = Style::new().fg(self.muted);
        if self.italic_muted { s.add_modifier(Modifier::ITALIC) } else { s }
    }

    pub fn heading(&self) -> Style {
        let s = Style::new().fg(self.accent);
        if self.bold_headings { s.add_modifier(Modifier::BOLD) } else { s }
    }

    pub fn accent_style(&self) -> Style {
        Style::new().fg(self.accent)
    }

    pub fn ok(&self) -> Style {
        Style::new().fg(self.ok)
    }

    pub fn warn(&self) -> Style {
        Style::new().fg(self.warn)
    }

    pub fn err(&self) -> Style {
        Style::new().fg(self.err)
    }

    pub fn selection(&self) -> Style {
        let s = Style::new().fg(self.sel_fg).bg(self.sel_bg);
        if self.underline_selection { s.add_modifier(Modifier::UNDERLINED | Modifier::BOLD) } else { s }
    }

    pub fn gauge(&self, fg: Color) -> Style {
        Style::new().fg(fg).bg(self.gauge_bg)
    }

    pub fn level_color(&self, pct: f64) -> Color {
        if pct >= 90.0 { self.err } else if pct >= 70.0 { self.warn } else { self.ok }
    }

    pub fn temp_color(&self, t: f64) -> Color {
        if t >= 85.0 { self.err } else if t >= 70.0 { self.warn } else { self.ok }
    }

    pub fn check(&self, on: bool) -> &str {
        if on { &self.check_on } else { &self.check_off }
    }

    /// Bloco com borda no estilo do tema.
    pub fn block<'a>(&self, title: impl Into<ratatui::text::Line<'a>>) -> Block<'a> {
        self.focus_block(title, false)
    }

    pub fn focus_block<'a>(&self, title: impl Into<ratatui::text::Line<'a>>, focused: bool) -> Block<'a> {
        Block::bordered().border_type(if self.rounded { BorderType::Rounded } else { BorderType::Plain }).border_style(Style::new().fg(if focused { self.border_focus } else { self.border })).title(title)
    }

    /// Próximo preset da lista (um tema personalizado volta para o primeiro).
    pub fn next_preset(&self) -> Theme {
        let base = self.name.trim_end_matches('+');
        let i = PRESETS.iter().position(|p| *p == base).map(|i| (i + 1) % PRESETS.len()).unwrap_or(0);
        preset(PRESETS[i]).expect("preset da lista existe")
    }
}

/// Lê o tema do arquivo. Arquivo ausente = tema padrão sem aviso; erro = tema padrão com o motivo.
pub fn load(path: &Path) -> (Theme, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Theme::default(), None),
        Err(e) => return (Theme::default(), Some(format!("não consegui ler {}: {e}", path.display()))),
    };
    if text.len() > 64 * 1024 {
        return (Theme::default(), Some(format!("{} é grande demais para um tema", path.display())));
    }
    match serde_json::from_str::<ThemeFile>(&text).map_err(|e| e.to_string()).and_then(|f| Theme::from_file(&f)) {
        Ok(t) => (t, None),
        Err(e) => (Theme::default(), Some(format!("tema inválido em {}: {e}. Usando o tema padrão.", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_hex_and_indexed_colors() {
        assert_eq!(parse_color("Red").unwrap(), Color::Red);
        assert_eq!(parse_color("light-blue").unwrap(), Color::LightBlue);
        assert_eq!(parse_color("DARK_GRAY").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("dark gray").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("#ff8800").unwrap(), Color::Rgb(255, 136, 0));
        assert_eq!(parse_color("#f80").unwrap(), Color::Rgb(255, 136, 0));
        assert_eq!(parse_color("208").unwrap(), Color::Indexed(208));
        assert_eq!(parse_color(" reset ").unwrap(), Color::Reset);
        for bad in ["", "vermelho", "#12", "#gggggg", "256", "-1", "#1234567"] {
            assert!(parse_color(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn every_preset_exists_and_roundtrips_through_json() {
        for name in PRESETS {
            let t = preset(name).unwrap();
            assert_eq!(t.name, name);
            let json = serde_json::to_string_pretty(&t.to_file()).unwrap();
            let back = Theme::from_file(&serde_json::from_str(&json).unwrap()).unwrap();
            assert_eq!(back, t, "{name}");
        }
        assert!(preset("inexistente").is_none());
    }

    #[test]
    fn overrides_apply_on_top_of_preset() {
        let f: ThemeFile = serde_json::from_str(r##"{"preset":"retro","accent":"#ff8800","bold_headings":false,"pointer":"-> "}"##).unwrap();
        let t = Theme::from_file(&f).unwrap();
        assert_eq!(t.accent, Color::Rgb(255, 136, 0));
        assert!(!t.bold_headings);
        assert_eq!(t.pointer, "-> ");
        assert_eq!(t.muted, preset("retro").unwrap().muted, "o resto vem do preset");
        assert_eq!(t.name, "retro+");
    }

    #[test]
    fn empty_file_is_the_default_theme() {
        let t = Theme::from_file(&serde_json::from_str("{}").unwrap()).unwrap();
        assert_eq!(t, Theme::default());
    }

    #[test]
    fn typos_and_bad_values_are_errors_with_the_field_name() {
        assert!(serde_json::from_str::<ThemeFile>(r#"{"acent":"red"}"#).is_err(), "campo desconhecido");
        let e = Theme::from_file(&serde_json::from_str(r#"{"accent":"vermelho"}"#).unwrap()).unwrap_err();
        assert!(e.contains("accent") && e.contains("vermelho"), "{e}");
        assert!(Theme::from_file(&serde_json::from_str(r#"{"preset":"x"}"#).unwrap()).unwrap_err().contains("opções"));
        for bad in ["\u{1b}[2J", "muito longo!", "a\nb"] {
            let f = ThemeFile { pointer: Some(bad.into()), ..Default::default() };
            assert!(Theme::from_file(&f).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn load_handles_missing_broken_and_valid_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("theme.json");
        assert_eq!(load(&p), (Theme::default(), None));
        std::fs::write(&p, "{ quebrado").unwrap();
        let (t, w) = load(&p);
        assert_eq!(t, Theme::default());
        assert!(w.unwrap().contains("tema inválido"));
        std::fs::write(&p, r#"{"preset":"neon"}"#).unwrap();
        assert_eq!(load(&p), (preset("neon").unwrap(), None));
        std::fs::write(&p, vec![b' '; 70_000]).unwrap();
        assert!(load(&p).1.is_some());
    }

    #[test]
    fn next_preset_cycles_and_custom_starts_over() {
        let mut t = Theme::default();
        let mut seen = vec![t.name.clone()];
        for _ in 0..PRESETS.len() {
            t = t.next_preset();
            seen.push(t.name.clone());
        }
        assert_eq!(seen.first(), seen.last(), "dá a volta");
        let custom = Theme { name: "personalizado".into(), ..Theme::default() };
        assert_eq!(custom.next_preset().name, "padrao");
    }

    #[test]
    fn styles_follow_flags() {
        let mut t = Theme::default();
        assert!(t.heading().add_modifier.contains(Modifier::BOLD) && !t.muted().add_modifier.contains(Modifier::ITALIC));
        t.bold_headings = false;
        t.italic_muted = true;
        t.underline_selection = true;
        assert!(!t.heading().add_modifier.contains(Modifier::BOLD) && t.muted().add_modifier.contains(Modifier::ITALIC));
        assert!(t.selection().add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(t.level_color(95.0), t.err);
        assert_eq!(t.temp_color(75.0), t.warn);
        assert_eq!(t.check(true), "[✓]");
    }
}
