//! Onglet Entreprise : liste des entreprises, organigramme en arbre, détail du poste.

use super::app::App;
use super::settings::tr;
use crate::tf;
use crate::model::company::Company;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};

fn draw_missions(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .missions
        .iter()
        .map(|m| {
            let (flag, color) = if m.has_report {
                (tr("✔ rapport", "✔ report"), Color::Green)
            } else if m.resumable {
                (tr("plan validé · reprenable", "plan approved · resumable"), Color::Cyan)
            } else if m.has_conception {
                (tr("plan", "plan"), Color::Yellow)
            } else {
                (tr("brief", "brief"), Color::DarkGray)
            };
            ListItem::new(vec![Line::raw(m.title.clone()), Line::styled(format!("  {flag}"), Style::new().fg(color))])
        })
        .collect();
    let mut st = ListState::default().with_selected((!app.missions.is_empty()).then_some(app.mission_sel));
    let hint = if app.missions.is_empty() { tr(" m : cadrer ", " m: frame one ") } else { tr(" t/T · Entrée ouvrir · r lancer/reprendre · R recommencer ", " t/T · Enter open · r run/resume · R restart ") };
    f.render_stateful_widget(
        List::new(items)
            .block(Block::bordered().title(tf!(" Missions ({}) ", " Missions ({}) ", app.missions.len())).title_bottom(hint))
            .highlight_style(highlight())
            .highlight_symbol("▸ "),
        area,
        &mut st,
    );
}

fn kv(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{k} : "), Style::new().fg(Color::Cyan)), Span::raw(v.to_string())])
}

fn highlight() -> Style {
    Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let [left_col, mid, right] = Layout::horizontal([Constraint::Percentage(24), Constraint::Percentage(40), Constraint::Percentage(36)]).areas(area);
    let [left, missions_area] = Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(left_col);
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
            .block(Block::bordered().title(tf!(" Entreprises ({}) ", " Companies ({}) ", app.companies.len())))
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
            Rect::new(mid.x, mid.y, mid.width + right.width, mid.height),
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
                Rect::new(mid.x, mid.y, mid.width + right.width, mid.height),
            );
            return;
        }
    };

    // --- arbre ---
    let rows = company.tree_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| {
            let s = &company.seats[r.idx];
            let mut spans = vec![
                Span::styled(r.prefix.clone(), Style::new().fg(Color::DarkGray)),
                Span::styled(format!("{:<4}", company.level(r.idx)), Style::new().fg(Color::Cyan)),
                Span::styled(s.title.clone(), Style::new().add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {}", s.agent), Style::new().fg(Color::DarkGray)),
            ];
            if s.builds {
                spans.push(Span::styled(" ⚒", Style::new().fg(Color::Yellow)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let sel = app.seat_sel.min(rows.len().saturating_sub(1));
    let mut st = ListState::default().with_selected((!rows.is_empty()).then_some(sel));
    f.render_stateful_widget(
        List::new(items)
            .block(Block::bordered().title(tf!(" {} — organigramme ", " {} — org chart ", company.name)))
            .highlight_style(highlight()),
        mid,
        &mut st,
    );

    // --- détail ---
    let mut lines: Vec<Line> = vec![];
    match &entry.problem {
        Some(p) => lines.push(Line::styled(format!("⚠ {p}"), Style::new().fg(Color::Red))),
        None => lines.push(Line::styled(tr("✔ organigramme valide", "✔ valid org chart"), Style::new().fg(Color::Green))),
    }
    let e = company.estimate_calls();
    lines.push(kv(tr("Appels (max)", "Calls (max)"), &tf!("conception {} + réalisation {} = {}", "design {} + realisation {} = {}", e.conception, e.realisation, e.total())));
    lines.push(kv(
        tr("Garde-fous", "Safeguards"),
        &tf!("{} appels · {} en parallèle · point de contrôle {}", "{} calls · {} in parallel · checkpoint {}", company.max_calls, company.max_parallel, if company.checkpoint { tr("oui", "yes") } else { tr("non", "no") }),
    ));
    if e.total() > company.max_calls {
        lines.push(Line::styled(tr("⚠ l'estimation dépasse max_calls", "⚠ the estimate exceeds max_calls"), Style::new().fg(Color::Yellow)));
    }
    lines.push(Line::raw(""));
    if let Some(r) = rows.get(sel) {
        lines.extend(seat_lines(company, r.idx, app));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::bordered().title(tr(" Poste ", " Seat "))), right);
}

fn seat_lines(company: &Company, i: usize, app: &App) -> Vec<Line<'static>> {
    let s = &company.seats[i];
    let agent = app.agents.iter().find(|a| a.name == s.agent);
    let kids = company.children(i);
    let mut v = vec![
        kv(tr("Poste", "Seat"), &format!("{} ({})", s.title, company.level(i))),
        kv("Id", &s.id),
        kv("Agent", &format!("{}{}", s.agent, s.model.as_deref().or(agent.and_then(|a| a.model.as_deref())).map(|m| format!(" [{m}]")).unwrap_or_default())),
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
    v.push(kv(tr("Réalisation", "Realisation"), if s.builds { tr("⚒ oui — peut modifier le projet dans son périmètre", "⚒ yes — can modify the project within its scope") } else { tr("non — texte uniquement", "no — text only") }));
    if !s.allow_bash.is_empty() {
        v.push(kv(tr("Commandes", "Commands"), &s.allow_bash.join(", ")));
    }
    v
}
