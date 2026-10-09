//! Terminal intégré : un processus (ici `claude`) tourne dans un pseudo-terminal que l'outil
//! possède. Sa sortie est interprétée par un émulateur (`vt100`) puis dessinée dans un onglet ;
//! les touches tapées dans l'onglet sont réinjectées dans le pseudo-terminal.

use super::app::{AppEvent, AssistantKind, Tab};
use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

/// Réponses aux requêtes que les applications envoient à leur terminal
/// (position du curseur, identification…). Sans elles, certaines attendent un délai.
#[derive(Default)]
pub struct Replier {
    out: Vec<u8>,
}

impl vt100::Callbacks for Replier {
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            (None, 'n', 6) => {
                let (r, col) = screen.cursor_position();
                self.out.extend(format!("\x1b[{};{}R", r + 1, col + 1).bytes());
            }
            (None, 'n', 5) => self.out.extend(b"\x1b[0n"),
            (None, 'c', 0) => self.out.extend(b"\x1b[?1;2c"),
            (Some(b'>'), 'c', 0) => self.out.extend(b"\x1b[>0;0;0c"),
            // Protocole clavier « kitty » : non pris en charge.
            (Some(b'?'), 'u', _) => self.out.extend(b"\x1b[?0u"),
            _ => {}
        }
    }
}

type Shared = Arc<Mutex<vt100::Parser<Replier>>>;

pub struct TermSession {
    pub id: u64,
    pub title: String,
    /// Assistant de création (None = simple `claude attach`).
    pub kind: Option<AssistantKind>,
    /// Onglet où revenir à la fermeture.
    pub origin: Tab,
    /// Éléments existants avant le lancement (pour repérer ce que l'assistant a créé).
    pub before: Vec<String>,
    pub started: Instant,
    pub exit_code: Option<u32>,
    pub exited: bool,
    parser: Shared,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    size: (u16, u16),
}

impl TermSession {
    /// Lance `program args…` dans un pseudo-terminal de `rows`×`cols`.
    pub fn spawn(program: &str, args: &[String], cwd: &Path, (rows, cols): (u16, u16), id: u64, tx: UnboundedSender<AppEvent>) -> Result<Self> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("cannot open a pseudo-terminal")?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // Jamais de facturation API : mêmes variables (même prédicat) que pour les autres lancements.
        for var in crate::runtime::claude_proc::billing_env_to_remove() {
            cmd.env_remove(var);
        }
        let child = pair.slave.spawn_command(cmd).with_context(|| format!("impossible de lancer `{program}`"))?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        // Un seul écrivain possible : partagé entre les touches de l'utilisateur et les réponses au terminal.
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser: Shared = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(rows, cols, 0, Replier::default())));

        let (p, tx2) = (parser.clone(), tx.clone());
        let reply_writer = writer.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16384];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let reply = {
                            let mut g = p.lock().unwrap();
                            g.process(&buf[..n]);
                            std::mem::take(&mut g.callbacks_mut().out)
                        };
                        if !reply.is_empty() {
                            let mut w = reply_writer.lock().unwrap();
                            let _ = w.write_all(&reply);
                            let _ = w.flush();
                        }
                        if tx2.send(AppEvent::TermOutput).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = tx2.send(AppEvent::TermExit(id));
        });

        Ok(Self {
            id,
            title: String::new(),
            kind: None,
            origin: Tab::Agents,
            before: vec![],
            started: Instant::now(),
            exit_code: None,
            exited: false,
            parser,
            writer,
            master: pair.master,
            child,
            killer,
            size: (rows, cols),
        })
    }

    pub fn send(&mut self, bytes: &[u8]) {
        if !self.exited {
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
    }

    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }

    /// Appelé quand le flux se ferme : récupère le code de sortie.
    pub fn mark_exited(&mut self) {
        self.exited = true;
        self.exit_code = self.child.wait().ok().map(|s| s.exit_code());
    }

    pub fn ok(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        f(self.parser.lock().unwrap().screen())
    }

    pub fn app_cursor(&self) -> bool {
        self.with_screen(|s| s.application_cursor())
    }

    /// Colle un texte (encadré par les marqueurs de collage si l'application les a demandés).
    pub fn paste(&mut self, text: &str) {
        if self.with_screen(|s| s.bracketed_paste()) {
            self.send(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.send(text.replace('\n', "\r").as_bytes());
        }
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}

/// Traduit une touche en octets pour le pseudo-terminal.
pub fn encode_key(k: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    // Paramètre de modificateur xterm : 1 + shift(1) + alt(2) + ctrl(4).
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let with_esc = |mut v: Vec<u8>| {
        if alt {
            v.insert(0, 0x1b);
        }
        v
    };
    let csi_letter = |c: char| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let csi_tilde = |n: u8| -> Vec<u8> {
        if m > 1 { format!("\x1b[{n};{m}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    Some(match k.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c.to_ascii_lowercase() {
                x @ 'a'..='z' => x as u8 - b'a' + 1,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 27,
                '\\' | '4' => 28,
                ']' | '5' => 29,
                '^' | '6' => 30,
                '_' | '7' | '/' => 31,
                '8' | '?' => 127,
                _ => return None,
            };
            with_esc(vec![b])
        }
        KeyCode::Char(c) => with_esc(c.to_string().into_bytes()),
        KeyCode::Enter if shift || alt => b"\x1b\r".to_vec(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => with_esc(vec![0x7f]),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => csi_letter('A'),
        KeyCode::Down => csi_letter('B'),
        KeyCode::Right => csi_letter('C'),
        KeyCode::Left => csi_letter('D'),
        KeyCode::Home => csi_letter('H'),
        KeyCode::End => csi_letter('F'),
        KeyCode::Insert => csi_tilde(2),
        KeyCode::Delete => csi_tilde(3),
        KeyCode::PageUp => csi_tilde(5),
        KeyCode::PageDown => csi_tilde(6),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", ['P', 'Q', 'R', 'S'][n as usize - 1]).into_bytes(),
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5];
            csi_tilde(code)
        }
        _ => return None,
    })
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Dessine l'écran émulé dans `area`. Renvoie la position du curseur si elle doit être affichée.
pub fn render(f: &mut Frame, area: Rect, screen: &vt100::Screen) -> Option<(u16, u16)> {
    let (rows, cols) = screen.size();
    let buf = f.buffer_mut();
    for r in 0..rows.min(area.height) {
        for c in 0..cols.min(area.width) {
            let Some(cell) = screen.cell(r, c) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut st = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
            for (on, m) in [
                (cell.bold(), Modifier::BOLD),
                (cell.dim(), Modifier::DIM),
                (cell.italic(), Modifier::ITALIC),
                (cell.underline(), Modifier::UNDERLINED),
                (cell.inverse(), Modifier::REVERSED),
            ] {
                if on {
                    st = st.add_modifier(m);
                }
            }
            let target = &mut buf[(area.x + c, area.y + r)];
            let s = cell.contents();
            target.set_symbol(if s.is_empty() { " " } else { s });
            target.set_style(st);
        }
    }
    if screen.hide_cursor() {
        return None;
    }
    let (cr, cc) = screen.cursor_position();
    (cr < area.height && cc < area.width).then_some((area.x + cc, area.y + cr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn touches_simples_et_controle() {
        let n = KeyModifiers::NONE;
        assert_eq!(encode_key(key(KeyCode::Char('a'), n), false).unwrap(), b"a");
        assert_eq!(encode_key(key(KeyCode::Char('é'), n), false).unwrap(), "é".as_bytes());
        assert_eq!(encode_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false).unwrap(), vec![3]);
        assert_eq!(encode_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL), false).unwrap(), vec![4]);
        assert_eq!(encode_key(key(KeyCode::Char('b'), KeyModifiers::ALT), false).unwrap(), b"\x1bb");
        assert_eq!(encode_key(key(KeyCode::Enter, n), false).unwrap(), b"\r");
        assert_eq!(encode_key(key(KeyCode::Enter, KeyModifiers::SHIFT), false).unwrap(), b"\x1b\r");
        assert_eq!(encode_key(key(KeyCode::Backspace, n), false).unwrap(), vec![0x7f]);
        assert_eq!(encode_key(key(KeyCode::Esc, n), false).unwrap(), vec![0x1b]);
        assert_eq!(encode_key(key(KeyCode::BackTab, KeyModifiers::SHIFT), false).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn fleches_et_navigation() {
        let n = KeyModifiers::NONE;
        assert_eq!(encode_key(key(KeyCode::Up, n), false).unwrap(), b"\x1b[A");
        assert_eq!(encode_key(key(KeyCode::Up, n), true).unwrap(), b"\x1bOA");
        assert_eq!(encode_key(key(KeyCode::Right, KeyModifiers::CONTROL), false).unwrap(), b"\x1b[1;5C");
        assert_eq!(encode_key(key(KeyCode::Left, KeyModifiers::ALT), false).unwrap(), b"\x1b[1;3D");
        assert_eq!(encode_key(key(KeyCode::Delete, n), false).unwrap(), b"\x1b[3~");
        assert_eq!(encode_key(key(KeyCode::PageDown, n), false).unwrap(), b"\x1b[6~");
        assert_eq!(encode_key(key(KeyCode::F(1), n), false).unwrap(), b"\x1bOP");
        assert_eq!(encode_key(key(KeyCode::F(5), n), false).unwrap(), b"\x1b[15~");
    }

    #[test]
    fn rendu_couleurs_et_curseur() {
        let mut p = vt100::Parser::new(5, 20, 0);
        p.process(b"\x1b[1;31mrouge\x1b[0m ok\r\n\x1b[7minv\x1b[0m\x1b[3;4H");
        let mut t = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut cursor = None;
        t.draw(|f| cursor = render(f, f.area(), p.screen())).unwrap();
        let buf = t.backend().buffer();
        let row0: String = (0..8).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(row0, "rouge ok");
        assert_eq!(buf[(0, 0)].fg, Color::Indexed(1));
        assert!(buf[(0, 0)].modifier.contains(Modifier::BOLD));
        assert!(buf[(0, 1)].modifier.contains(Modifier::REVERSED));
        assert_eq!(cursor, Some((3, 2)));
    }

    #[test]
    fn caracteres_larges() {
        let mut p = vt100::Parser::new(2, 10, 0);
        p.process("a世b".as_bytes());
        let mut t = Terminal::new(TestBackend::new(10, 2)).unwrap();
        t.draw(|f| {
            render(f, f.area(), p.screen());
        })
        .unwrap();
        let buf = t.backend().buffer();
        assert_eq!(buf[(0, 0)].symbol(), "a");
        assert_eq!(buf[(1, 0)].symbol(), "世");
    }

    fn wait_for(rx: &mut mpsc::UnboundedReceiver<AppEvent>, cond: &mut dyn FnMut() -> bool, what: &str) {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if cond() {
                return;
            }
            let _ = rx.try_recv();
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("délai dépassé : {what}");
    }

    #[test]
    fn processus_reel_dans_un_pty() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let script = r#"printf '\033[32mbonjour\033[0m\n'; printf 'age ? '; read x; echo "recu:$x"; exit 3"#;
        let mut t = TermSession::spawn("sh", &["-c".into(), script.into()], &std::env::temp_dir(), (10, 40), 1, tx).unwrap();
        let has = |t: &TermSession, s: &str| t.with_screen(|sc| sc.contents().contains(s));
        let mut ok = false;
        wait_for(&mut rx, &mut || { ok = has(&t, "age ?"); ok }, "invite affichée");
        t.send(b"42\r"); // injection d'entrée utilisateur
        wait_for(&mut rx, &mut || has(&t, "recu:42"), "entrée reçue par le processus");
        // Fin du processus : le flux se ferme et le code de sortie est récupéré.
        let end = Instant::now() + Duration::from_secs(10);
        let mut exited = false;
        while Instant::now() < end && !exited {
            while let Ok(ev) = rx.try_recv() {
                exited |= matches!(ev, AppEvent::TermExit(1));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(exited, "TermExit attendu");
        t.mark_exited();
        assert_eq!(t.exit_code, Some(3));
        assert!(!t.ok());
    }

    #[test]
    fn redimensionnement_et_reponse_au_terminal() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        // Le script demande la position du curseur (ESC[6n) et affiche la réponse reçue.
        let script = r#"stty raw -echo; printf '\033[6n'; IFS= read -r -d R r; stty sane; echo "rep:$(printf '%s' "$r" | tr -d '\033')"; stty size"#;
        let mut t = TermSession::spawn("sh", &["-c".into(), script.into()], &std::env::temp_dir(), (10, 40), 1, tx).unwrap();
        t.resize(12, 50);
        assert_eq!(t.size(), (12, 50));
        let has = |t: &TermSession, s: &str| t.with_screen(|sc| sc.contents().contains(s));
        wait_for(&mut rx, &mut || has(&t, "rep:[1;1"), "réponse à ESC[6n");
        wait_for(&mut rx, &mut || has(&t, "12 50"), "taille vue par le processus après resize");
    }
}
