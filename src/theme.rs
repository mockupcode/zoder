use ratatui::style::{Color, Modifier, Style};

/// Palette taken from a live truecolor capture of the reference TUI
/// at 120x40 (`38;2;r;g;b` / `48;2;r;g;b` / OSC 12).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub bg_dark: Color,
    pub bg_light: Color,
    pub bg_sel: Color,
    pub fg: Color,
    pub fg_bright: Color,
    pub fg_dim: Color,
    pub fg_mute: Color,
    pub fg_gray: Color,
    pub fg_mid: Color,
    pub border: Color,
    pub prompt_border: Color,
    pub logo: Color,
    pub gold: Color,
    pub keyword: Color,
    pub cyan: Color,
    pub info: Color,
    pub green: Color,
    pub red: Color,
    pub orange: Color,
    pub md: Color,
    pub diff_del_bg: Color,
    pub diff_ins_bg: Color,
    pub cursor: Color,
}

impl Theme {
    pub fn night() -> Self {
        Self {
            bg: Color::Rgb(20, 20, 20),
            bg_dark: Color::Rgb(17, 17, 17),
            bg_light: Color::Rgb(36, 36, 36),
            bg_sel: Color::Rgb(54, 54, 54),
            fg: Color::Rgb(225, 225, 225),
            fg_bright: Color::Rgb(200, 200, 200),
            fg_dim: Color::Rgb(108, 108, 108),
            fg_mute: Color::Rgb(88, 88, 88),
            fg_gray: Color::Rgb(120, 120, 120),
            fg_mid: Color::Rgb(128, 128, 128),
            border: Color::Rgb(51, 51, 51),
            prompt_border: Color::Rgb(80, 80, 88),
            logo: Color::Rgb(113, 113, 113),
            gold: Color::Rgb(224, 175, 104),
            keyword: Color::Rgb(187, 154, 247),
            cyan: Color::Rgb(137, 221, 255),
            info: Color::Rgb(13, 185, 215),
            green: Color::Rgb(158, 206, 106),
            red: Color::Rgb(247, 118, 142),
            orange: Color::Rgb(255, 158, 100),
            md: Color::Rgb(154, 189, 245),
            diff_del_bg: Color::Rgb(73, 8, 18),
            diff_ins_bg: Color::Rgb(0, 57, 0),
            cursor: Color::Rgb(200, 200, 200),
        }
    }

    pub fn base(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(self.fg_dim).bg(self.bg)
    }

    pub fn mute(&self) -> Style {
        Style::default().fg(self.fg_mute).bg(self.bg)
    }

    pub fn accent(&self) -> Style {
        Style::default().fg(self.gold).bg(self.bg)
    }

    pub fn accent_bold(&self) -> Style {
        self.accent().add_modifier(Modifier::BOLD)
    }

    pub fn bold(&self) -> Style {
        Style::default()
            .fg(self.fg)
            .bg(self.bg)
            .add_modifier(Modifier::BOLD)
    }

    pub fn success(&self) -> Style {
        Style::default().fg(self.green).bg(self.bg)
    }

    pub fn warn(&self) -> Style {
        Style::default().fg(self.gold).bg(self.bg)
    }

    pub fn error(&self) -> Style {
        Style::default().fg(self.red).bg(self.bg)
    }

    pub fn info(&self) -> Style {
        Style::default().fg(self.info).bg(self.bg)
    }

    pub fn user_band(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg_light)
    }

    pub fn code(&self) -> Style {
        Style::default().fg(self.cyan).bg(self.bg_dark)
    }

    pub fn inverted_accent(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg_light)
    }

    pub fn selected(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg_sel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_matches_captured_sgr() {
        let t = Theme::night();
        assert_eq!(t.bg, Color::Rgb(20, 20, 20));
        assert_eq!(t.fg, Color::Rgb(225, 225, 225));
        assert_eq!(t.prompt_border, Color::Rgb(80, 80, 88));
        assert_eq!(t.border, Color::Rgb(51, 51, 51));
        assert_eq!(t.gold, Color::Rgb(224, 175, 104));
        assert_eq!(t.cursor, Color::Rgb(200, 200, 200));
        assert_eq!(t.bg_sel, Color::Rgb(54, 54, 54));
        assert_eq!(t.fg_mute, Color::Rgb(88, 88, 88));
    }
}
