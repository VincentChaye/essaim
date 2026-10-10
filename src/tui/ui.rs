use super::settings::{Lang, tr};
use crate::tf;
use super::app::{App, form_labels, Field, Mode, NodeState, PendingAction, RunView, Tab};
use crate::runtime::executor::NodeStatus;
use crate::runtime::sessions::{Phase, Session};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Tabs, Wrap};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &App) {
    super::settings::set_current(app.lang);
    if app.settings_menu.as_ref().is_some_and(|m| m.first_run) {
        f.render_widget(Clear, f.area());
        draw_settings(f, app);
        return;
    }
    let [tabs, body, help] = Layout::vertical([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)]).areas(f.area());

    let titles: Vec<Line> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let label = if *t == Tab::Run && app.running() {
                format!("{} {}", i + 1, format!("{} {}", t.title(app.lang), SPINNER[app.tick as usize % 10]))
            } else if *t == Tab::Terminal && app.term.as_ref().is_some_and(|s| !s.exited) {
                format!("{} {} ●", i + 1, t.title(app.lang))
            } else {
                format!("{} {}", i + 1, t.title(app.lang))
            };
            Line::raw(label)
        })
        .collect();
    f.render_widget(
        Tabs::new(titles)
            .select(app.tab.index())
            .block(
                Block::bordered()
                    .title(" essaim ")
                    .title_top(Line::from(Span::styled(app.lang.t(" Ctrl+K aide · F10 réglages ", " Ctrl+K help · F10 settings "), Style::new().fg(Color::Yellow))).right_aligned()),
            )
            .highlight_style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        tabs,
    );

    match app.tab {
        Tab::Agents => draw_agents(f, body, app),
        Tab::Company => super::company_view::draw(f, body, app),
        Tab::Run => draw_run(f, body, app),
        Tab::Sessions => draw_sessions(f, body, app),
        Tab::Terminal => draw_terminal(f, body, app),
    }

    if app.help_open {
        let area = centered(f.area(), 92, (f.area().height * 9 / 10).max(8));
        f.render_widget(Clear, area);
        let inner = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), area.height.saturating_sub(2));
        let lines = if app.help_full { super::manual::render_full(inner.width as usize, app.lang) } else { super::manual::render_tab(inner.width as usize, app.tab.index(), app.lang) };
        let max = lines.len().saturating_sub(inner.height as usize).min(u16::MAX as usize) as u16;
        app.help_max.set(max);
        app.help_page.set(inner.height);
        let scroll = app.help_scroll.min(max);
        let end = (scroll as usize + inner.height as usize).min(lines.len());
        let pos = if lines.is_empty() { 0 } else { 100 * end / lines.len() };
        let title = if app.help_full { " man essaim ".to_string() } else { format!(" {} · {} ", app.lang.t("aide", "help"), app.tab.title(app.lang)) };
        let toggle = if app.help_full { app.lang.t("m aide de l'onglet", "m tab help") } else { app.lang.t("m manuel complet", "m full manual") };
        let block = Block::bordered()
            .title(title)
            .title_bottom(format!(" {}-{}/{} ({pos}%) · q {} · {toggle} · ↑↓ space/b ", scroll as usize + 1, end, lines.len(), app.lang.t("quitter", "close")))
            .border_style(Style::new().fg(Color::Yellow));
        f.render_widget(Paragraph::new(lines).scroll((scroll, 0)).block(block), area);
    }

    if let Some(v) = &app.mission_view {
        let area = centered(f.area(), 96, (f.area().height * 9 / 10).max(10));
        f.render_widget(Clear, area);
        let block = Block::bordered()
            .title(tf!(" Mission · {} ", " Mission · {} ", v.title))
            .title_bottom(tr(" c copier · d discuter du compte rendu avec le modèle · ↑↓ défiler · espace/b page · Échap : fermer ", " c copy · d discuss the report with the model · ↑↓ scroll · space/b page · Esc: close "))
            .border_style(Style::new().fg(Color::Cyan));
        f.render_widget(Paragraph::new(v.text.clone()).scroll((v.scroll, 0)).block(block), area);
    }

    if let Some(cp) = app.checkpoint.as_ref().filter(|_| app.tab == Tab::Run) {
        let area = centered(f.area(), 88, (f.area().height * 8 / 10).max(10));
        f.render_widget(Clear, area);
        let block = Block::bordered()
            .title(tr(" Plan consolidé — lancer la réalisation ? ", " Consolidated plan — start realisation? "))
            .title_bottom(tr(" Entrée : lancer (les postes ⚒ modifient le projet) · c : copier · e : modifier le plan avec le boss · Échap : arrêter ici · ↑↓ défiler ", " Enter: start (⚒ seats modify the project) · c: copy · e: revise the plan with the boss · Esc: stop here · ↑↓ scroll "))
            .border_style(Style::new().fg(Color::Yellow));
        f.render_widget(Paragraph::new(cp.plan.clone()).wrap(Wrap { trim: false }).scroll((cp.scroll, 0)).block(block), area);
    }

    if let Some(es) = app.escalation.as_ref().filter(|_| app.tab == Tab::Run) {
        let area = centered(f.area(), 88, (f.area().height * 8 / 10).max(10));
        f.render_widget(Clear, area);
        let block = Block::bordered()
            .title(tr(" Mission bloquée — le CEO n'a pas pu la débloquer ", " Mission blocked — the CEO could not clear it "))
            .title_bottom(tr(" c : copier · e : discuter avec le CEO · Entrée : relancer la correction · Échap : arrêter la mission · ↑↓ défiler ", " c: copy · e: talk with the CEO · Enter: retry the correction · Esc: stop the mission · ↑↓ scroll "))
            .border_style(Style::new().fg(Color::Red));
        f.render_widget(Paragraph::new(es.summary.clone()).wrap(Wrap { trim: false }).scroll((es.scroll, 0)).block(block), area);
    }

    if app.settings_menu.is_some() {
        draw_settings(f, app);
    }

    match &app.mode {
        Mode::EditAgent(form) => draw_form(f, form),
        Mode::EditSeat(form) => draw_seat_form(f, form),
        Mode::ChooseEdit(c) => {
            let area = centered(f.area(), 70, 8);
            f.render_widget(Clear, area);
            let btn = |i: usize, text: &str| {
                let st = if c.sel == i { Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD) } else { Style::new() };
                Span::styled(format!("[ {text} ]"), st)
            };
            let lines = vec![
                Line::from(tf!("Modifier {} :", "Edit {}:", c.label)),
                Line::raw(""),
                Line::from(vec![btn(0, tr("1 Édition simple", "1 Simple edit")), Span::raw("   "), btn(1, tr("2 Avec le modèle", "2 With the model"))]).centered(),
                Line::raw(""),
                Line::from(Span::styled(
                    tr("simple : formulaire ou $EDITOR · modèle : conversation avec Claude (mode plan)", "simple: form or $EDITOR · model: conversation with Claude (plan mode)"),
                    Style::new().fg(Color::DarkGray),
                )),
                Line::from(tr("←→ choisir · Entrée valider · Échap annuler", "←→ pick · Enter confirm · Esc cancel")),
            ];
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::bordered().title(tr(" Mode d'édition ", " Edit mode ")).border_style(Style::new().fg(Color::Cyan))), area);
        }
        Mode::LinkMission(form) => {
            let h = (form.rows.len() as u16 + 5).min(f.area().height.saturating_sub(2)).max(7);
            let area = centered(f.area(), 78, h);
            f.render_widget(Clear, area);
            let mut lines = vec![Line::from(tf!("« {} » ne peut être lancée qu'après la réussite de :", "\"{}\" can only run after these succeed:", form.title)), Line::raw("")];
            for (i, (_, title, on)) in form.rows.iter().enumerate() {
                let st = if i == form.sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                lines.push(Line::styled(format!(" [{}] {title}", if *on { "x" } else { " " }), st));
            }
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::bordered()
                        .title(tr(" Missions à réussir avant ", " Missions to succeed first "))
                        .title_bottom(tr(" ↑↓ · espace cocher · Entrée enregistrer · Échap annuler ", " ↑↓ · space toggle · Enter save · Esc cancel "))
                        .border_style(Style::new().fg(Color::Cyan)),
                ),
                area,
            );
        }
        Mode::ConfirmAction(a) => {
            let users = match a {
                PendingAction::DeleteAgent(name) => app.seats_using_agent(name),
                _ => vec![],
            };
            let mut text = format!("{}\n", a.question());
            if let Some(d) = a.detail(&users) {
                text.push_str(&format!("{d}\n"));
            }
            text.push_str(tr("\no = oui · autre touche = non", "\ny = yes · any other key = no"));
            let area = centered(f.area(), 70, 7);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(text).wrap(Wrap { trim: false }).block(Block::bordered().title(tr(" Confirmation ", " Confirmation ")).border_style(Style::new().fg(Color::Red))),
                area,
            );
        }
        Mode::Normal => {}
    }

    let (text, style) = if !app.status.is_empty() {
        (app.status.clone(), Style::new().fg(Color::Yellow))
    } else {
        (help_text(app).to_string(), Style::new().fg(Color::DarkGray))
    };
    f.render_widget(Paragraph::new(text).style(style), help);
}

fn draw_settings(f: &mut Frame, app: &App) {
    let Some(m) = &app.settings_menu else { return };
    let area = centered(f.area(), 50, 9);
    f.render_widget(Clear, area);
    let mut lines = vec![Line::raw(if m.first_run { "Choisis ta langue · Choose your language" } else { "Langue · Language" }), Line::raw("")];
    for (i, l) in Lang::ALL.iter().enumerate() {
        let style = if i == m.sel { Style::new().add_modifier(Modifier::BOLD).bg(Color::DarkGray) } else { Style::new() };
        lines.push(Line::styled(format!(" {} {}. {} ", if i == m.sel { "▸" } else { " " }, i + 1, l.name()), style));
    }
    let hint = if m.first_run { " ↑↓ · Entrée / Enter " } else { " ↑↓ · Entrée / Enter · Échap / Esc " };
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Réglages · Settings ").title_bottom(hint).border_style(Style::new().fg(Color::Cyan))),
        area,
    );
}

fn help_text(app: &App) -> &'static str {
    if app.lang == Lang::En {
        return help_text_en(app);
    }
    match (&app.mode, app.tab) {
        (Mode::EditAgent(_), _) => "Tab champ suivant · ←→ choisir le modèle · Ctrl+S enregistrer · Esc annuler",
        (Mode::EditSeat(_), _) => "↑↓/Tab changer de champ · ←→ choisir · Entrée sur un bouton · Ctrl+S enregistrer · Esc annuler",
        (Mode::ChooseEdit(_), _) => "←→ choisir · 1 simple · 2 avec le modèle · Entrée valider · Échap annuler",
        (Mode::LinkMission(_), _) => "↑↓ choisir · espace cocher · Entrée enregistrer · Échap annuler",
        (Mode::ConfirmAction(_), _) => "o confirmer · autre touche (n, Échap) annuler",
        (_, Tab::Agents) => "↑↓ choisir · n créer avec l'assistant · N formulaire · e éditer · D supprimer · q quitter",
        (_, Tab::Company) => "←↑↓→ zones · Entrée entrer · Échap sortir · dans une zone : e éditer (a ajouter · x retirer dans l'organigramme) · r lancer · Ctrl+K toutes les touches",
        (_, Tab::Run) if app.show_history => "↑↓ choisir · Entrée ouvrir · D supprimer · h retour · Tab onglet · q quitter",
        (_, Tab::Run) => "↑↓ nœud · c copier le journal (C tous) · s annuler le run · h historique · Tab onglet · q quitter",
        (_, Tab::Terminal) => match &app.term {
            None => "n dans Agents ou Entreprise ouvre un assistant ici · Tab onglet · q quitter",
            Some(t) if t.exited => "session terminée — appuyez sur une touche pour fermer",
            Some(_) if app.term_focus => "F1-F5 / Alt+1-5 / Ctrl+PgUp-PgDn : changer d'onglet · Ctrl+\\ : touches de navigation",
            Some(_) => "Entrée reprendre la session · x terminer · Tab / F1-F5 onglet · q quitter",
        },
        (_, Tab::Sessions) => "↑↓ choisir · a attacher · s arrêter · x supprimer · r rafraîchir · Tab onglet · q quitter",
    }
}

fn help_text_en(app: &App) -> &'static str {
    match (&app.mode, app.tab) {
        (Mode::EditAgent(_), _) => "Tab next field · ←→ pick model · Ctrl+S save · Esc cancel",
        (Mode::EditSeat(_), _) => "↑↓/Tab change field · ←→ pick · Enter on a button · Ctrl+S save · Esc cancel",
        (Mode::ChooseEdit(_), _) => "←→ pick · 1 simple · 2 with the model · Enter confirm · Esc cancel",
        (Mode::LinkMission(_), _) => "↑↓ pick · space toggle · Enter save · Esc cancel",
        (Mode::ConfirmAction(_), _) => "y confirm · any other key (n, Esc) cancels",
        (_, Tab::Agents) => "↑↓ pick · n create with assistant · N form · e edit · D delete · q quit",
        (_, Tab::Company) => "←↑↓→ zones · Enter enter · Esc leave · in a zone: e edit (a add · x remove in the org chart) · r run · Ctrl+K all keys",
        (_, Tab::Run) if app.show_history => "↑↓ pick · Enter open · D delete · h back · Tab tab · q quit",
        (_, Tab::Run) => "↑↓ node · c copy log (C all) · s cancel run · h history · Tab tab · q quit",
        (_, Tab::Terminal) => match &app.term {
            None => "n in Agents or Company opens an assistant here · Tab tab · q quit",
            Some(t) if t.exited => "session ended — press any key to close",
            Some(_) if app.term_focus => "F1-F5 / Alt+1-5 / Ctrl+PgUp-PgDn: switch tab · Ctrl+\\: navigation keys",
            Some(_) => "Enter resume session · x end · Tab / F1-F5 tab · q quit",
        },
        (_, Tab::Sessions) => "↑↓ pick · a attach · s stop · x delete · r refresh · Tab tab · q quit",
    }
}

fn centered(area: Rect, pct_w: u16, h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    Rect::new(area.x + (area.width - w) / 2, area.y + area.height.saturating_sub(h) / 2, w, h.min(area.height))
}

fn list_state(sel: usize, len: usize) -> ListState {
    ListState::default().with_selected((len > 0).then_some(sel))
}

fn highlight() -> Style {
    Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
}

fn draw_agents(f: &mut Frame, area: Rect, app: &App) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(area);
    let items: Vec<ListItem> = app.agents.iter().map(|a| ListItem::new(a.name.clone())).collect();
    let title = format!(" Agents ({}) ", app.agents.len());
    f.render_stateful_widget(
        List::new(items).block(Block::bordered().title(title)).highlight_style(highlight()).highlight_symbol("▸ "),
        left,
        &mut list_state(app.agent_sel, app.agents.len()),
    );
    let detail = match app.agents.get(app.agent_sel) {
        None => vec![Line::raw(tr("Aucun agent. Appuyez sur n : un assistant vous aide à le concevoir (N = formulaire manuel).", "No agent. Press n: an assistant helps you design one (N = manual form)."))],
        Some(a) => {
            let mut v = vec![
                field_line(tr("Nom", "Name"), &a.name),
                field_line("Description", &a.description),
                field_line(tr("Modèle", "Model"), a.model.as_deref().unwrap_or(tr("(défaut)", "(default)"))),
                field_line(tr("Outils", "Tools"), &a.tools.as_ref().map(|t| t.join(", ")).unwrap_or_else(|| tr("(tous)", "(all)").into())),
                Line::raw(""),
            ];
            v.extend(a.prompt.lines().map(|l| Line::raw(l.to_string())));
            v
        }
    };
    let block = Block::bordered().title(tr(" Détail ", " Details "));
    f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }).block(block), right);
}

fn field_line(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{k} : "), Style::new().fg(Color::Cyan)), Span::raw(v.to_string())])
}

fn draw_form(f: &mut Frame, form: &super::app::AgentForm) {
    let area = centered(f.area(), 80, (f.area().height * 8 / 10).max(18));
    f.render_widget(Clear, area);
    let title = if form.original.is_some() { tr(" Éditer l'agent ", " Edit agent ") } else { tr(" Nouvel agent ", " New agent ") };
    let outer = Block::bordered().title(title).border_style(Style::new().fg(Color::Cyan));
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Length(3), Constraint::Length(3), Constraint::Min(4)]).split(inner);
    for (i, field) in form.fields.iter().enumerate() {
        let focused = form.focus == i;
        match field {
            Field::Text(b) => b.render(f, rows[i], form_labels()[i], focused),
            Field::Choice { options, idx } => {
                f.render_widget(choice_widget(options, *idx, focused, form_labels()[i], tr("(défaut)", "(default)")), rows[i]);
            }
        }
    }
}

fn choice_widget<'a>(options: &[String], idx: usize, focused: bool, title: &'a str, empty: &str) -> Paragraph<'a> {
    let border = if focused { Color::Cyan } else { Color::DarkGray };
    let mut spans = vec![];
    for (j, o) in options.iter().enumerate() {
        let label = if o.is_empty() { empty.to_string() } else { o.clone() };
        let style = if j == idx {
            Style::new().fg(Color::Black).bg(if focused { Color::Cyan } else { Color::Gray }).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        spans.push(Span::styled(format!(" {label} "), style));
        spans.push(Span::raw(" "));
    }
    Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }).block(Block::bordered().title(title).border_style(Style::new().fg(border)))
}

fn draw_seat_form(f: &mut Frame, form: &super::app::SeatForm) {
    // Champs (3 lignes chacun) + pied : boutons et ligne de vérification.
    let area = centered(f.area(), 80, (form.fields.len() as u16 * 3 + 2 + 3).min(f.area().height));
    f.render_widget(Clear, area);
    let title = if form.original.is_some() { tr(" Éditer le poste ", " Edit seat ") } else { tr(" Nouveau poste ", " New seat ") };
    let outer = Block::bordered().title(title).border_style(Style::new().fg(Color::Cyan));
    let full = outer.inner(area);
    f.render_widget(outer, area);
    let [inner, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(full);
    // Fenêtre défilante : le champ ciblé reste visible, une barre à droite indique la position.
    let total = form.fields.len();
    let visible = ((inner.height / 3) as usize).clamp(1, total);
    let focus_field = form.focus.min(total - 1);
    let first = (focus_field + 1).saturating_sub(visible).min(total - visible);
    let body = Rect { width: inner.width.saturating_sub(1), ..inner };
    let rows = Layout::vertical(vec![Constraint::Length(3); visible]).split(body);
    for (slot, i) in (first..first + visible).enumerate() {
        let focused = form.focus == i;
        match &form.fields[i] {
            Field::Text(b) => b.render(f, rows[slot], &form.labels[i], focused),
            Field::Choice { options, idx } => f.render_widget(choice_widget(options, *idx, focused, &form.labels[i], if i == 3 { tr("(modèle de l'agent)", "(agent's model)") } else { tr("(aucun : boss)", "(none: boss)") }), rows[slot]),
        }
    }
    if visible < total {
        let mut state = ScrollbarState::new(total - visible + 1).position(first);
        f.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight), inner, &mut state);
    }
    let btn = |label: &str, at: usize, color: Color| {
        let st = if form.focus == at { Style::new().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD) } else { Style::new().fg(color) };
        Span::styled(format!(" [ {label} ] "), st)
    };
    let buttons = Line::from(vec![
        btn(tr("Valider les changements", "Apply changes"), total, Color::Green),
        Span::raw("  "),
        btn(tr("Annuler", "Cancel"), total + 1, Color::Red),
    ]);
    let ask = match form.confirm {
        Some(true) => Line::styled(tr("Enregistrer ces changements ? o = oui · autre touche = revenir", "Save these changes? o = yes · any other key = back"), Style::new().fg(Color::Yellow)),
        Some(false) => Line::styled(tr("Abandonner les changements ? o = oui · autre touche = revenir", "Discard the changes? o = yes · any other key = back"), Style::new().fg(Color::Yellow)),
        None => Line::raw(""),
    };
    f.render_widget(Paragraph::new(vec![Line::raw(""), buttons.centered(), ask.centered()]), footer);
}

fn status_span(state: &NodeState, tick: u64) -> Span<'static> {
    match state {
        NodeState::Waiting => Span::styled("·", Style::new().fg(Color::DarkGray)),
        NodeState::Running => Span::styled(SPINNER[tick as usize % 10], Style::new().fg(Color::Cyan)),
        NodeState::Done(NodeStatus::Succeeded) => Span::styled("✔", Style::new().fg(Color::Green)),
        NodeState::Done(NodeStatus::Failed(_)) | NodeState::Done(NodeStatus::Cancelled) => Span::styled("✖", Style::new().fg(Color::Red)),
        NodeState::Done(NodeStatus::Blocked(_)) => Span::styled("⚠", Style::new().fg(Color::Yellow)),
        NodeState::Done(NodeStatus::Skipped(_)) => Span::styled("⊘", Style::new().fg(Color::Yellow)),
    }
}

fn draw_history(f: &mut Frame, area: Rect, app: &App) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
    let items: Vec<ListItem> = app
        .history
        .iter()
        .map(|h| {
            let (icon, color) = match h.finished {
                Some(true) => ("✔", Color::Green),
                Some(false) => ("✖", Color::Red),
                None => ("…", Color::Yellow),
            };
            ListItem::new(Line::from(vec![
                Span::styled(icon, Style::new().fg(color)),
                Span::raw(format!(" {}  {}", h.started_label(), h.pipeline)),
            ]))
        })
        .collect();
    f.render_stateful_widget(
        List::new(items).block(Block::bordered().title(tf!(" Historique ({}) ", " History ({}) ", app.history.len()))).highlight_style(highlight()).highlight_symbol("▸ "),
        left,
        &mut list_state(app.hist_sel, app.history.len()),
    );
    let lines: Vec<Line> = match app.history.get(app.hist_sel) {
        None => vec![Line::raw(tr("Aucun run enregistré. Lancez une mission depuis l'onglet Entreprise.", "No recorded run. Launch a mission from the Company tab."))],
        Some(h) => {
            let etat = match h.finished {
                Some(true) => tr("réussi", "succeeded"),
                Some(false) => tr("échec", "failed"),
                None => tr("interrompu", "interrupted"),
            };
            let mut v = vec![
                field_line(tr("Mission", "Mission"), &h.pipeline),
                field_line("Date", &h.started_label()),
                field_line(tr("Résultat", "Result"), etat),
                field_line("Tokens", &h.tokens.to_string()),
                Line::raw(""),
            ];
            for n in &h.nodes {
                let (icon, color) = match &n.status {
                    Some(NodeStatus::Succeeded) => ("✔", Color::Green),
                    Some(NodeStatus::Failed(_)) | Some(NodeStatus::Cancelled) => ("✖", Color::Red),
                    Some(NodeStatus::Blocked(_)) => ("⚠", Color::Yellow),
                    Some(NodeStatus::Skipped(_)) => ("⊘", Color::Yellow),
                    None => ("·", Color::DarkGray),
                };
                v.push(Line::from(vec![Span::styled(icon, Style::new().fg(color)), Span::raw(format!(" {}{} [{}]", "  ".repeat(n.depth), if n.label.is_empty() { &n.id } else { &n.label }, n.agent))]));
            }
            v.push(Line::raw(""));
            v.push(Line::styled(tr("Entrée  le détail et les journaux", "Enter (left pane, once inside): open details and logs"), Style::new().fg(Color::DarkGray)));
            v
        }
    };
    let block = Block::bordered().title(tr(" Aperçu ", " Preview "));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), right);
}

fn draw_run(f: &mut Frame, area: Rect, app: &App) {
    if app.show_history {
        return draw_history(f, area, app);
    }
    let Some(run) = &app.run else {
        f.render_widget(
            Paragraph::new(tr("Aucun run. Choisissez une mission dans l'onglet Entreprise et appuyez sur r.\n\nh : historique des runs précédents.", "No run. Pick a mission in the Company tab and press r.\n\nh: history of previous runs."))
                .block(Block::bordered().title(" Run ")),
            area,
        );
        return;
    };
    let [head, main] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    let (label, color) = match run.finished {
        None => (tr("en cours", "running").to_string(), Color::Cyan),
        Some(true) => (tr("terminé ✔", "done ✔").to_string(), Color::Green),
        Some(false) => (tr("terminé avec erreurs ✖", "done with errors ✖").to_string(), Color::Red),
    };
    let tokens: u64 = if run.finished.is_some() { run.tokens } else { run.nodes.iter().map(|n| n.tokens).sum() };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{} — ", run.name), Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(label, Style::new().fg(color)),
            Span::raw(tf!("   {tokens} tokens", "   {tokens} tokens")),
        ]))
        .block(Block::bordered().title(" Run ")),
        head,
    );

    // Un run d'entreprise a des libellés indentés plus longs : colonne des nœuds élargie.
    let left_pct = if run.nodes.iter().any(|n| n.depth > 0) { 46 } else { 30 };
    let [left, right] = Layout::horizontal([Constraint::Percentage(left_pct), Constraint::Percentage(100 - left_pct)]).areas(main);
    draw_nodes(f, left, run, app);
    let log_block = Block::bordered().title(format!(" Journal : {} ", run.nodes.get(run.sel).map(|n| n.label.as_str()).unwrap_or("-")));
    let inner = log_block.inner(right);
    let lines: Vec<Line> = run.nodes.get(run.sel).map(|n| n.log.iter().map(|l| Line::raw(l.clone())).collect()).unwrap_or_default();
    let scroll = tail_scroll(&lines, inner);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(log_block), right);
}

fn draw_nodes(f: &mut Frame, area: Rect, run: &RunView, app: &App) {
    let tick = app.tick;
    let items: Vec<ListItem> = run
        .nodes
        .iter()
        .map(|n| ListItem::new(Line::from(vec![status_span(&n.state, tick), Span::raw(format!(" {}{}  ", "  ".repeat(n.depth), n.label)), Span::styled(n.agent.clone(), Style::new().fg(Color::DarkGray))])))
        .collect();
    f.render_stateful_widget(
        List::new(items).block(Block::bordered().title(tr(" Nœuds ", " Nodes "))).highlight_style(highlight()).highlight_symbol("▸ "),
        area,
        &mut list_state(run.sel, run.nodes.len()),
    );
}

fn age(started_ms: Option<u64>) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    match started_ms {
        Some(t) if now >= t => {
            let s = (now - t) / 1000;
            if s < 60 {
                format!("{s} s")
            } else if s < 3600 {
                format!("{} min", s / 60)
            } else {
                format!("{} h", s / 3600)
            }
        }
        _ => "?".into(),
    }
}

fn phase_span(s: &Session, tick: u64) -> Span<'static> {
    match s.phase() {
        Phase::Working => Span::styled(SPINNER[tick as usize % 10], Style::new().fg(Color::Cyan)),
        Phase::Done => Span::styled("✔", Style::new().fg(Color::Green)),
        Phase::Failed(_) => Span::styled("✖", Style::new().fg(Color::Red)),
        Phase::Other(_) => Span::styled("?", Style::new().fg(Color::Yellow)),
    }
}

/// Défilement pour suivre la fin d'un journal : on compte les lignes après retour à la ligne.
fn tail_scroll(lines: &[Line], inner: Rect) -> u16 {
    let w = inner.width.max(1) as usize;
    let total: usize = lines.iter().map(|l| (l.width().max(1) + w - 1) / w).sum();
    total.saturating_sub(inner.height as usize) as u16
}

fn draw_sessions(f: &mut Frame, area: Rect, app: &App) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(area);
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|s| {
            let mut spans = vec![phase_span(s, app.tick), Span::raw(format!(" {}", s.label()))];
            if s.is_essaim() {
                spans.push(Span::styled("  essaim", Style::new().fg(Color::DarkGray)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    f.render_stateful_widget(
        List::new(items)
            .block(Block::bordered().title(tf!(" Sessions background ({}) ", " Background sessions ({}) ", app.sessions.len())))
            .highlight_style(highlight())
            .highlight_symbol("▸ "),
        left,
        &mut list_state(app.sess_sel, app.sessions.len()),
    );

    let [head, log] = Layout::vertical([Constraint::Length(7), Constraint::Min(0)]).areas(right);
    let mut lines: Vec<Line> = vec![];
    if let Some(e) = &app.sess_error {
        lines.push(Line::styled(format!("⚠ {e}"), Style::new().fg(Color::Red)));
    }
    match app.selected_session() {
        None => {
            lines.push(Line::raw(tr("Aucune session background.", "No background session.")));
            lines.push(Line::raw(tr("Elles apparaissent ici quand vous lancez `claude --bg`", "They appear here when you run `claude --bg`")));
                        lines.push(Line::styled(tr("Sessions cloud : non suivies (décompte abonnement non vérifié).", "Cloud sessions: not tracked (subscription accounting unverified)."), Style::new().fg(Color::DarkGray)));
        }
        Some(s) => {
            lines.push(field_line(tr("Nom", "Name"), &s.label()));
            lines.push(field_line("Id", s.id.as_deref().unwrap_or("?")));
            lines.push(field_line(tr("État", "State"), &format!("{} / {}", s.state.as_deref().unwrap_or("?"), s.status.as_deref().unwrap_or("?"))));
            lines.push(field_line(tr("Dossier", "Folder"), &s.cwd));
            lines.push(field_line(tr("Démarrée", "Started"), &tf!("il y a {}", "{} ago", age(s.started_at))));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::bordered().title(" Session ")), head);

    let block = Block::bordered().title(tr(" Transcription ", " Transcript "));
    let inner = block.inner(log);
    let tl: Vec<Line> = app.sess_detail.iter().map(|l| Line::raw(l.clone())).collect();
    let scroll = tail_scroll(&tl, inner);
    f.render_widget(Paragraph::new(tl).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(block), log);
}

fn draw_terminal(f: &mut Frame, area: Rect, app: &App) {
    let Some(t) = &app.term else {
        f.render_widget(
            Paragraph::new(tr("Aucune session Claude ouverte.\n\nDans l'onglet Agents ou Entreprise, n ouvre un assistant de création ici.\nDans l'onglet Sessions, a rattache une session background.", "No Claude session open.\n\nIn the Agents or Company tab, n opens a creation assistant here.\nIn the Sessions tab, a attaches a background session."))
                .block(Block::bordered().title(" Claude ")),
            area,
        );
        return;
    };
    let cursor = t.with_screen(|s| super::term::render(f, area, s));
    if app.term_focus && !t.exited {
        if let Some((x, y)) = cursor {
            f.set_cursor_position((x, y));
        }
    }
    if t.exited {
        let code = t.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
        let banner = Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1);
        f.render_widget(
            Paragraph::new(tf!(" Session terminée (code {code}) — appuyez sur une touche pour fermer ", " Session ended (code {code}) — press any key to close "))
                .style(Style::new().fg(Color::Black).bg(Color::Yellow)),
            banner,
        );
    }
}
