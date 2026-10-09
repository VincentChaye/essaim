pub mod app;
pub mod assistant;
pub mod company_view;
pub mod editor;
pub mod manual;
pub mod settings;
pub mod term;
pub mod ui;

#[cfg(test)]
mod tests;

use anyhow::Result;
use app::{App, AppEvent};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use ratatui::DefaultTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

/// Ouvre un fichier dans $VISUAL / $EDITOR (ou vi) en suspendant la TUI.
fn open_in_editor(path: &PathBuf) {
    let cmd = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".into());
    let mut parts = cmd.split_whitespace();
    if let Some(bin) = parts.next() {
        let _ = std::process::Command::new(bin).args(parts).arg(path).status();
    }
}

fn init_terminal() -> DefaultTerminal {
    let terminal = ratatui::init();
    // Le collage arrive d'un bloc (et non touche par touche) : indispensable pour le terminal intégré.
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    terminal
}

fn restore_terminal() {
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
}

pub async fn run(root: PathBuf) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(root, tx.clone());
    app.load_settings(settings::path());

    // Lecture clavier dans un thread dédié ; mise en pause pendant l'éditeur externe.
    let paused = Arc::new(AtomicBool::new(false));
    {
        let (tx, paused) = (tx.clone(), paused.clone());
        std::thread::spawn(move || {
            loop {
                if paused.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(20));
                } else if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                    let ev = match event::read() {
                        Ok(Event::Key(k)) => AppEvent::Key(k),
                        Ok(Event::Paste(text)) => AppEvent::Paste(text),
                        Ok(_) => AppEvent::Tick, // redimensionnement : simple redessin
                        Err(_) => return,
                    };
                    if tx.send(ev).is_err() {
                        return;
                    }
                } else if tx.is_closed() {
                    return;
                }
            }
        });
    }
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_millis(200));
            loop {
                t.tick().await;
                if tx.send(AppEvent::Tick).is_err() {
                    return;
                }
            }
        });
    }

    let mut terminal = init_terminal();
    let result: Result<()> = async {
        loop {
            // Zone centrale = écran moins la barre d'onglets (3 lignes) et la ligne d'aide (1).
            let size = terminal.size()?;
            app.set_body_size(size.width, size.height.saturating_sub(4));
            terminal.draw(|f| ui::draw(f, &app))?;
            let Some(ev) = rx.recv().await else { break };
            app.on_event(ev);
            // Regroupe les événements déjà en attente avant de redessiner.
            while let Ok(ev) = rx.try_recv() {
                app.on_event(ev);
            }
            app.launch_pending();
            if let Some(path) = app.editor_request.take() {
                paused.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(120)).await;
                restore_terminal();
                open_in_editor(&path);
                terminal = init_terminal();
                paused.store(false, Ordering::SeqCst);
                app.reload();
            }
            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;
    restore_terminal();
    result
}
