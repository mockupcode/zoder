use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;

use super::{App, Focus, Overlay};
use crate::session::AgentMode;
use crate::slash;
use crate::tools;

impl App {
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return;
        }
        if matches!(self.overlay, Overlay::None) {
            self.handle_global_or_main(key);
        } else {
            self.handle_overlay(key);
        }
    }

    pub(super) fn handle_global_or_main(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if ctrl {
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('d') => {
                    self.ctrl_q();
                    return;
                }
                KeyCode::Char('c') => {
                    self.ctrl_c();
                    return;
                }
                KeyCode::Char('n') => {
                    self.ctrl_n();
                    return;
                }
                KeyCode::Char('r') => {
                    self.open_sessions();
                    return;
                }
                KeyCode::Char('t') => {
                    self.show_todos = !self.show_todos;
                    return;
                }
                KeyCode::Char('o') => {
                    self.cycle_always();
                    return;
                }
                KeyCode::Char('m') => {
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
                KeyCode::Char('s') => {
                    self.composer.stash_or_pop();
                    return;
                }
                KeyCode::Char('x') | KeyCode::Char('?') => {
                    self.overlay = Overlay::Help;
                    return;
                }
                KeyCode::Char('l') => {
                    /* reserved */
                    return;
                }
                KeyCode::Char('k') => {
                    self.scroll_transcript(1);
                    return;
                }
                KeyCode::Char('j') => {
                    self.scroll_transcript(-1);
                    return;
                }
                KeyCode::Char('u') => {
                    let half = (self.layout.get().body.height / 2).max(1);
                    self.scroll_transcript(half as i16);
                    return;
                }
                _ => {}
            }
        }

        if key.code == KeyCode::Char('?') && self.composer.is_empty() {
            self.overlay = Overlay::Help;
            return;
        }

        match key.code {
            KeyCode::Tab if shift => self.cycle_mode(),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Prompt => Focus::Scrollback,
                    Focus::Scrollback if self.show_todos => Focus::Todos,
                    Focus::Scrollback | Focus::Todos => Focus::Prompt,
                };
            }
            KeyCode::BackTab => self.cycle_mode(),
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
                if shift && c == '\t' {
                    self.cycle_mode();
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
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('c'))
        {
            if matches!(key.code, KeyCode::Char('q')) {
                self.ctrl_q();
            } else {
                self.overlay = Overlay::None;
            }
            return;
        }
        match &mut self.overlay {
            Overlay::None => {}
            Overlay::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?')
                ) {
                    self.overlay = Overlay::None;
                }
            }
            Overlay::QuitConfirm => match key.code {
                KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('y') => self.should_quit = true,
                _ => self.overlay = Overlay::None,
            },
            Overlay::NewConfirm => match key.code {
                KeyCode::Char('n') | KeyCode::Enter | KeyCode::Char('y') => {
                    self.overlay = Overlay::None;
                    self.new_session();
                }
                _ => self.overlay = Overlay::None,
            },
            Overlay::Sessions { .. } => self.handle_sessions_key(key),
            Overlay::Models { items, selected } => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    if *selected + 1 < items.len() {
                        *selected += 1;
                    }
                }
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
            Overlay::Permission { selected, .. } => match key.code {
                KeyCode::Up | KeyCode::BackTab => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => {
                    if *selected < 1 {
                        *selected += 1;
                    }
                }
                KeyCode::Char('1') | KeyCode::Enter if *selected == 0 => self.answer_perm(true),
                KeyCode::Char('2') | KeyCode::Enter if *selected == 1 => self.answer_perm(false),
                KeyCode::Char('y') | KeyCode::Char('a') => self.answer_perm(true),
                KeyCode::Char('n') | KeyCode::Char('d') | KeyCode::Esc => self.answer_perm(false),
                _ => {}
            },
        }
    }

    pub(crate) fn answer_perm(&mut self, allow: bool) {
        if let Overlay::Permission { reply: Some(r), .. } =
            std::mem::replace(&mut self.overlay, Overlay::None)
        {
            let _ = r.send(allow);
        }
    }

    pub(super) fn ctrl_q(&mut self) {
        let now = Instant::now();
        if self
            .last_ctrl_q
            .map(|t| now.duration_since(t) < Duration::from_millis(1000))
            .unwrap_or(false)
        {
            self.should_quit = true;
        } else {
            self.last_ctrl_q = Some(now);
            self.overlay = Overlay::QuitConfirm;
            self.toast("press again to quit");
        }
    }

    pub(super) fn ctrl_n(&mut self) {
        let now = Instant::now();
        if self
            .last_ctrl_n
            .map(|t| now.duration_since(t) < Duration::from_millis(1000))
            .unwrap_or(false)
        {
            self.overlay = Overlay::None;
            self.new_session();
        } else {
            self.last_ctrl_n = Some(now);
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
            self.cancel.store(true, Ordering::Relaxed);
            self.toast("cancelling turn");
            return;
        }
        self.toast("press ctrl+q twice to quit");
    }

    pub(super) fn on_esc(&mut self) {
        if self.running {
            self.toast("press ctrl+c to cancel the turn");
            return;
        }
        let now = Instant::now();
        if self
            .last_esc
            .map(|t| now.duration_since(t) < Duration::from_millis(800))
            .unwrap_or(false)
        {
            if !self.composer.is_empty() {
                self.composer.stash_or_pop();
                if !self.composer.is_empty() {
                    self.composer.clear();
                }
            }
            self.last_esc = None;
        } else {
            self.last_esc = Some(now);
            if !self.composer.is_empty() {
                self.toast("press again to clear");
            }
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
        let act = {
            let Overlay::Sessions {
                query,
                selected,
                confirm_delete,
                expanded,
                filter_cwd,
                searching,
            } = &mut self.overlay
            else {
                return;
            };
            if *confirm_delete {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => Act::Delete(*selected),
                    KeyCode::Char('n') | KeyCode::Esc => {
                        *confirm_delete = false;
                        Act::None
                    }
                    _ => Act::None,
                }
            } else {
                match key.code {
                    KeyCode::Esc => {
                        if *searching || !query.is_empty() {
                            query.clear();
                            *searching = false;
                            *selected = 0;
                            Act::None
                        } else {
                            Act::Close
                        }
                    }
                    KeyCode::Up => {
                        *selected = selected.saturating_sub(1);
                        Act::None
                    }
                    KeyCode::Down => {
                        if n > 0 {
                            *selected = (*selected + 1).min(n - 1);
                        }
                        Act::None
                    }
                    KeyCode::Enter => Act::Resume(*selected),
                    KeyCode::Backspace if *searching || !query.is_empty() => {
                        query.pop();
                        *selected = 0;
                        if query.is_empty() {
                            *searching = false;
                        }
                        Act::None
                    }
                    KeyCode::Char('/') if !*searching => {
                        *searching = true;
                        Act::None
                    }
                    KeyCode::Char('e') if !*searching && query.is_empty() => {
                        *expanded = !*expanded;
                        *selected = 0;
                        Act::None
                    }
                    KeyCode::Char('f') if !*searching && query.is_empty() => {
                        *filter_cwd = !*filter_cwd;
                        *selected = 0;
                        Act::None
                    }
                    KeyCode::Char('d') if !*searching && query.is_empty() => {
                        if n > 0 {
                            *confirm_delete = true;
                        }
                        Act::None
                    }
                    KeyCode::Char(c) => {
                        *searching = true;
                        query.push(c);
                        *selected = 0;
                        Act::None
                    }
                    _ => Act::None,
                }
            }
        };
        match act {
            Act::Close => self.overlay = Overlay::None,
            Act::Resume(i) => self.resume_selected(i),
            Act::Delete(i) => self.delete_selected(i),
            Act::None => {}
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        if !matches!(self.overlay, Overlay::None | Overlay::Sessions { .. })
            && matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        {
            let pos = Position::new(ev.column, ev.row);
            if self.close_hit.get().is_some_and(|r| r.contains(pos)) {
                if matches!(self.overlay, Overlay::Permission { .. }) {
                    self.answer_perm(false);
                } else {
                    self.overlay = Overlay::None;
                }
                return;
            }
        }
        if matches!(self.overlay, Overlay::Sessions { .. }) {
            match ev.kind {
                MouseEventKind::ScrollUp => {
                    if let Overlay::Sessions { selected, .. } = &mut self.overlay {
                        *selected = selected.saturating_sub(1);
                    }
                }
                MouseEventKind::ScrollDown => {
                    let n = self.picker_sessions().len();
                    if let Overlay::Sessions { selected, .. } = &mut self.overlay {
                        if n > 0 {
                            *selected = (*selected + 1).min(n - 1);
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let pos = Position::new(ev.column, ev.row);
                    if let Some(r) = self.close_hit.get() {
                        if r.contains(pos) {
                            self.overlay = Overlay::None;
                            return;
                        }
                    }
                    let hits = self.pick_hits.take();
                    let hit = hits.iter().find(|(r, _)| r.contains(pos)).map(|(_, i)| *i);
                    self.pick_hits.set(hits);
                    if let Some(i) = hit {
                        self.resume_selected(i);
                    }
                }
                _ => {}
            }
            return;
        }
        if self.at_entries().is_some_and(|c| !c.is_empty())
            && !self.slash_items().is_some_and(|c| !c.is_empty())
        {
            match ev.kind {
                MouseEventKind::ScrollUp => self.on_up(),
                MouseEventKind::ScrollDown => self.on_down(),
                MouseEventKind::Down(MouseButton::Left) => {
                    let pos = Position::new(ev.column, ev.row);
                    let hits = self.pick_hits.take();
                    let hit = hits.iter().find(|(r, _)| r.contains(pos)).map(|(_, i)| *i);
                    self.pick_hits.set(hits);
                    if let Some(i) = hit {
                        self.file_sel = i;
                    }
                }
                _ => {}
            }
            return;
        }
        if self.slash_items().is_some_and(|c| !c.is_empty()) {
            match ev.kind {
                MouseEventKind::ScrollUp => self.on_up(),
                MouseEventKind::ScrollDown => self.on_down(),
                MouseEventKind::Down(MouseButton::Left) => {
                    let pos = Position::new(ev.column, ev.row);
                    let hits = self.pick_hits.take();
                    let hit = hits.iter().find(|(r, _)| r.contains(pos)).map(|(_, i)| *i);
                    self.pick_hits.set(hits);
                    if let Some(i) = hit {
                        self.slash_sel = i;
                    }
                }
                _ => {}
            }
            return;
        }
        match ev.kind {
            MouseEventKind::ScrollUp => self.scroll_transcript(1),
            MouseEventKind::ScrollDown => self.scroll_transcript(-1),
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = Position::new(ev.column, ev.row);
                if let Some(r) = self.layout.get().arrow_down {
                    if r.contains(pos) {
                        self.follow = true;
                        self.scroll = 0;
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn scroll_transcript(&mut self, delta: i16) {
        let max = self.layout.get().max_scroll;
        if delta > 0 {
            self.follow = false;
            let next = self.scroll.saturating_add(delta as u16);
            self.scroll = if max == 0 { next } else { next.min(max) };
        } else {
            self.scroll = self.scroll.saturating_sub((-delta) as u16);
            if self.scroll == 0 {
                self.follow = true;
            }
        }
    }

    pub(super) fn on_up(&mut self) {
        if self.focus == Focus::Prompt {
            if let Some(cmds) = self.slash_items() {
                if !cmds.is_empty() {
                    if self.slash_sel > 0 {
                        self.slash_sel -= 1;
                    }
                    return;
                }
            }
            if self.at_entries().is_some_and(|e| !e.is_empty()) {
                if self.file_sel > 0 {
                    self.file_sel -= 1;
                }
                return;
            }
            if self.composer.is_empty() || self.composer.history_idx.is_some() {
                self.composer.history_up();
            }
            return;
        }
        self.scroll_transcript(1);
        if self.selected_block > 0 {
            self.selected_block -= 1;
        }
    }

    pub(super) fn on_down(&mut self) {
        if self.focus == Focus::Prompt {
            if let Some(cmds) = self.slash_items() {
                if !cmds.is_empty() {
                    if self.slash_sel + 1 < cmds.len() {
                        self.slash_sel += 1;
                    }
                    return;
                }
            }
            if let Some(files) = self.at_entries() {
                if self.file_sel + 1 < files.len() {
                    self.file_sel += 1;
                }
                return;
            }
            self.composer.history_down();
            return;
        }
        self.scroll_transcript(-1);
        if self.selected_block + 1 < self.session.blocks.len() {
            self.selected_block += 1;
        }
    }

    pub(super) fn fold_selected(&mut self, collapse: bool) {
        if let Some(b) = self.session.blocks.get_mut(self.selected_block) {
            b.set_folded(collapse);
        }
    }

    pub(super) fn cycle_mode(&mut self) {
        self.session.mode = self.session.mode.next();
        let label = self.session.mode.label();
        self.toast(format!("mode {label}"));
        let _ = self.session.save();
    }

    pub(super) fn cycle_always(&mut self) {
        if self.session.mode == AgentMode::Always {
            self.session.mode = AgentMode::Normal;
        } else {
            self.session.mode = AgentMode::Always;
        }
        self.toast(format!("mode {}", self.session.mode.label()));
    }

    pub fn slash_items(&self) -> Option<Vec<&'static slash::Command>> {
        self.composer.slash_query().map(slash::matches)
    }

    pub fn at_entries(&self) -> Option<Vec<tools::FileHit>> {
        self.composer
            .at_query()
            .map(|q| tools::list_at_level(&self.session.cwd, q, 200))
    }

    fn at_enter_level(&mut self) -> bool {
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
