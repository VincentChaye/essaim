//! Onglet Entreprise : liste des entreprises, organigramme en arbre, détail du poste.

use super::app::{App, CompZone};
use super::settings::tr;
use crate::tf;
use crate::model::company::Company;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use crate::model::company::{GRAPH_BOX_W, graph_nav};

const BOX_H: usize = 4;
/// Hauteur d'un niveau : la boîte + 2 lignes de connecteurs.
const PITCH: usize = BOX_H + 2;

type Cell = (char, Style);

fn draw_missions(f: &mut Frame, area: Rect, app: &App) {
    let w = (area.width as usize).saturating_sub(10).max(8);
    let vis = app.visible_missions();
    let items: Vec<ListItem> = vis
        .iter()
        .map(|&i| {
            let m = &app.missions[i];
            let waiting = !m.waiting_for.is_empty();
            let (flag, color) = if waiting {
                (tr("⏳ en attente", "⏳ waiting"), Color::DarkGray)
            } else if m.done {
                (tr("✔ réussie", "✔ succeeded"), Color::Green)
            } else if m.has_report {
                (tr("rapport (non réussie)", "report (not succeeded)"), Color::Yellow)
            } else if m.resumable {
                (tr("plan validé · reprenable", "plan approved · resumable"), Color::Cyan)
            } else if m.has_conception {
                (tr("plan", "plan"), Color::Yellow)
            } else {
                (tr("brief", "brief"), Color::DarkGray)
            };
            let title_of = |id: &str| app.missions.iter().find(|x| x.id == id).map(|x| x.title.clone());
            let open = app.mission_open.get(&m.id);
            let head = Style::new().fg(if waiting { Color::DarkGray } else { Color::Reset });
            let mut lines = vec![Line::styled(format!("{} {}", if open.is_some() { "▾" } else { "▸" }, m.title), head), Line::styled(format!("  {flag}"), Style::new().fg(color))];
            // Ordre de lancement : ce qu'il faut réussir avant, et ce que celle-ci débloque.
            for a in &m.after {
                if let Some(t) = title_of(a) {
                    let ok = !m.waiting_for.contains(a);
                    lines.push(Line::styled(format!("  ↳ {} {t}", if ok { tr("après ✔", "after ✔") } else { tr("après ✖", "after ✖") }), Style::new().fg(if ok { Color::Green } else { Color::DarkGray })));
                }
            }
            for next in app.missions.iter().filter(|x| x.after.contains(&m.id)) {
                lines.push(Line::styled(format!("  ↦ {} {}", tr("débloque", "unlocks"), next.title), Style::new().fg(Color::DarkGray)));
            }
            if let Some(s) = open {
                let mut sect = |label: &str, col: Color, items: &[String], none: &str| {
                    lines.push(Line::styled(format!("  {label}"), Style::new().fg(col).add_modifier(Modifier::BOLD)));
                    if items.is_empty() {
                        lines.push(Line::styled(format!("    {none}"), Style::new().fg(Color::DarkGray)));
                    }
                    for it in items {
                        let chars: Vec<char> = it.chars().collect();
                        for (n, part) in chars.chunks(w).enumerate() {
                            lines.push(Line::raw(format!("{}{}", if n == 0 { "    • " } else { "      " }, part.iter().collect::<String>())));
                        }
                    }
                };
                sect(tr("But", "Goal"), Color::Cyan, &s.goal, "—");
                sect(tr("Fichiers", "Files"), Color::Yellow, &s.files, tr("aucun cité", "none cited"));
                sect(tr("Résultat", "Outcome"), Color::Green, &s.outcome, tr("pas de rapport", "no report"));
            }
            ListItem::new(lines)
        })
        .collect();
    let mut st = ListState::default().with_selected(vis.iter().position(|&i| i == app.mission_sel));
    let open = app.missions.get(app.mission_sel).is_some_and(|m| app.mission_open.contains_key(&m.id));
    let hint = if vis.is_empty() {
        tr(" m : cadrer ", " m: frame one ")
    } else if open {
        tr(" ↑↓ · ← replier · r lancer · a prérequis · v réussie ", " ↑↓ · ← fold · r run · a prerequisites · v succeeded ")
    } else {
        tr(" ↑↓ · → déplier · r lancer · a prérequis · v réussie ", " ↑↓ · → unfold · r run · a prerequisites · v succeeded ")
    };
    f.render_stateful_widget(
        List::new(items)
            .block(zone_block(app, CompZone::Missions).title(tf!(" Missions ({}) ", " Missions ({}) ", vis.len())).title_bottom(hint))
            .highlight_style(highlight()),
        area,
        &mut st,
    );
}

fn kv(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{k} : "), Style::new().fg(Color::Cyan)), Span::raw(v.to_string())])
}

/// Bordure de la zone : jaune gras si on est dedans, cyan si le curseur est dessus.
fn zone_block<'a>(app: &App, z: CompZone) -> Block<'a> {
    let b = Block::bordered();
    if app.zone != z {
        return b;
    }
    b.border_style(if app.zone_in { Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD) } else { Style::new().fg(Color::Cyan) })
}

fn highlight() -> Style {
    Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    // Bandeau du haut : entreprises · missions · poste ; tout le reste de la page est pour le graphe.
    let [top, graph_area] = Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).flex(ratatui::layout::Flex::Start).areas(area);
    let top = Rect { height: top.height.max(12).min(area.height / 2), ..top };
    let graph_area = Rect { y: top.y + top.height, height: area.height.saturating_sub(top.height), ..graph_area };
    let [left, missions_area, right] = Layout::horizontal([Constraint::Percentage(22), Constraint::Percentage(28), Constraint::Percentage(50)]).areas(top);
    draw_missions(f, missions_area, app);

    // --- entreprises ---
    let items: Vec<ListItem> = app
        .companies
        .iter()
        .map(|c| {
            let bad = c.parsed.is_err() || c.problem.is_some();
            ListItem::new(format!("{}{}", if bad { "⚠ " } else { "" }, c.file.trim_end_matches(".toml")))
        })
        .collect();
    let mut st = ListState::default().with_selected((!app.companies.is_empty()).then_some(app.comp_sel));
    f.render_stateful_widget(
        List::new(items)
            .block(zone_block(app, CompZone::Companies).title(tf!(" Entreprises ({}) ", " Companies ({}) ", app.companies.len())).title_bottom(tr(" Entrée puis ↑↓ ", " Enter then ↑↓ ")))
            .highlight_style(highlight())
            .highlight_symbol("▸ "),
        left,
        &mut st,
    );

    let Some(entry) = app.companies.get(app.comp_sel) else {
        f.render_widget(
            Paragraph::new(tr("Aucune entreprise dans .claude/companies/.\n\nn : un assistant conçoit l'organigramme avec toi (et crée les agents manquants).\n`essaim init` en crée aussi une d'exemple.", "No company in .claude/companies/.\n\nn: an assistant designs the org chart with you (and creates the missing agents).\n`essaim init` also creates an example one."))
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(tr(" Organigramme ", " Org chart "))),
            graph_area,
        );
        return;
    };
    let company = match &entry.parsed {
        Ok(c) => c,
        Err(e) => {
            f.render_widget(
                Paragraph::new(Line::styled(tf!("Illisible : {e}", "Unreadable: {e}"), Style::new().fg(Color::Red)))
                    .wrap(Wrap { trim: false })
                    .block(Block::bordered().title(tr(" Organigramme ", " Org chart "))),
                graph_area,
            );
            return;
        }
    };

    // --- graphe ---
    let rows = company.tree_rows();
    let sel = app.seat_sel.min(rows.len().saturating_sub(1));
    draw_graph(f, graph_area, company, sel, app, entry.problem.is_none());

    // --- détail ---
    let mut lines: Vec<Line> = vec![];
    if let Some(p) = &entry.problem {
        lines.push(Line::styled(format!("⚠ {p}"), Style::new().fg(Color::Red)));
    }
    let e = company.estimate_calls();
    lines.push(kv(tr("Appels (max)", "Calls (max)"), &tf!("conception {} + réalisation {} = {}", "design {} + realisation {} = {}", e.conception, e.realisation, e.total())));
    lines.push(kv(
        tr("Garde-fous", "Safeguards"),
        &tf!("{} appels · {} parallèles · contrôle {}", "{} calls · {} in parallel · checkpoint {}", company.max_calls, company.max_parallel, if company.checkpoint { tr("oui", "yes") } else { tr("non", "no") }),
    ));
    if e.total() > company.max_calls {
        lines.push(Line::styled(tr("⚠ l'estimation dépasse max_calls", "⚠ the estimate exceeds max_calls"), Style::new().fg(Color::Yellow)));
    }
    if let Some(r) = rows.get(sel) {
        lines.extend(seat_lines(company, r.idx, app));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::bordered().title(tr(" Poste ", " Seat "))), right);
}

/// Modèle effectif d'un poste et son origine : surcharge du poste, modèle de l'agent, ou défaut de `claude`.
fn seat_model_label(seat: Option<&str>, agent: Option<&str>) -> String {
    match (seat, agent) {
        (Some(m), _) => format!("{m} ({})", tr("fixé par le poste", "set by the seat")),
        (None, Some(m)) => format!("{m} ({})", tr("hérité de l'agent", "inherited from the agent")),
        (None, None) => tr("défaut de claude", "claude default").to_string(),
    }
}

fn seat_lines(company: &Company, i: usize, app: &App) -> Vec<Line<'static>> {
    let s = &company.seats[i];
    let agent = app.agents.iter().find(|a| a.name == s.agent);
    let kids = company.children(i);
    let mut v = vec![
        kv(tr("Poste", "Seat"), &format!("{} ({})", s.title, company.level(i))),
        kv("Agent", &format!("{} · id {}", s.agent, s.id)),
        kv(tr("Modèle", "Model"), &seat_model_label(s.model.as_deref(), agent.and_then(|a| a.model.as_deref()))),
    ];
    if agent.is_none() {
        v.push(Line::styled(tr("  ⚠ agent introuvable", "  ⚠ agent not found"), Style::new().fg(Color::Red)));
    }
    v.push(kv(
        tr("Supérieur", "Supervisor"),
        &company.parent(i).map(|p| format!("{} ({})", company.seats[p].title, company.seats[p].id)).unwrap_or_else(|| "— (boss)".into()),
    ));
    v.push(kv(
        tr("Subordonnés", "Subordinates"),
        &if kids.is_empty() {
            tr("aucun", "none").to_string()
        } else {
            format!("{} — {}", kids.len(), kids.iter().map(|&k| company.seats[k].title.as_str()).collect::<Vec<_>>().join(", "))
        },
    ));
    let expertise = s.expertise.clone().or_else(|| agent.map(|a| a.description.clone())).unwrap_or_default();
    v.push(kv("Expertise", &expertise));
    v.push(kv(tr("Réalisation", "Realisation"), if s.builds { tr("⚒ oui — modifie le projet", "⚒ yes — modifies the project") } else { tr("non — texte uniquement", "no — text only") }));
    if !s.allow_bash.is_empty() {
        v.push(kv(tr("Commandes", "Commands"), &s.allow_bash.join(", ")));
    }
    v
}

fn put(grid: &mut [Vec<Cell>], x: usize, y: usize, c: char, st: Style) {
    if let Some(cell) = grid.get_mut(y).and_then(|r| r.get_mut(x)) {
        *cell = (c, st);
    }
}

fn put_str(grid: &mut [Vec<Cell>], x: usize, y: usize, text: &str, st: Style) {
    for (k, c) in text.chars().enumerate() {
        put(grid, x + k, y, c, st);
    }
}

/// Tronque à `w` caractères (… si coupé).
fn clip(s: &str, w: usize) -> String {
    if s.chars().count() <= w { s.to_string() } else { s.chars().take(w.saturating_sub(1)).chain(std::iter::once('…')).collect() }
}

/// Caractère de tracé selon les directions reliées.
fn joint(up: bool, down: bool, left: bool, right: bool) -> char {
    match (up, down, left, right) {
        (true, true, true, true) => '┼',
        (true, true, false, true) => '├',
        (true, true, true, false) => '┤',
        (true, true, false, false) => '│',
        (true, false, true, true) => '┴',
        (false, true, true, true) => '┬',
        (false, true, false, true) => '┌',
        (false, true, true, false) => '┐',
        (true, false, false, true) => '└',
        (true, false, true, false) => '┘',
        _ => '─',
    }
}

/// Organigramme en arbre de boîtes (supérieur en haut, niveaux en lignes), centré sur le poste sélectionné.
fn draw_graph(f: &mut Frame, area: Rect, company: &Company, sel: usize, app: &App, valid: bool) {
    let title = match valid {
        true => tf!(" {} — organigramme valide ✔ ", " {} — valid org chart ✔ ", company.name),
        false => tf!(" {} — organigramme invalide ⚠ ", " {} — invalid org chart ⚠ ", company.name),
    };
    let hint = tr(" Entrée puis ←↑↓→ · a ajouter · e éditer · x retirer ", " Enter then ←↑↓→ · a add · e edit · x remove ");
    let block = zone_block(app, CompZone::Graph).title(title).title_bottom(hint);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let g = company.graph();
    if g.is_empty() || inner.width == 0 || inner.height == 0 {
        return;
    }
    let depth_max = g.iter().map(|n| n.depth).max().unwrap_or(0);
    let (w, h) = (g.iter().map(|n| n.cx).max().unwrap_or(0) + GRAPH_BOX_W / 2 + 1, (depth_max + 1) * PITCH - 2);
    let mut grid: Vec<Vec<Cell>> = vec![vec![(' ', Style::new()); w]; h];
    let line = Style::new().fg(Color::DarkGray);

    // Connecteurs : barre horizontale à mi-hauteur entre un supérieur et ses subordonnés.
    for (row, n) in g.iter().enumerate() {
        let kids = graph_nav::children(&g, row);
        if kids.is_empty() {
            continue;
        }
        let y0 = n.depth * PITCH + BOX_H;
        let xs: Vec<usize> = kids.iter().map(|&k| g[k].cx).collect();
        let (lo, hi) = (xs.iter().copied().fold(n.cx, usize::min), xs.iter().copied().fold(n.cx, usize::max));
        put(&mut grid, n.cx, y0, '│', line);
        for x in lo..=hi {
            put(&mut grid, x, y0 + 1, joint(x == n.cx, xs.contains(&x), x > lo, x < hi), line);
        }
    }

    // Boîtes.
    for (row, n) in g.iter().enumerate() {
        let s = &company.seats[n.idx];
        let (x0, y0) = (n.cx - GRAPH_BOX_W / 2, n.depth * PITCH);
        let selected = row == sel;
        let agent = app.agents.iter().find(|a| a.name == s.agent);
        let border = if selected { Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD) } else { Style::new().fg(Color::Gray) };
        let fill = if selected { Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD) } else { Style::new() };
        let inner_w = GRAPH_BOX_W - 2;
        let label = format!(" {} ", company.level(n.idx));
        let top = format!("┌{label}{}┐", "─".repeat(inner_w - label.chars().count()));
        put_str(&mut grid, x0, y0, &top, border);
        put_str(&mut grid, x0, y0 + BOX_H - 1, &format!("└{}┘", "─".repeat(inner_w)), border);
        for y in [y0 + 1, y0 + 2] {
            put(&mut grid, x0, y, '│', border);
            put(&mut grid, x0 + GRAPH_BOX_W - 1, y, '│', border);
            put_str(&mut grid, x0 + 1, y, &" ".repeat(inner_w), fill);
        }
        put_str(&mut grid, x0 + 1, y0 + 1, &clip(&s.title, inner_w), fill.add_modifier(Modifier::BOLD).fg(Color::White));
        let agent_style = if agent.is_none() { fill.fg(Color::Red) } else { fill.fg(Color::Gray) };
        put_str(&mut grid, x0 + 1, y0 + 2, &clip(&s.agent, if s.builds { inner_w - 2 } else { inner_w }), agent_style);
        if s.builds {
            put(&mut grid, x0 + GRAPH_BOX_W - 2, y0 + 2, '⚒', fill.fg(Color::Yellow));
        }
    }

    // Fenêtre : le poste sélectionné reste au centre quand le graphe dépasse.
    let sn = g[sel.min(g.len() - 1)];
    let (iw, ih) = (inner.width as usize, inner.height as usize);
    let ox = sn.cx.saturating_sub(iw / 2).min(w.saturating_sub(iw));
    let oy = (sn.depth * PITCH + BOX_H / 2).saturating_sub(ih / 2).min(h.saturating_sub(ih));
    let buf = f.buffer_mut();
    for dy in 0..ih.min(h - oy) {
        for dx in 0..iw.min(w - ox) {
            let (c, st) = grid[oy + dy][ox + dx];
            buf[(inner.x + dx as u16, inner.y + dy as u16)].set_char(c).set_style(st);
        }
    }
    let more = Style::new().fg(Color::Yellow);
    let (mx, my) = (inner.x + inner.width / 2, inner.y + inner.height / 2);
    if ox > 0 {
        buf[(inner.x, my)].set_char('◀').set_style(more);
    }
    if ox + iw < w {
        buf[(inner.x + inner.width - 1, my)].set_char('▶').set_style(more);
    }
    if oy > 0 {
        buf[(mx, inner.y)].set_char('▲').set_style(more);
    }
    if oy + ih < h {
        buf[(mx, inner.y + inner.height - 1)].set_char('▼').set_style(more);
    }
}
