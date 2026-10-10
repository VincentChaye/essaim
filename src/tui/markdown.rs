//! Rendu Markdown lisible : `glow` s'il est installé, sinon un rendu intégré léger.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::io::Write;
use std::process::{Command, Stdio};

/// Rend `md` sur `width` colonnes. `glow` (https://github.com/charmbracelet/glow) est utilisé s'il est dans le PATH.
pub fn render(md: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(20);
    if let Some(ansi) = glow(md, width) {
        return ansi.lines().map(ansi_line).collect();
    }
    builtin(md, width)
}

pub fn glow_available() -> bool {
    Command::new("glow").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn glow(md: &str, width: usize) -> Option<String> {
    let mut child = Command::new("glow")
        .args(["-s", "dark", "-w", &width.to_string(), "-"])
        .env("CLICOLOR_FORCE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let input = md.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out = child.wait_with_output().ok()?;
    let _ = writer.join();
    (out.status.success() && !out.stdout.is_empty()).then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Convertit une ligne contenant des séquences SGR (couleurs, gras…) en `Line`.
pub fn ansi_line(s: &str) -> Line<'static> {
    let mut spans = vec![];
    let mut style = Style::new();
    let mut buf = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            buf.push(c);
            continue;
        }
        if it.peek() != Some(&'[') {
            continue;
        }
        it.next();
        let mut params = String::new();
        let mut fin = ' ';
        for ch in it.by_ref() {
            if ch.is_ascii_digit() || ch == ';' {
                params.push(ch);
            } else {
                fin = ch;
                break;
            }
        }
        if fin != 'm' {
            continue; // autre séquence (curseur, effacement…) : ignorée
        }
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut buf), style));
        }
        style = apply_sgr(style, &params);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    // glow remplit chaque ligne d'espaces jusqu'à la largeur : on les retire.
    while spans.last().is_some_and(|sp| sp.content.trim().is_empty() && sp.style.bg.is_none()) {
        spans.pop();
    }
    Line::from(spans)
}

fn apply_sgr(mut st: Style, params: &str) -> Style {
    let codes: Vec<u32> = if params.is_empty() { vec![0] } else { params.split(';').map(|p| p.parse().unwrap_or(0)).collect() };
    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            0 => st = Style::new(),
            1 => st = st.add_modifier(Modifier::BOLD),
            2 => st = st.add_modifier(Modifier::DIM),
            3 => st = st.add_modifier(Modifier::ITALIC),
            4 => st = st.add_modifier(Modifier::UNDERLINED),
            7 => st = st.add_modifier(Modifier::REVERSED),
            9 => st = st.add_modifier(Modifier::CROSSED_OUT),
            22 => st = st.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => st = st.remove_modifier(Modifier::ITALIC),
            24 => st = st.remove_modifier(Modifier::UNDERLINED),
            n @ 30..=37 => st = st.fg(Color::Indexed((n - 30) as u8)),
            39 => st.fg = None,
            n @ 40..=47 => st = st.bg(Color::Indexed((n - 40) as u8)),
            49 => st.bg = None,
            n @ 90..=97 => st = st.fg(Color::Indexed((n - 90 + 8) as u8)),
            n @ 100..=107 => st = st.bg(Color::Indexed((n - 100 + 8) as u8)),
            n @ (38 | 48) => {
                let color = match codes.get(i + 1) {
                    Some(5) => codes.get(i + 2).map(|&v| Color::Indexed(v as u8)).inspect(|_| i += 2),
                    Some(2) if codes.len() > i + 4 => {
                        let c = Color::Rgb(codes[i + 2] as u8, codes[i + 3] as u8, codes[i + 4] as u8);
                        i += 4;
                        Some(c)
                    }
                    _ => None,
                };
                if let Some(c) = color {
                    st = if n == 38 { st.fg(c) } else { st.bg(c) };
                }
            }
            _ => {}
        }
        i += 1;
    }
    st
}

/// Rendu de secours : titres, listes, citations, code, gras / code en ligne.
fn builtin(md: &str, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![];
    let mut in_code = false;
    for raw in md.lines() {
        let t = raw.trim_end();
        if t.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push(Line::from(Span::styled(format!("  {t}"), Style::new().fg(Color::Green))));
            continue;
        }
        let h = t.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&h) && t[h..].starts_with(' ') {
            let text = t[h..].trim().to_string();
            let color = if h == 1 { Color::Magenta } else if h == 2 { Color::Cyan } else { Color::Yellow };
            if h == 1 {
                out.push(Line::from(Span::styled(text.clone(), Style::new().fg(color).add_modifier(Modifier::BOLD))));
                out.push(Line::from(Span::styled("═".repeat(text.chars().count().min(width)), Style::new().fg(color))));
            } else {
                out.push(Line::raw(""));
                out.push(Line::from(Span::styled(format!("{} {text}", "▎".repeat(1)), Style::new().fg(color).add_modifier(Modifier::BOLD))));
            }
            continue;
        }
        if t.trim_start().starts_with('|') {
            // Tableau : conservé tel quel, ligne de séparation estompée.
            let sep = t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '));
            let st = if sep { Style::new().fg(Color::DarkGray) } else { Style::new() };
            out.push(Line::from(Span::styled(t.to_string(), st)));
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            out.push(Line::from(vec![Span::styled("│ ", Style::new().fg(Color::DarkGray)), Span::styled(q.trim().to_string(), Style::new().add_modifier(Modifier::ITALIC))]));
            continue;
        }
        let trimmed = t.trim_start();
        let indent = t.len() - trimmed.len();
        let (prefix, body) = if let Some(b) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            (format!("{}• ", " ".repeat(indent)), b)
        } else {
            (" ".repeat(indent), trimmed)
        };
        let mut spans = vec![Span::raw(prefix)];
        spans.extend(inline(body));
        out.push(Line::from(spans));
    }
    out
}

/// **gras**, *italique* et `code` en ligne.
fn inline(s: &str) -> Vec<Span<'static>> {
    let mut spans = vec![];
    let mut buf = String::new();
    let mut bold = false;
    let mut code = false;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>, bold: bool, code: bool| {
        if buf.is_empty() {
            return;
        }
        let mut st = Style::new();
        if bold {
            st = st.add_modifier(Modifier::BOLD);
        }
        if code {
            st = st.fg(Color::Green);
        }
        spans.push(Span::styled(std::mem::take(buf), st));
    };
    while i < chars.len() {
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut buf, &mut spans, bold, code);
            bold = !bold;
            i += 2;
        } else if chars[i] == '`' {
            flush(&mut buf, &mut spans, bold, code);
            code = !code;
            i += 1;
        } else {
            buf.push(chars[i]);
            i += 1;
        }
    }
    flush(&mut buf, &mut spans, bold, code);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_converti_en_styles() {
        let l = ansi_line("\x1b[1;31mrouge\x1b[0m puis \x1b[38;5;42mvert\x1b[m");
        assert_eq!(l.spans[0].content, "rouge");
        assert_eq!(l.spans[0].style.fg, Some(Color::Indexed(1)));
        assert!(l.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(l.spans[1].content, " puis ");
        assert_eq!(l.spans[2].style.fg, Some(Color::Indexed(42)));
    }

    #[test]
    fn rendu_integre_titres_listes_gras() {
        let lines = builtin("# Titre\n\n- un **gras**\n```\ncode\n```", 40);
        let txt: Vec<String> = lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect();
        assert_eq!(txt[0], "Titre");
        assert!(txt.iter().any(|l| l == "• un gras"));
        assert!(txt.iter().any(|l| l == "  code"));
    }
}
