use anyhow::{bail, Context, Result};
use board_core::config::ThemeConfig;
use ratatui::style::Color;
use std::cell::Cell;

pub const BUILTIN_THEMES: &[&str] = &[
    "board", "stem", "dracula", "gruvbox", "nord", "light", "terminal",
];

/// Semantic palette for the board TUI. Views consume roles rather than named
/// terminal colors so a light theme remains as legible as the original dark
/// presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub background: Color,
    pub panel: Color,
    pub selection: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_alt: Color,
    pub success: Color,
    pub info: Color,
    pub warning: Color,
    pub error: Color,
}

fn rgb(value: u32) -> Color {
    Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

fn from_rgb(values: [u32; 12]) -> Theme {
    let [background, panel, selection, border, text, muted, accent, accent_alt, success, info, warning, error] =
        values.map(rgb);
    Theme {
        background,
        panel,
        selection,
        border,
        text,
        muted,
        accent,
        accent_alt,
        success,
        info,
        warning,
        error,
    }
}

fn builtin(name: &str) -> Option<Theme> {
    match name {
        // The existing herdr-board palette remains the compatibility default.
        "board" => Some(from_rgb([
            0x030d16, 0x071622, 0x082133, 0x435b69, 0xf2f6f8, 0x7d8a94, 0x75d7ff, 0xd67cff,
            0x7ee787, 0x56d4dd, 0xe3b341, 0xff7b72,
        ])),
        "stem" => Some(from_rgb([
            0x11111b, 0x181826, 0x222031, 0x313142, 0xcecde1, 0x8c8da2, 0xbea0eb, 0xd6a5ef,
            0xa7cf91, 0x89b4fa, 0xe7d196, 0xeb8b8b,
        ])),
        "dracula" => Some(from_rgb([
            0x282a36, 0x21222c, 0x44475a, 0x626580, 0xf8f8f2, 0xa5a8c8, 0xbd93f9, 0xff79c6,
            0x50fa7b, 0x8be9fd, 0xf1fa8c, 0xff5555,
        ])),
        "gruvbox" => Some(from_rgb([
            0x282828, 0x32302f, 0x504945, 0x665c54, 0xebdbb2, 0xa89984, 0xfabd2f, 0xd3869b,
            0xb8bb26, 0x83a598, 0xfe8019, 0xfb4934,
        ])),
        "nord" => Some(from_rgb([
            0x2e3440, 0x353d4b, 0x434c5e, 0x616e88, 0xe5e9f0, 0xa7b4ca, 0x88c0d0, 0xb48ead,
            0xa3be8c, 0x81a1c1, 0xebcb8b, 0xbf616a,
        ])),
        "light" => Some(from_rgb([
            0xfaf9f6, 0xf0eee9, 0xe6ddf5, 0xb9b5c2, 0x292532, 0x686273, 0x6841a5, 0x8f4f9f,
            0x347141, 0x216a80, 0x916000, 0xb63142,
        ])),
        "terminal" => Some(Theme {
            background: Color::Reset,
            panel: Color::Reset,
            selection: Color::DarkGray,
            border: Color::DarkGray,
            text: Color::Reset,
            muted: Color::DarkGray,
            accent: Color::LightCyan,
            accent_alt: Color::LightMagenta,
            success: Color::LightGreen,
            info: Color::LightBlue,
            warning: Color::LightYellow,
            error: Color::LightRed,
        }),
        _ => None,
    }
}

impl Default for Theme {
    fn default() -> Self {
        builtin("board").expect("the built-in board theme exists")
    }
}

impl Theme {
    pub fn from_config(config: &ThemeConfig) -> Result<Self> {
        let mut theme = builtin(&config.name).with_context(|| {
            format!(
                "unknown board theme {:?}; choose one of {}",
                config.name,
                BUILTIN_THEMES.join(", ")
            )
        })?;
        let custom = &config.custom;
        apply(&mut theme.background, "background", &custom.background)?;
        apply(&mut theme.panel, "panel_bg", &custom.panel_bg)?;
        apply(&mut theme.selection, "selection_bg", &custom.selection_bg)?;
        apply(&mut theme.border, "border", &custom.border)?;
        apply(&mut theme.text, "text", &custom.text)?;
        apply(&mut theme.muted, "muted", &custom.muted)?;
        apply(&mut theme.accent, "accent", &custom.accent)?;
        apply(&mut theme.accent_alt, "accent_alt", &custom.accent_alt)?;
        apply(&mut theme.success, "green", &custom.green)?;
        apply(&mut theme.info, "blue", &custom.blue)?;
        apply(&mut theme.warning, "yellow", &custom.yellow)?;
        apply(&mut theme.error, "red", &custom.red)?;
        Ok(theme)
    }

    /// Foreground with enough contrast for a semantic filled background.
    pub fn on(&self, background: Color) -> Color {
        match background {
            Color::Yellow | Color::LightYellow => Color::Black,
            Color::Red | Color::LightRed => Color::White,
            Color::Rgb(r, g, b)
                if u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 > 150_000 =>
            {
                rgb(0x17151c)
            }
            Color::Reset => self.text,
            _ => rgb(0xf8f6fb),
        }
    }
}

thread_local! {
    static RENDER_THEME: Cell<Theme> = Cell::new(Theme::default());
}

/// Scope the implicit palette used by low-level shared widgets to one draw.
/// Thread-local storage keeps parallel snapshot tests independent while
/// avoiding theme plumbing through every generic button helper.
pub(crate) fn with_render_theme<T>(theme: Theme, render: impl FnOnce() -> T) -> T {
    RENDER_THEME.with(|slot| {
        let previous = slot.replace(theme);
        let result = render();
        slot.set(previous);
        result
    })
}

pub(crate) fn render_theme() -> Theme {
    RENDER_THEME.get()
}

fn apply(target: &mut Color, role: &str, value: &Option<String>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let normalized = match value.to_ascii_lowercase().as_str() {
        "default" | "none" | "transparent" => "reset",
        _ => value,
    };
    *target = normalized
        .parse::<Color>()
        .map_err(|_| anyhow::anyhow!("invalid theme color {role}: {value}"))?;
    if matches!(target, Color::Indexed(_)) {
        bail!("indexed colors are not supported in theme color {role}: {value}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::config::{ThemeConfig, ThemeOverrides};

    #[test]
    fn light_theme_is_a_complete_light_palette() {
        let theme = Theme::from_config(&ThemeConfig {
            name: "light".into(),
            ..ThemeConfig::default()
        })
        .unwrap();

        assert_eq!(theme.background, Color::Rgb(0xfa, 0xf9, 0xf6));
        assert_ne!(theme.text, theme.background);
        assert_ne!(theme.selection, theme.panel);
    }

    #[test]
    fn custom_roles_override_the_selected_builtin() {
        let theme = Theme::from_config(&ThemeConfig {
            name: "stem".into(),
            custom: ThemeOverrides {
                accent: Some("#123456".into()),
                panel_bg: Some("reset".into()),
                ..ThemeOverrides::default()
            },
        })
        .unwrap();

        assert_eq!(theme.accent, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(theme.panel, Color::Reset);
    }

    #[test]
    fn unknown_theme_and_invalid_color_fail_explicitly() {
        let unknown = Theme::from_config(&ThemeConfig {
            name: "mystery".into(),
            ..ThemeConfig::default()
        })
        .unwrap_err();
        assert!(unknown.to_string().contains("unknown board theme"));

        let invalid = Theme::from_config(&ThemeConfig {
            custom: ThemeOverrides {
                accent: Some("definitely-not-a-color".into()),
                ..ThemeOverrides::default()
            },
            ..ThemeConfig::default()
        })
        .unwrap_err();
        assert!(invalid.to_string().contains("invalid theme color accent"));
    }

    #[test]
    fn terminal_alert_backgrounds_keep_the_original_readable_foregrounds() {
        let theme = builtin("terminal").unwrap();
        assert_eq!(theme.on(theme.warning), Color::Black);
        assert_eq!(theme.on(theme.error), Color::White);
    }
}
