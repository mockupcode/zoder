use std::time::Duration;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;

use super::{step_index, App, Focus, Overlay, Residue};
use crate::slash;
use crate::text::{csi_final, csi_param};

/// How long an `ESC` keeps the residue filter armed.
const RESIDUE_WINDOW: Duration = Duration::from_millis(250);

impl App {
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return;
        }
        if key.code == KeyCode::Esc {
            self.residue = Some(Residue::arm());
        } else if self.eat_residue(&key) {
            return;
        }
        self.touch();
        if self.running && key.code == KeyCode::Esc {
            self.cancel_turn();
            return;
        }
        if matches!(self.overlay, Overlay::None) {
            self.handle_global_or_main(key);
        } else {
            self.handle_overlay(key);
        }
    }

    fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
    }

    /// Swallow `ESC [ … M` tails that leak in as ordinary characters after a
    /// lone ESC was consumed on its own.
    fn eat_residue(&mut self, key: &KeyEvent) -> bool {
        let Some(res) = self.residue.as_mut() else {
            return false;
        };
        if res.started.elapsed() > RESIDUE_WINDOW {
            self.residue = None;
            return false;
        }
        let KeyCode::Char(c) = key.code else {
            self.residue = None;
            return false;
        };
        if res.in_seq {
            if csi_param(c) {
                return true;
            }
            self.residue = None;
            return csi_final(c);
        }
        if matches!(c, '[' | 'O') {
            res.in_seq = true;
            true
        } else {
            self.residue = None;
            false
        }
    }

    pub(super) fn handle_global_or_main(&mut self, key: KeyEvent) {
        if let Some(l) = ctrl_letter(&key) {
            match l {
                'q' | 'd' => {
                    self.ctrl_q();
                    return;
                }
                'c' => {
                    self.ctrl_c();
                    return;
                }
                'n' => {
                    self.ctrl_n();
                    return;
                }
                'r' => {
                    self.open_sessions();
                    return;
                }
                't' => {
                    self.show_todos = !self.show_todos;
                    return;
                }
                'm' => {
                    if self.focus == Focus::Prompt {
                        self.composer.multiline = !self.composer.multiline;
                        self.toast(if self.composer.multiline {
                            "multiline on — Enter inserts a line, send with Alt+Enter"
                        } else {
                            "multiline off"
                        });
                    } else {
                        self.open_models();
                    }
                    return;
                }
                's' => {
                    self.composer.stash_or_pop();
                    return;
                }
                'x' | '?' => {
                    self.open_help();
                    return;
                }
                'l' => {
                    return;
                }
                'k' => {
                    self.scroll_transcript(1);
                    return;
                }
                'j' => {
                    self.scroll_transcript(-1);
                    return;
                }
                'u' => {
                    let half = (self.layout.get().body.height / 2).max(1);
                    self.scroll_transcript(half as i16);
                    return;
                }
                _ => {}
            }
        }

        if ascii_char(&key) == Some('?') && self.composer.is_empty() {
            self.open_help();
            return;
        }

        match key.code {
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Prompt => Focus::Scrollback,
                    Focus::Scrollback if self.show_todos => Focus::Todos,
                    Focus::Scrollback | Focus::Todos => Focus::Prompt,
                };
            }
            KeyCode::Esc => self.on_esc(),
            KeyCode::Enter => self.on_enter(key.modifiers),
            KeyCode::Backspace => {
                if self.focus == Focus::Prompt {
                    self.composer.backspace();
                }
            }
            KeyCode::Delete => {
                if self.focus == Focus::Prompt {
                    self.composer.delete();
                }
            }
            KeyCode::Left => {
                if self.focus == Focus::Prompt {
                    if self.at_pop_level() {
                        return;
                    }
                    self.composer.left();
                } else {
                    self.fold_selected(true);
                }
            }
            KeyCode::Right => {
                if self.focus == Focus::Prompt {
                    if self.at_enter_level() {
                        return;
                    }
                    self.composer.right();
                } else {
                    self.fold_selected(false);
                }
            }
            KeyCode::Home => self.composer.home(),
            KeyCode::End => self.composer.end(),
            KeyCode::Up => self.on_up(),
            KeyCode::Down => self.on_down(),
            KeyCode::PageUp => {
                let page = self.layout.get().body.height.max(1);
                self.scroll_transcript(page as i16);
            }
            KeyCode::PageDown => {
                let page = self.layout.get().body.height.max(1);
                self.scroll_transcript(-(page as i16));
            }
            KeyCode::Char(c) => {
                if c.is_control() || key.modifiers.contains(KeyModifiers::CONTROL) {
                    return;
                }
                if self.focus != Focus::Prompt {
                    self.focus = Focus::Prompt;
                }
                self.composer.insert(c);
                self.slash_sel = 0;
                self.file_sel = 0;
            }
            _ => {}
        }
    }

    pub(super) fn handle_overlay(&mut self, key: KeyEvent) {
        if let Some(l) = ctrl_letter(&key) {
            match l {
                'q' => {
                    self.ctrl_q();
                    return;
                }
                'c' => {
                    self.close_overlay();
                    return;
                }
                _ => {}
            }
        }
        if self.overlay.is_sessions() {
            self.handle_sessions_key(key);
            return;
        }
        match &mut self.overlay {
            Overlay::None | Overlay::Sessions(_) => {}
            Overlay::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter)
                    || matches!(ascii_char(&key), Some('q' | '?'))
                {
                    self.overlay = Overlay::None;
                }
            }
            Overlay::QuitConfirm => match key.code {
                KeyCode::Enter => self.should_quit = true,
                _ if matches!(ascii_char(&key), Some('q' | 'y')) => self.should_quit = true,
                _ => self.overlay = Overlay::None,
            },
            Overlay::NewConfirm => match key.code {
                KeyCode::Enter => {
                    self.overlay = Overlay::None;
                    self.new_session();
                }
                _ if matches!(ascii_char(&key), Some('n' | 'y')) => {
                    self.overlay = Overlay::None;
                    self.new_session();
                }
                _ => self.overlay = Overlay::None,
            },
            Overlay::Models { items, selected } => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => step_index(selected, items.len(), false),
                KeyCode::Down => step_index(selected, items.len(), true),
                KeyCode::Enter => {
                    if let Some(m) = items.get(*selected).cloned() {
                        self.cfg.set_model(m.clone());
                        self.client.model = m.clone();
                        self.session.model = m;
                        self.toast("model updated");
                    }
                    self.overlay = Overlay::None;
                }
                _ => {}
            },
            Overlay::Question {
                options,
                selected,
                draft,
                ..
            } => {
                if options.is_empty() {
                    match key.code {
                        KeyCode::Esc => self.answer_question("cancelled"),
                        KeyCode::Enter => {
                            let t = draft.clone();
                            self.answer_question(t);
                        }
                        KeyCode::Backspace => {
                            draft.pop();
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            draft.push(c);
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Esc => self.answer_question("cancelled"),
                        KeyCode::Up => step_index(selected, options.len(), false),
                        KeyCode::Down => step_index(selected, options.len(), true),
                        KeyCode::Enter => {
                            let ans = options
                                .get(*selected)
                                .cloned()
                                .unwrap_or_else(|| "cancelled".into());
                            self.answer_question(ans);
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() => {
                            let n = c.to_digit(10).unwrap_or(0) as usize;
                            if n >= 1 && n <= options.len() {
                                let ans = options[n - 1].clone();
                                self.answer_question(ans);
                            }
                        }
                        _ if matches!(ascii_char(&key), Some('y'))
                            && options.iter().any(|o| o == "yes") =>
                        {
                            self.answer_question("yes");
                        }
                        _ if matches!(ascii_char(&key), Some('n'))
                            && options.iter().any(|o| o == "no") =>
                        {
                            self.answer_question("no");
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    pub(crate) fn answer_question(&mut self, ans: impl Into<String>) {
        if let Overlay::Question { reply: Some(r), .. } =
            std::mem::replace(&mut self.overlay, Overlay::None)
        {
            let _ = r.send(ans.into());
        }
    }

    pub(super) fn ctrl_q(&mut self) {
        if self.chord_q.hit(Duration::from_millis(1000)) {
            self.should_quit = true;
        } else {
            self.overlay = Overlay::QuitConfirm;
            self.toast("press again to quit");
        }
    }

    pub(super) fn ctrl_n(&mut self) {
        if self.chord_n.hit(Duration::from_millis(1000)) {
            self.close_overlay();
            self.new_session();
        } else {
            self.overlay = Overlay::NewConfirm;
            self.toast("press again for a new session");
        }
    }

    pub(super) fn ctrl_c(&mut self) {
        if !self.composer.is_empty() {
            self.composer.clear();
            return;
        }
        if self.running {
            self.cancel_turn();
            return;
        }
        self.toast("press ctrl+q twice to quit");
    }

    pub(super) fn on_esc(&mut self) {
        if self.running {
            self.cancel_turn();
            return;
        }
        if self.chord_esc.hit(Duration::from_millis(800)) {
            if !self.composer.is_empty() {
                self.composer.stash_or_pop();
                if !self.composer.is_empty() {
                    self.composer.clear();
                }
            }
        } else if !self.composer.is_empty() {
            self.toast("press again to clear");
        }
    }

    pub(super) fn on_enter(&mut self, mods: KeyModifiers) {
        if self.focus != Focus::Prompt {
            return;
        }
        let newline_chord = mods.contains(KeyModifiers::SHIFT)
            || mods.contains(KeyModifiers::ALT)
            || mods.contains(KeyModifiers::SUPER);
        if self.composer.multiline {
            if newline_chord {
                self.submit();
            } else {
                self.composer.newline();
            }
            return;
        }
        if newline_chord {
            self.composer.newline();
            return;
        }
        if self.composer.text.ends_with('\\') {
            self.composer.backspace();
            self.composer.newline();
            return;
        }
        self.submit();
    }

    fn handle_sessions_key(&mut self, key: KeyEvent) {
        enum Act {
            Close,
            Resume(usize),
            Delete(usize),
            None,
        }
        let n = self.picker_sessions().len();
        let Some(s) = self.overlay.sessions_mut() else {
            return;
        };
        let letter = ascii_char(&key);
        let act = if s.confirm_delete {
            match key.code {
                KeyCode::Enter => Act::Delete(s.selected),
                KeyCode::Esc => {
                    s.confirm_delete = false;
                    Act::None
                }
                _ if letter == Some('y') => Act::Delete(s.selected),
                _ if letter == Some('n') => {
                    s.confirm_delete = false;
                    Act::None
                }
                _ => Act::None,
            }
        } else {
            match key.code {
                KeyCode::Esc => {
                    if s.searching || !s.query.is_empty() {
                        s.query.clear();
                        s.searching = false;
                        s.selected = 0;
                        Act::None
                    } else {
                        Act::Close
                    }
                }
                KeyCode::Up => {
                    s.step(n, false);
                    Act::None
                }
                KeyCode::Down => {
                    s.step(n, true);
                    Act::None
                }
                KeyCode::Enter => Act::Resume(s.selected),
                KeyCode::Backspace if s.searching || !s.query.is_empty() => {
                    s.query.pop();
                    s.selected = 0;
                    if s.query.is_empty() {
                        s.searching = false;
                    }
                    Act::None
                }
                KeyCode::Char(c) => {
                    let cmd = crate::layout::to_latin(c).unwrap_or_else(|| c.to_ascii_lowercase());
                    if !s.searching && cmd == '/' {
                        s.searching = true;
                        Act::None
                    } else if !s.searching && s.query.is_empty() && cmd == 'e' {
                        s.expanded = !s.expanded;
                        s.selected = 0;
                        Act::None
                    } else if !s.searching && s.query.is_empty() && cmd == 'f' {
                        s.filter_cwd = !s.filter_cwd;
                        s.selected = 0;
                        Act::None
                    } else if !s.searching && s.query.is_empty() && cmd == 'd' {
                        if n > 0 {
                            s.confirm_delete = true;
                        }
                        Act::None
                    } else {
                        s.searching = true;
                        s.query.push(c);
                        s.selected = 0;
                        Act::None
                    }
                }
                _ => Act::None,
            }
        };
        match act {
            Act::Close => self.close_overlay(),
            Act::Resume(i) => self.resume_selected(i),
            Act::Delete(i) => self.delete_selected(i),
            Act::None => {}
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        if matches!(ev.kind, MouseEventKind::Moved) {
            return;
        }
        self.touch();
        if !matches!(self.overlay, Overlay::None | Overlay::Sessions(_))
            && matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        {
            let pos = Position::new(ev.column, ev.row);
            if self.close_hit.get().is_some_and(|r| r.contains(pos)) {
                if matches!(self.overlay, Overlay::Question { .. }) {
                    self.answer_question("cancelled");
                } else {
                    self.close_overlay();
                }
                return;
            }
        }
        if self.overlay.is_sessions() {
            self.mouse_sessions(ev);
            return;
        }
        let has_files = self.at_entries().is_some_and(|c| !c.is_empty());
        let has_slash = self.slash_items().is_some_and(|c| !c.is_empty());
        if (has_files && !has_slash) || has_slash {
            self.mouse_dropdown(ev, has_files && !has_slash);
            return;
        }
        match ev.kind {
            MouseEventKind::ScrollUp => self.scroll_transcript(1),
            MouseEventKind::ScrollDown => self.scroll_transcript(-1),
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = Position::new(ev.column, ev.row);
                if self
                    .layout
                    .get()
                    .arrow_down
                    .is_some_and(|r| r.contains(pos))
                {
                    self.follow = true;
                    self.scroll.set(0);
                }
            }
            _ => {}
        }
    }

    fn mouse_sessions(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::ScrollUp => {
                let n = self.picker_sessions().len();
                if let Some(s) = self.overlay.sessions_mut() {
                    s.step(n, false);
                }
            }
            MouseEventKind::ScrollDown => {
                let n = self.picker_sessions().len();
                if let Some(s) = self.overlay.sessions_mut() {
                    s.step(n, true);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = Position::new(ev.column, ev.row);
                if self.close_hit.get().is_some_and(|r| r.contains(pos)) {
                    self.close_overlay();
                    return;
                }
                if let Some(i) = self.pick_index(pos) {
                    self.resume_selected(i);
                }
            }
            _ => {}
        }
    }

    fn mouse_dropdown(&mut self, ev: MouseEvent, files: bool) {
        match ev.kind {
            MouseEventKind::ScrollUp => self.on_up(),
            MouseEventKind::ScrollDown => self.on_down(),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(i) = self.pick_index(Position::new(ev.column, ev.row)) {
                    if files {
                        self.file_sel = i;
                    } else {
                        self.slash_sel = i;
                    }
                }
            }
            _ => {}
        }
    }

    fn pick_index(&self, pos: Position) -> Option<usize> {
        let hits = self.pick_hits.take();
        let hit = hits.iter().find(|(r, _)| r.contains(pos)).map(|(_, i)| *i);
        self.pick_hits.set(hits);
        hit
    }

    pub(super) fn scroll_transcript(&mut self, delta: i16) {
        let max = self.layout.get().max_scroll;
        if max == 0 {
            self.scroll.set(0);
            if delta < 0 {
                self.follow = true;
            }
            return;
        }
        if delta > 0 {
            self.follow = false;
            let next = self.scroll.get().saturating_add(delta as u16);
            self.scroll.set(next.min(max));
        } else {
            self.scroll
                .set(self.scroll.get().saturating_sub((-delta) as u16));
            if self.scroll.get() == 0 {
                self.follow = true;
            }
        }
    }

    pub(super) fn on_up(&mut self) {
        if self.focus == Focus::Prompt {
            if self.nav_dropdown(false) {
                return;
            }
            if self.composer.is_empty() || self.composer.history_idx.is_some() {
                self.composer.history_up();
            }
            return;
        }
        self.scroll_transcript(1);
        step_index(&mut self.selected_block, self.session.blocks.len(), false);
    }

    pub(super) fn on_down(&mut self) {
        if self.focus == Focus::Prompt {
            if self.nav_dropdown(true) {
                return;
            }
            self.composer.history_down();
            return;
        }
        self.scroll_transcript(-1);
        step_index(&mut self.selected_block, self.session.blocks.len(), true);
    }

    fn nav_dropdown(&mut self, down: bool) -> bool {
        if let Some(cmds) = self.slash_items() {
            if !cmds.is_empty() {
                step_index(&mut self.slash_sel, cmds.len(), down);
                return true;
            }
        }
        if let Some(files) = self.at_entries() {
            if !files.is_empty() {
                step_index(&mut self.file_sel, files.len(), down);
                return true;
            }
        }
        false
    }

    pub(super) fn fold_selected(&mut self, collapse: bool) {
        if let Some(b) = self.session.blocks.get_mut(self.selected_block) {
            b.set_folded(collapse);
        }
    }

    pub fn slash_items(&self) -> Option<Vec<&'static slash::Command>> {
        self.composer.slash_query().map(slash::matches)
    }

    pub fn at_entries(&self) -> Option<Vec<crate::tools::FileHit>> {
        let q = self.composer.at_query()?;
        Some(self.at_cache.borrow_mut().hits(&self.session.cwd, q))
    }

    fn at_enter_level(&mut self) -> bool {
        self.confirm_at(false)
    }

    pub(super) fn confirm_at(&mut self, insert_file: bool) -> bool {
        let Some(files) = self.at_entries() else {
            return false;
        };
        if files.is_empty() {
            return false;
        }
        let i = self.file_sel.min(files.len() - 1);
        if files[i].is_dir {
            self.composer.replace_at_query(&files[i].rel);
            self.file_sel = 0;
            true
        } else if insert_file {
            let pick = files[i].rel.clone();
            self.composer.replace_at_query(&format!("{pick} "));
            true
        } else {
            false
        }
    }

    fn at_pop_level(&mut self) -> bool {
        let Some(q) = self.composer.at_query() else {
            return false;
        };
        if q.is_empty() {
            return false;
        }
        let bang = q.starts_with('!');
        let rest = q.trim_start_matches('!');
        let parent = {
            let trimmed = rest.trim_end_matches('/');
            match trimmed.rfind('/') {
                Some(i) => trimmed[..=i].to_string(),
                None => String::new(),
            }
        };
        if bang {
            self.composer.replace_at_query(&format!("!{parent}"));
        } else {
            self.composer.replace_at_query(&parent);
        }
        self.file_sel = 0;
        true
    }
}

/// Ctrl chord as a Latin letter, independent of input language.
/// Terminals send either `Ctrl+c` / `Ctrl+C` or the C0 byte (`\x03`).
fn ctrl_letter(key: &KeyEvent) -> Option<char> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if matches!(c, '\t' | '\n' | '\r') {
        return None;
    }
    let n = c as u32;
    if (1..27).contains(&n) {
        return Some((b'a' + n as u8 - 1) as char);
    }
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    if c.is_ascii_alphabetic() {
        Some(c.to_ascii_lowercase())
    } else if c == '?' {
        Some('?')
    } else {
        crate::layout::to_latin(c)
    }
}

fn ascii_char(key: &KeyEvent) -> Option<char> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    if let Some(l) = crate::layout::to_latin(c) {
        return Some(l);
    }
    if c.is_ascii() {
        Some(c.to_ascii_lowercase())
    } else {
        None
    }
}
