use unicode_segmentation::UnicodeSegmentation;

use crate::text::{byte_at_grapheme, grapheme_at_byte, grapheme_len};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftKind {
    Chat,
    Shell,
}

#[derive(Debug, Clone)]
pub struct Composer {
    pub text: String,
    pub cursor: usize,
    pub kind: DraftKind,
    pub multiline: bool,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub stash: Option<String>,
    pub hint: String,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            kind: DraftKind::Chat,
            multiline: false,
            history: Vec::new(),
            history_idx: None,
            stash: None,
            hint: String::new(),
        }
    }
}

impl Composer {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert(&mut self, ch: char) {
        if self.text.is_empty() && ch == '!' {
            self.kind = DraftKind::Shell;
            return;
        }
        let i = self.cursor.min(self.text.len());
        self.text.insert(i, ch);
        self.cursor = i + ch.len_utf8();
        self.history_idx = None;
        self.hint.clear();
    }

    pub fn insert_str(&mut self, s: &str) {
        let i = self.cursor.min(self.text.len());
        self.text.insert_str(i, s);
        self.cursor = i + s.len();
        self.history_idx = None;
    }

    pub fn newline(&mut self) {
        self.insert('\n');
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            if self.kind == DraftKind::Shell && self.text.is_empty() {
                self.kind = DraftKind::Chat;
            }
            return;
        }
        let g = grapheme_at_byte(&self.text, self.cursor).saturating_sub(1);
        let start = byte_at_grapheme(&self.text, g);
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let g = grapheme_at_byte(&self.text, self.cursor);
        let end = byte_at_grapheme(&self.text, g + 1).min(self.text.len());
        self.text.drain(self.cursor..end);
    }

    pub fn left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let g = grapheme_at_byte(&self.text, self.cursor).saturating_sub(1);
        self.cursor = byte_at_grapheme(&self.text, g);
    }

    pub fn right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let g = grapheme_at_byte(&self.text, self.cursor) + 1;
        self.cursor = byte_at_grapheme(&self.text, g).min(self.text.len());
    }

    pub fn home(&mut self) {
        let line_start = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.cursor = line_start;
    }

    pub fn end(&mut self) {
        let rest = &self.text[self.cursor..];
        self.cursor += rest.find('\n').unwrap_or(rest.len());
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.kind = DraftKind::Chat;
        self.history_idx = None;
        self.hint.clear();
    }

    pub fn take(&mut self) -> String {
        let out = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.kind = DraftKind::Chat;
        self.history_idx = None;
        if !out.trim().is_empty() && self.history.last().map(|s| s != &out).unwrap_or(true) {
            self.history.push(out.clone());
        }
        out
    }

    pub fn stash_or_pop(&mut self) {
        if self.text.is_empty() {
            if let Some(s) = self.stash.take() {
                self.text = s;
                self.cursor = self.text.len();
            }
        } else {
            self.stash = Some(std::mem::take(&mut self.text));
            self.cursor = 0;
        }
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_idx {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_idx = Some(next);
        self.text = self.history[next].clone();
        self.cursor = self.text.len();
    }

    pub fn history_down(&mut self) {
        let Some(i) = self.history_idx else {
            return;
        };
        if i + 1 >= self.history.len() {
            self.history_idx = None;
            self.text.clear();
            self.cursor = 0;
            return;
        }
        self.history_idx = Some(i + 1);
        self.text = self.history[i + 1].clone();
        self.cursor = self.text.len();
    }

    pub fn slash_query(&self) -> Option<&str> {
        if self.kind != DraftKind::Chat {
            return None;
        }
        if self.text.starts_with('/') && !self.text.contains('\n') {
            Some(self.text.trim_start_matches('/'))
        } else {
            None
        }
    }

    pub fn replace_at_query(&mut self, new_q: &str) {
        let Some(at) = self.text[..self.cursor].rfind('@') else {
            return;
        };
        let rest = &self.text[at + 1..self.cursor];
        let bang = if rest.starts_with('!') { "!" } else { "" };
        let insert = format!("@{bang}{new_q}");
        self.text.replace_range(at..self.cursor, &insert);
        self.cursor = at + insert.len();
    }

    pub fn at_query(&self) -> Option<&str> {
        if self.kind != DraftKind::Chat {
            return None;
        }
        let before = &self.text[..self.cursor];
        let at = before.rfind('@')?;
        let q = &before[at + 1..];
        if q.chars().any(|c| c.is_whitespace()) {
            None
        } else {
            Some(q)
        }
    }

    pub fn visual_cursor_col(&self, width: usize) -> (u16, u16) {
        let prefix = &self.text[..self.cursor.min(self.text.len())];
        let mut row = 0u16;
        let mut col = 0u16;
        let w = width.max(1);
        for g in prefix.graphemes(true) {
            if g == "\n" {
                row = row.saturating_add(1);
                col = 0;
                continue;
            }
            let gw = unicode_width::UnicodeWidthStr::width(g).max(1) as u16;
            if col as usize + gw as usize >= w {
                row = row.saturating_add(1);
                col = 0;
            }
            col = col.saturating_add(gw);
        }
        let _ = grapheme_len(&self.text);
        (col, row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bang_enters_shell_kind() {
        let mut c = Composer::default();
        c.insert('!');
        assert_eq!(c.kind, DraftKind::Shell);
        assert!(c.text.is_empty());
        c.insert('l');
        c.insert('s');
        assert_eq!(c.text, "ls");
    }

    #[test]
    fn take_records_history() {
        let mut c = Composer::default();
        c.insert_str("hello");
        let got = c.take();
        assert_eq!(got, "hello");
        assert!(c.is_empty());
        assert_eq!(c.history, vec!["hello".to_string()]);
    }
}
