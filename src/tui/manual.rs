//! Manuel d'utilisation au format d'une page `man` : titre `ESSAIM(1)`, sections en majuscules
//! (NOM, SYNOPSIS, DESCRIPTION…), corps indenté, listes à retrait suspendu. La mise en page est
//! recalculée à la largeur du terminal.

use ratatui::style::{Color, Modifier, Style};
use super::settings::Lang;
use ratatui::text::{Line, Span};

/// Retrait du corps de section, comme `man` (7 colonnes).
const BODY: usize = 7;
/// Largeur de la colonne des termes dans les listes.
const TERM: usize = 16;

pub enum Entry {
    Section(&'static str),
    Para(&'static str),
    /// Terme (touche, commande, variable) et sa description.
    Item(&'static str, &'static str),
    /// Ligne de code ou d'exemple, non reformatée.
    Code(&'static str),
    Blank,
}

use Entry::*;

pub const MANUAL: &[Entry] = &[
    Section("NOM"),
    Para("essaim — concevoir et exécuter des équipes d'agents Claude Code, organisées en entreprise."),
    Blank,
    Section("SYNOPSIS"),
    Code("essaim"),
    Code("essaim init | agents"),
    Code("essaim company show|check <entreprise.toml>"),
    Code("essaim company run <entreprise.toml> (--brief \"…\" | --mission <id>) [--yes]"),
    Blank,
    Section("DESCRIPTION"),
    Para("Un agent est un fichier .claude/agents/<nom>.md. On les assemble en entreprise (organigramme boss, VP, managers, workers). Tout passe par le CLI claude et ton abonnement : les clés API sont ignorées et les coûts affichés sont notionnels."),
    Para("Sans argument, essaim ouvre l'interface en onglets, dans le dossier courant (racine du projet)."),
    Blank,
    Section("NAVIGATION"),
    Item("Tab, Maj+Tab", "onglet suivant, précédent."),
    Item("1 … 5", "va à l'onglet numéroté."),
    Item("F1 … F5", "idem ; fonctionne aussi dans la session Claude."),
    Item("Alt+1 … Alt+5", "idem (AZERTY : Alt+& é \" ' ()."),
    Item("Ctrl+PgUp/PgDn", "onglet précédent, suivant."),
    Item("↑ ↓, j k", "choisir dans une liste."),
    Item("Ctrl+K", "ouvre ou ferme l'aide de l'onglet courant (m : manuel complet)."),
    Item("F10", "réglages (langue), mémorisés d'un lancement à l'autre."),
    Item("q, Ctrl+C", "quitte et annule le run en cours."),
    Blank,
    Section("ONGLET 1 : AGENTS"),
    Item("n", "crée un agent avec l'assistant (conversation en mode plan)."),
    Item("N", "crée un agent avec un formulaire."),
    Item("e, Entrée", "édite l'agent choisi."),
    Item("D", "supprime l'agent (confirmation)."),
    Para("Dans le formulaire : Tab champ suivant, ← → choisit le modèle, Ctrl+S enregistre, Échap annule."),
    Blank,
    Section("ONGLET 2 : ENTREPRISE"),
    Para("Organigramme en arbre avec niveaux N, N-1, N-2… et intitulés de poste. Un poste marqué ⚒ réalise dans le projet, dans son périmètre ; les autres produisent du texte. Une mission se déroule en trois temps : conception en cascade (le boss délègue aux VP, qui délèguent aux managers, qui répartissent entre les workers, puis les rapports remontent), plan consolidé à valider, réalisation."),
    Item("n", "crée une entreprise avec l'assistant : organigramme conçu en conversation, agents manquants créés."),
    Item("a", "ajoute un poste sous le poste choisi (formulaire : id, intitulé, agent, supérieur, expertise, ⚒, commandes shell)."),
    Item("e", "édite le poste choisi dans le même formulaire ; renommer un id met à jour ses subordonnés."),
    Item("x", "retire le poste choisi (confirmation) ; ses subordonnés remontent d'un cran. Le boss ne part que seul."),
    Item("m", "cadre une mission : conversation avec le boss (mode plan) qui écrit le brief."),
    Item("r", "lance la mission choisie : conception, plan à valider, réalisation. Une mission dont la conception est terminée (annulée ou refusée ensuite) reprend à la réalisation, sans rejouer la conception."),
    Item("R", "relance la mission choisie depuis le début, sans reprise."),
    Item("t, T", "mission suivante, précédente."),
    Item("Entrée, o", "ouvre la mission choisie (brief, plan, rapport) en lecture, même une ancienne."),
    Item("c, C", "entreprise suivante, précédente."),
    Item("E", "édite le fichier TOML dans $EDITOR (les formulaires de poste réécrivent le fichier : les commentaires TOML sont perdus)."),
    Blank,
    Section("ONGLET 3 : RUN"),
    Item("↑ ↓", "choisit un nœud (son journal s'affiche)."),
    Item("s", "annule le run en cours."),
    Item("h", "historique ; Entrée rouvre un run, D le supprime."),
    Item("Entrée, Échap", "au plan consolidé : lance la réalisation, ou arrête."),
    Item("e", "au plan consolidé : ouvre une conversation avec le boss pour modifier le plan (il réécrit conception.md) ; en quittant (/exit), le plan relu s'affiche et Entrée lance la réalisation avec lui."),
    Blank,
    Section("ONGLET 4 : SESSIONS"),
    Item("a", "attache la session background (dans l'onglet 5)."),
    Item("s", "arrête la session."),
    Item("x", "supprime la session (confirmation)."),
    Item("r", "rafraîchit la liste."),
    Blank,
    Section("ONGLET 5 : CLAUDE"),
    Para("Un vrai Claude Code dans un terminal intégré : toutes les touches lui sont transmises, y compris q et Ctrl+C."),
    Item("Ctrl+\\", "rend les touches de navigation à essaim ; la session continue."),
    Item("Entrée, i", "reprend la session."),
    Item("x", "ferme la session."),
    Item("/exit", "termine la session (commande de Claude)."),
    Blank,
    Section("COMMANDES"),
    Item("init", "crée des agents et une entreprise d'exemple, sans écraser."),
    Item("agents", "liste les agents."),
    Item("company show", "affiche l'organigramme et l'estimation d'appels."),
    Item("company check", "valide l'organigramme."),
    Item("company run", "exécute une mission ; --yes valide sans demander."),
    Blank,
    Section("FICHIERS"),
    Item(".claude/agents/", "un fichier .md par agent (format standard de Claude Code)."),
    Item(".claude/companies/", "entreprises TOML (postes, hiérarchie, périmètres)."),
    Item(".essaim/runs/", "historique des runs."),
    Item(".essaim/missions/", "brief, conception.md et rapport.md de chaque mission."),
    Blank,
    Section("ENVIRONNEMENT"),
    Item("VISUAL, EDITOR", "éditeur ouvert par la touche e (vi par défaut)."),
    Item("ESSAIM_CLAUDE_BIN", "remplace la commande claude (tests)."),
    Item("ANTHROPIC_API_KEY", "ignorée : retirée de chaque processus lancé."),
    Blank,
    Section("CODE DE SORTIE"),
    Item("0", "succès."),
    Item("1", "un nœud ou un poste a échoué, ou la validation a échoué."),
    Blank,
    Section("EXEMPLES"),
    Code("essaim init && essaim"),
    Code("essaim company run .claude/companies/acme-studio.toml --brief \"Ajouter une page d'accueil\""),
    Blank,
    Section("VOIR AUSSI"),
    Para("claude(1), README.md"),
];

const NAV_EN: &[Entry] = &[
    Section("NAVIGATION"),
    Item("Tab, Shift+Tab", "next, previous tab."),
    Item("1 … 5", "go to the numbered tab."),
    Item("F1 … F5", "same; also works inside the Claude session."),
    Item("Alt+1 … Alt+5", "same (AZERTY: Alt+& é \" ' ()."),
    Item("Ctrl+PgUp/PgDn", "previous, next tab."),
    Item("↑ ↓, j k", "pick in a list."),
    Item("Ctrl+K", "open or close the help for the current tab (m: full manual)."),
    Item("F10", "settings (language), remembered between launches."),
    Item("q, Ctrl+C", "quit and cancel the current run."),
];

const TABS_EN: [&[Entry]; 5] = [
    &[
        Section("TAB 1: AGENTS"),
        Item("n", "create an agent with the assistant (plan-mode conversation)."),
        Item("N", "create an agent with a form."),
        Item("e, Enter", "edit the selected agent."),
        Item("D", "delete the agent (confirmation)."),
        Para("In the form: Tab next field, ← → pick the model, Ctrl+S save, Esc cancel."),
    ],
    &[
        Section("TAB 2: COMPANY"),
        Para("Org chart as a tree with levels N, N-1, N-2… and job titles. A seat marked ⚒ builds in the project, within its scope; the others produce text. A mission has three steps: cascading design (boss delegates to VPs, who delegate to managers, who split the work among workers, then reports flow back up), a consolidated plan to approve, then realisation."),
        Item("n", "create a company with the assistant: org chart designed in conversation, missing agents created."),
        Item("a", "add a seat under the selected one (form: id, title, agent, supervisor, expertise, ⚒, shell commands)."),
        Item("e", "edit the selected seat in the same form; renaming an id updates its subordinates."),
        Item("x", "remove the selected seat (confirmation); its subordinates move up. The boss can only leave alone."),
        Item("m", "frame a mission: conversation with the boss (plan mode) who writes the brief."),
        Item("r", "run the selected mission: design, plan to approve, realisation. A mission whose design is finished (cancelled or refused afterwards) resumes at realisation without replaying the design."),
        Item("R", "run the selected mission again from the start, no resume."),
        Item("t, T", "next, previous mission."),
        Item("Enter, o", "open the selected mission (brief, plan, report) read-only, even an old one."),
        Item("c, C", "next, previous company."),
        Item("E", "edit the TOML file in $EDITOR (seat forms rewrite the file: TOML comments are lost)."),
    ],
    &[
        Section("TAB 3: RUN"),
        Item("↑ ↓", "pick a node (its log is shown)."),
        Item("s", "cancel the current run."),
        Item("h", "history; Enter reopens a run, D deletes it."),
        Item("Enter, Esc", "at the consolidated plan: start realisation, or stop."),
        Item("e", "at the consolidated plan: open a conversation with the boss to change the plan (it rewrites conception.md); on exit (/exit) the reloaded plan is shown and Enter starts realisation with it."),
    ],
    &[
        Section("TAB 4: SESSIONS"),
        Item("a", "attach the background session (in tab 5)."),
        Item("s", "stop the session."),
        Item("x", "delete the session (confirmation)."),
        Item("r", "refresh the list."),
    ],
    &[
        Section("TAB 5: CLAUDE"),
        Para("A real Claude Code in an embedded terminal: every key is passed to it, including q and Ctrl+C."),
        Item("Ctrl+\\", "give the navigation keys back to essaim; the session keeps running."),
        Item("Enter, i", "resume the session."),
        Item("x", "close the session."),
        Item("/exit", "end the session (a Claude command)."),
    ],
];

/// Aide de l'onglet courant : navigation + section de l'onglet.
fn tab_entries(tab: usize, lang: Lang) -> Vec<&'static Entry> {
    let mut out: Vec<&'static Entry> = vec![];
    let sections: Vec<&'static [Entry]> = match lang {
        Lang::En => vec![NAV_EN, TABS_EN[tab.min(4)]],
        Lang::Fr => {
            let prefix = format!("ONGLET {}", tab + 1);
            ["NAVIGATION", prefix.as_str()]
                .iter()
                .filter_map(|p| {
                    let start = MANUAL.iter().position(|e| matches!(e, Section(t) if t.starts_with(p)))?;
                    let end = MANUAL[start + 1..].iter().position(|e| matches!(e, Section(_))).map_or(MANUAL.len(), |i| start + 1 + i);
                    Some(&MANUAL[start..end])
                })
                .collect()
        }
    };
    for sec in sections {
        out.extend(sec.iter().filter(|e| !matches!(e, Blank)));
        out.push(&Blank);
    }
    out
}

const HEAD_EN: &[Entry] = &[
    Section("NAME"),
    Para("essaim — design and run teams of Claude Code agents, organised as a company."),
    Blank,
    Section("SYNOPSIS"),
    Code("essaim"),
    Code("essaim init | agents"),
    Code("essaim company show|check <company.toml>"),
    Code("essaim company run <company.toml> (--brief \"…\" | --mission <id>) [--yes]"),
    Blank,
    Section("DESCRIPTION"),
    Para("An agent is a file .claude/agents/<name>.md. They are assembled into a company (org chart: boss, VPs, managers, workers). Everything goes through the claude CLI and your subscription: API keys are ignored and the costs shown are notional."),
    Para("With no argument, essaim opens the tabbed interface in the current folder (project root). Models are briefed in English and told the language you chose (F10)."),
    Blank,
];

const TAIL_EN: &[Entry] = &[
    Section("COMMANDS"),
    Item("init", "create example agents and company, without overwriting."),
    Item("agents", "list the agents."),
    Item("company show", "show the org chart and the call estimate."),
    Item("company check", "validate the org chart."),
    Item("company run", "run a mission; --yes approves without asking."),
    Blank,
    Section("FILES"),
    Item(".claude/agents/", "one .md file per agent (standard Claude Code format)."),
    Item(".claude/companies/", "TOML companies (seats, hierarchy, scopes)."),
    Item(".essaim/runs/", "run history."),
    Item(".essaim/missions/", "brief, conception.md and rapport.md of each mission."),
    Blank,
    Section("ENVIRONMENT"),
    Item("VISUAL, EDITOR", "editor opened by the e key (vi by default)."),
    Item("ESSAIM_CLAUDE_BIN", "replaces the claude command (tests)."),
    Item("ESSAIM_CONFIG_DIR", "folder of settings.toml (language), instead of the platform default."),
    Item("ANTHROPIC_API_KEY", "ignored: removed from every launched process."),
    Blank,
    Section("EXIT STATUS"),
    Item("0", "success."),
    Item("1", "a node or seat failed, or validation failed."),
    Blank,
    Section("EXAMPLES"),
    Code("essaim init && essaim"),
    Code("essaim company run .claude/companies/acme-studio.toml --brief \"Add a landing page\""),
    Blank,
    Section("SEE ALSO"),
    Para("claude(1), README.md"),
];

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let (mut out, mut cur) = (vec![], String::new());
    for w in text.split_whitespace() {
        // Mot plus long qu'une ligne (chemin, URL) : coupé en tronçons.
        let chars: Vec<char> = w.chars().collect();
        let pieces: Vec<String> = if chars.len() > width { chars.chunks(width).map(|c| c.iter().collect()).collect() } else { vec![w.to_string()] };
        for p in pieces {
            let need = cur.chars().count() + usize::from(!cur.is_empty()) + p.chars().count();
            if !cur.is_empty() && need > width {
                out.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(&p);
        }
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

/// Manuel complet dans la langue choisie.
pub fn render_full(width: usize, lang: Lang) -> Vec<Line<'static>> {
    match lang {
        Lang::Fr => render(width),
        Lang::En => {
            let mut e: Vec<&Entry> = HEAD_EN.iter().chain(NAV_EN.iter()).collect();
            e.push(&Blank);
            for t in TABS_EN {
                e.extend(t.iter());
                e.push(&Blank);
            }
            e.extend(TAIL_EN.iter());
            layout(&e, width, "User manual")
        }
    }
}

/// Manuel complet (français) mis en page pour `width` colonnes.
pub fn render(width: usize) -> Vec<Line<'static>> {
    layout(&MANUAL.iter().collect::<Vec<_>>(), width, "Manuel d'utilisation")
}

/// Aide limitée à l'onglet `tab` (0–5), dans la langue choisie.
pub fn render_tab(width: usize, tab: usize, lang: Lang) -> Vec<Line<'static>> {
    layout(&tab_entries(tab, lang), width, lang.t("Aide de l'onglet", "Tab help"))
}

fn layout(entries: &[&Entry], width: usize, mid: &str) -> Vec<Line<'static>> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(Color::DarkGray);
    let mut out: Vec<Line<'static>> = vec![];

    // En-tête et pied, comme une page man : ESSAIM(1)   Manuel   ESSAIM(1)
    let pad = width.saturating_sub(18 + mid.chars().count());
    let head = if width >= 18 + mid.chars().count() {
        format!("ESSAIM(1){}{}{}ESSAIM(1)", " ".repeat(pad / 2), mid, " ".repeat(pad - pad / 2))
    } else {
        "ESSAIM(1)".to_string() // terminal étroit : seul le titre de page
    };
    out.push(Line::styled(head.chars().take(width.max(1)).collect::<String>(), dim));
    out.push(Line::raw(""));

    for e in entries.iter().copied() {
        match e {
            Section(t) => out.push(Line::styled((*t).to_string(), bold)),
            Blank => out.push(Line::raw("")),
            Code(t) => {
                // Pas de reformatage des mots, mais repli dur avec retrait de continuation : rien n'est coupé.
                let avail = width.saturating_sub(BODY + 1).max(8);
                let chars: Vec<char> = t.chars().collect();
                for (i, chunk) in chars.chunks(avail.saturating_sub(if chars.len() > avail { 2 } else { 0 }).max(4)).enumerate() {
                    let indent = if i == 0 { BODY } else { BODY + 2 };
                    out.push(Line::raw(format!("{}{}", " ".repeat(indent), chunk.iter().collect::<String>())));
                }
            }
            Para(t) => {
                for l in wrap(t, width.saturating_sub(BODY + 1)) {
                    out.push(Line::raw(format!("{}{l}", " ".repeat(BODY))));
                }
            }
            Item(term, desc) => {
                // Terminal étroit ou terme long : le terme prend sa ligne, la description passe dessous.
                let stacked = width < 48 || term.chars().count() >= TERM;
                let indent = if stacked { BODY + 4 } else { BODY + TERM };
                let lines = wrap(desc, width.saturating_sub(indent + 1));
                let tw = term.chars().count();
                if stacked {
                    for t in wrap(term, width.saturating_sub(BODY + 1)) {
                        out.push(Line::from(vec![Span::raw(" ".repeat(BODY)), Span::styled(t, bold)]));
                    }
                }
                for (i, l) in lines.into_iter().enumerate() {
                    if i == 0 && !stacked {
                        out.push(Line::from(vec![
                            Span::raw(" ".repeat(BODY)),
                            Span::styled((*term).to_string(), bold),
                            Span::raw(format!("{}{l}", " ".repeat(TERM - tw))),
                        ]));
                    } else {
                        out.push(Line::raw(format!("{}{l}", " ".repeat(indent))));
                    }
                }
            }
        }
    }
    out.push(Line::raw(""));
    let foot = format!("essaim{}ESSAIM(1)", " ".repeat(width.saturating_sub(15)));
    out.push(Line::styled(foot.chars().take(width.max(1)).collect::<String>(), dim));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(width: usize) -> Vec<String> {
        render(width).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn structure_d_une_page_man() {
        let t = text(100);
        assert!(t[0].starts_with("ESSAIM(1)") && t[0].ends_with("ESSAIM(1)"));
        assert_eq!(t[0].chars().count(), 100);
        assert!(text(24)[0].chars().count() <= 24, "en-tête tronqué sur terminal étroit");
        let sections: Vec<&String> = t.iter().filter(|l| !l.is_empty() && !l.starts_with(' ') && l.chars().all(|c| !c.is_lowercase())).collect();
        for s in ["NOM", "SYNOPSIS", "DESCRIPTION", "COMMANDES", "FICHIERS", "ENVIRONNEMENT", "CODE DE SORTIE", "EXEMPLES", "VOIR AUSSI"] {
            assert!(sections.iter().any(|l| l.as_str() == s), "section {s} absente");
        }
        // Ordre des sections de man : NOM, SYNOPSIS, DESCRIPTION en tête.
        let pos = |s: &str| t.iter().position(|l| l == s).unwrap();
        assert!(pos("NOM") < pos("SYNOPSIS") && pos("SYNOPSIS") < pos("DESCRIPTION") && pos("DESCRIPTION") < pos("COMMANDES"));
        assert!(pos("EXEMPLES") < pos("VOIR AUSSI"));
    }

    #[test]
    fn corps_indente_et_retrait_suspendu() {
        let t = text(60);
        assert!(t.iter().any(|l| l.starts_with("       essaim — concevoir")), "corps indenté de 7 colonnes");
        let i = t.iter().position(|l| l.starts_with("       Tab, Maj+Tab")).unwrap();
        assert!(t[i].contains("onglet suivant"), "terme et description sur la même ligne");
        // Une description longue se poursuit avec le retrait de la colonne des descriptions.
        let long = t.iter().position(|l| l.starts_with("       company run")).unwrap();
        assert!(t[long].trim_start().starts_with("company run"));
        assert!(t.iter().all(|l| l.chars().count() <= 60), "aucune ligne ne dépasse la largeur");
    }

    #[test]
    fn ne_depasse_jamais_la_largeur_et_ne_plante_pas() {
        for w in [24, 30, 40, 47, 48, 60, 80, 120, 200] {
            for l in text(w) {
                assert!(l.chars().count() <= w, "largeur {w} : {l:?}");
            }
        }
    }

    #[test]
    fn les_touches_documentees_existent_dans_le_code() {
        let t = text(120).join("\n");
        for k in ["Ctrl+K", "F1 … F5", "Ctrl+\\", "company run", "--yes", "ESSAIM_CLAUDE_BIN"] {
            assert!(t.contains(k), "{k}");
        }
    }

    #[test]
    fn decoupe_des_mots() {
        assert_eq!(wrap("un deux trois quatre", 9), vec!["un deux", "trois", "quatre"]);
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("démesurémentlongmot", 8), vec!["démesuré", "mentlong", "mot"], "mot trop long coupé");
    }
}
