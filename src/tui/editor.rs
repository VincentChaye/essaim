use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

/// Petit tampon de texte éditable (une ou plusieurs lignes). Colonnes en caractères.
#[derive(Debug, Clone, Default)]
pub struct TextBuf {
    lines: Vec<String>,
    row: usize,
    col: usize,
    pub multiline: bool,
}

impl TextBuf {
    pub fn new(text: &str, multiline: bool) -> Self {
        let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        Self { lines, row, col, multiline }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn byte(&self, row: usize, col: usize) -> usize {
        self.lines[row].char_indices().nth(col).map(|(b, _)| b).unwrap_or(self.lines[row].len())
    }

    pub fn insert(&mut self, c: char) {
        let b = self.byte(self.row, self.col);
        self.lines[self.row].insert(b, c);
        self.col += 1;
    }

    pub fn newline(&mut self) {
        if !self.multiline {
            return;
        }
        let b = self.byte(self.row, self.col);
        let rest = self.lines[self.row].split_off(b);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    pub fn backspace(&mut self) {
        if self.col > 0 {
            let b = self.byte(self.row, self.col - 1);
            self.lines[self.row].remove(b);
            self.col -= 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.len(self.row);
            self.lines[self.row].push_str(&cur);
        }
    }

    pub fn delete(&mut self) {
        if self.col < self.len(self.row) {
            let b = self.byte(self.row, self.col);
            self.lines[self.row].remove(b);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    /// Traite une touche ; renvoie `true` si elle a été consommée.
    pub fn handle_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('a') if ctrl => self.col = 0,
            KeyCode::Char('e') if ctrl => self.col = self.len(self.row),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => self.insert(c),
            KeyCode::Enter => self.newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.len(self.row);
                }
            }
            KeyCode::Right => {
                if self.col < self.len(self.row) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            KeyCode::Up if self.row > 0 => {
                self.row -= 1;
                self.col = self.col.min(self.len(self.row));
            }
            KeyCode::Down if self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.col = self.col.min(self.len(self.row));
            }
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.len(self.row),
            _ => return false,
        }
        true
    }

    /// Dessine le champ dans un cadre titré ; place le curseur si `focused`.
    pub fn render(&self, f: &mut Frame, area: Rect, title: &str, focused: bool) {
        let style = if focused { Style::new().fg(Color::Cyan) } else { Style::new().fg(Color::DarkGray) };
        let block = Block::bordered().title(title.to_string()).border_style(style);
        let inner = block.inner(area);
        let (w, h) = (inner.width as usize, inner.height as usize);
        let sy = (self.row + 1).saturating_sub(h.max(1));
        let sx = (self.col + 1).saturating_sub(w.max(1));
        let lines: Vec<Line> = self.lines.iter().map(|l| Line::raw(l.clone())).collect();
        f.render_widget(Paragraph::new(lines).block(block).scroll((sy as u16, sx as u16)), area);
        if focused && w > 0 && h > 0 {
            f.set_cursor_position((inner.x + (self.col - sx) as u16, inner.y + (self.row - sy) as u16));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn saisie_et_accents() {
        let mut b = TextBuf::new("", false);
        for c in "été".chars() {
            b.handle_key(key(KeyCode::Char(c)));
        }
        b.handle_key(key(KeyCode::Left));
        b.handle_key(key(KeyCode::Backspace));
        assert_eq!(b.text(), "éé");
    }

    #[test]
    fn multiligne() {
        let mut b = TextBuf::new("ab", true);
        b.handle_key(key(KeyCode::Left));
        b.handle_key(key(KeyCode::Enter));
        assert_eq!(b.text(), "a\nb");
        b.handle_key(key(KeyCode::Backspace));
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn monoligne_ignore_entree() {
        let mut b = TextBuf::new("x", false);
        b.handle_key(key(KeyCode::Enter));
        assert_eq!(b.text(), "x");
    }

    #[test]
    fn navigation_verticale() {
        let mut b = TextBuf::new("long\nab", true);
        b.handle_key(key(KeyCode::Up));
        b.handle_key(key(KeyCode::Down));
        b.handle_key(key(KeyCode::Char('!')));
        assert_eq!(b.text(), "long\nab!");
    }
}
