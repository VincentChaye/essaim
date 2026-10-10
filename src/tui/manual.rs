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
    Code("essaim company run <entreprise.toml> (--brief \"…\" | --brief @fichier | --mission <id> [--resume]) [--yes]"),
    Blank,
    Section("DESCRIPTION"),
    Para("Un agent est un fichier .claude/agents/<nom>.md. On les assemble en entreprise (organigramme boss, VP, managers, workers). Tout passe par le CLI claude et ton abonnement : les clés API sont ignorées et la consommation est affichée en tokens."),
    Para("Sans argument, essaim ouvre l'interface en onglets, dans le dossier courant (racine du projet)."),
    Para("Cinq onglets : 1 Agents, 2 Entreprise, 3 Run, 4 Sessions, 5 Claude (session Claude Code intégrée)."),
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
    Item("q, Ctrl+C", "quitte. Si un run est en cours, demande confirmation : il est alors annulé."),
    Blank,
    Section("ONGLET 1 : AGENTS"),
    Item("n", "crée un agent avec l'assistant (conversation en mode plan)."),
    Item("N", "crée un agent avec un formulaire."),
    Item("e, Entrée", "ouvre le choix du mode d'édition : simple (formulaire ou $EDITOR) ou avec le modèle (conversation). 1/2 choisissent, Échap annule."),
    Item("D", "supprime l'agent (confirmation ; la confirmation liste les postes qui l'utilisent)."),
    Para("Dans le formulaire : Tab champ suivant, ← → choisit le modèle, Ctrl+S enregistre, Échap annule."),
    Blank,
    Section("ONGLET 2 : ENTREPRISE"),
    Para("Organigramme en graphe d'arbre (une boîte par poste, un niveau par ligne) avec niveaux N, N-1, N-2… et intitulés de poste. Un poste marqué ⚒ réalise dans le projet, dans son périmètre ; les autres produisent du texte. Une mission se déroule en trois temps : conception en cascade (le boss délègue aux VP, qui délèguent aux managers, qui répartissent entre les workers, puis les rapports remontent), plan consolidé à valider, réalisation."),
    Item("n", "crée une entreprise avec l'assistant : organigramme conçu en conversation, agents manquants créés."),
    Item("← → ↓ ↑", "à l'ouverture de l'onglet, on est déjà dans la zone Entreprises (↑ ↓ choisit l'entreprise, Entrée passe aux Missions). Échap sort de la zone ; ← → change alors de zone (Entreprises, Missions), ↓ va à l'organigramme, ↑ revient aux Missions, Entrée entre dans la zone."),
    Item("← →", "zone Missions : → déplie le résumé de la mission choisie (but, fichiers, résultat), ← le replie ; Espace bascule. ↑ ↓ choisit la mission."),
    Item("↑ ↓ ← →", "dans l'organigramme (après Entrée) : ↑ le supérieur, ↓ le premier subordonné, ← → le voisin du même niveau (même d'une autre branche). Home/g : le boss."),
    Para("Formulaire de poste : ↑ ↓ / Tab change de champ (la fenêtre défile, avec une barre), Entrée passe au champ suivant, ← → choisit dans une liste, Ctrl+S ou le bouton [Valider les changements] enregistre, Échap ou [Annuler] abandonne. Valider et annuler demandent une vérification (o = oui, autre touche = retour au formulaire)."),
    Item("a", "dans l'organigramme : ajoute un poste sous le poste choisi (formulaire : id, intitulé, agent, supérieur, expertise, ⚒, commandes shell)."),
    Item("e", "édite le poste choisi : une fenêtre propose l'édition simple (formulaire ; renommer un id met à jour ses subordonnés) ou l'édition avec le modèle (conversation en mode plan qui modifie le fichier). Même choix pour un agent, une entreprise ou un brief ($EDITOR en édition simple)."),
    Item("x", "dans l'organigramme : retire le poste choisi (confirmation) ; ses subordonnés remontent d'un cran. Le boss ne part que seul."),
    Item("m", "cadre une mission : conversation avec le boss (mode plan) qui écrit le brief."),
    Item("r", "lance la mission choisie : conception, plan à valider, réalisation. Une mission dont la conception est terminée (annulée ou refusée ensuite) reprend à la réalisation, sans rejouer la conception."),
    Item("R", "relance la mission choisie depuis le début, sans reprise (confirmation : le plan et le rapport existants sont remplacés). r demande la même confirmation dès qu'il y a un plan ou un rapport à remplacer."),
    Item("t, T", "mission suivante, précédente."),
    Item("Entrée, o", "zone Missions : ouvre la mission choisie en lecture, même une ancienne : compte rendu de fin de mission, rapport du boss, plan, brief (rendu avec glow s'il est installé). Dans la lecture : ↑ ↓ défile, Espace / b page suivante / précédente, g début, Échap ou q ferme."),
    Item("d", "dans la lecture d'une mission : discute du compte rendu avec le modèle (conversation en mode plan). Il propose les actions à mener, une entreprise existante pour les réaliser ou la création d'une nouvelle, puis écrit actions.md et les briefs de suite une fois validés. /exit pour revenir."),
    Item("c, C", "entreprise suivante, précédente."),
    Item("e", "dans la zone Entreprises ou Missions : édition simple ($EDITOR, les formulaires de poste réécrivent le fichier : les commentaires TOML sont perdus) ou avec le modèle."),
    Blank,
    Section("ONGLET 3 : RUN"),
    Item("↑ ↓", "choisit un nœud (son journal s'affiche)."),
    Item("s", "annule le run en cours (confirmation)."),
    Item("h", "historique ; Entrée rouvre un run, D le supprime."),
    Item("Entrée, Échap", "au plan consolidé : lance la réalisation, ou arrête ; ↑ ↓ / PgUp / PgDn font défiler le plan. En fin de mission, compte-rendu.md est écrit et la consommation s'affiche en tokens."),
    Item("e", "au plan consolidé : ouvre une conversation avec le boss pour modifier le plan (il réécrit conception.md) ; en quittant (/exit), le plan relu s'affiche et Entrée lance la réalisation avec lui."),
    Blank,
    Section("ONGLET 4 : SESSIONS"),
    Item("a, Entrée", "attache la session background (dans l'onglet 5)."),
    Item("s", "arrête la session (confirmation)."),
    Item("x", "supprime la session et son historique (confirmation)."),
    Item("r", "rafraîchit la liste."),
    Blank,
    Section("ONGLET 5 : CLAUDE"),
    Para("Un vrai Claude Code dans un terminal intégré : toutes les touches lui sont transmises, y compris q et Ctrl+C."),
    Item("Ctrl+\\", "rend les touches de navigation à essaim ; la session continue."),
    Item("Entrée, i", "reprend la session."),
    Item("x", "termine la session (confirmation)."),
    Item("/exit", "termine la session (commande de Claude)."),
    Blank,
    Section("COMMANDES"),
    Item("init", "crée des agents et une entreprise d'exemple, sans écraser."),
    Item("agents", "liste les agents."),
    Item("company show", "affiche l'organigramme et l'estimation d'appels."),
    Item("company check", "valide l'organigramme."),
    Item("company run", "exécute une mission. --brief @fichier lit le brief dans un fichier ; --mission <id> --resume reprend une mission interrompue (une conception terminée n'est pas rejouée) ; --yes valide sans demander."),
    Para("Sans --yes, un plan consolidé demande [o/N/e] : o lance la réalisation, e modifie le plan avec le boss, toute autre réponse arrête. Si checkpoint = false dans le TOML, il n'y a ni invite ni pause : la réalisation démarre après la conception. Avertissement d'exécution dans ce cas : [À CONFIRMER : texte et canal à fixer]."),
    Blank,
    Section("FICHIERS"),
    Item(".claude/agents/", "un fichier .md par agent (format standard de Claude Code)."),
    Item(".claude/companies/", "entreprises TOML (postes, hiérarchie, périmètres)."),
    Item(".essaim/runs/", "historique des runs."),
    Item(".essaim/missions/", "brief, conception.md, rapport.md, compte-rendu.md (et actions.md après une discussion) de chaque mission."),
    Item("settings.toml", "langue, dans le dossier de configuration de la plateforme ; ESSAIM_CONFIG_DIR le remplace."),
    Blank,
    Section("ENVIRONNEMENT"),
    Item("VISUAL, EDITOR", "éditeur ouvert par la touche e (vi par défaut)."),
    Item("glow", "si la commande glow est dans le PATH (brew install glow), elle met en forme les comptes rendus et rapports ; sinon un rendu intégré est utilisé."),
    Item("ESSAIM_CLAUDE_BIN", "remplace la commande claude (tests)."),
    Item("ESSAIM_CONFIG_DIR", "dossier de settings.toml (langue), à la place de celui de la plateforme."),
    Item("ANTHROPIC_API_KEY", "ignorée : retirée de chaque processus lancé."),
    Item("ANTHROPIC_AUTH_TOKEN", "ignorée : retirée de chaque processus lancé."),
    Item("CLAUDE_CODE_USE_*", "toute variable commençant par CLAUDE_CODE_USE_ (BEDROCK, VERTEX, FOUNDRY…) : retirée de chaque processus lancé."),
    Blank,
    Section("CODE DE SORTIE"),
    Item("0", "succès. Une réalisation refusée au point de contrôle sort aussi en 0."),
    Item("1", "un nœud ou un poste a échoué, ou la validation a échoué, ou --yes manque (mission de plus de 30 appels)."),
    Blank,
    Section("EXEMPLES"),
    Code("essaim init && essaim"),
    Code("essaim company check .claude/companies/acme-studio.toml"),
    Code("essaim company run .claude/companies/acme-studio.toml --brief \"Ajouter une page d'accueil\""),
    Code("printf '%s\\n' \"Ajouter une page d'accueil\" > brief.md && essaim company run .claude/companies/acme-studio.toml --brief @brief.md"),
    Code("essaim company run .claude/companies/acme-studio.toml --mission <id> --resume"),
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
    Item("q, Ctrl+C", "quit; asks for confirmation if a run is in progress (it is then cancelled)."),
];

const TABS_EN: [&[Entry]; 5] = [
    &[
        Section("TAB 1: AGENTS"),
        Item("n", "create an agent with the assistant (plan-mode conversation)."),
        Item("N", "create an agent with a form."),
        Item("e, Enter", "choose the edit mode: simple (form or $EDITOR) or with the model (conversation). 1/2 pick, Esc cancels."),
        Item("D", "delete the agent (confirmation)."),
        Para("In the form: Tab next field, ← → pick the model, Ctrl+S save, Esc cancel."),
    ],
    &[
        Section("TAB 2: COMPANY"),
        Para("Org chart as a tree graph (one box per seat, one level per row) with levels N, N-1, N-2… and job titles. A seat marked ⚒ builds in the project, within its scope; the others produce text. A mission has three steps: cascading design (boss delegates to VPs, who delegate to managers, who split the work among workers, then reports flow back up), a consolidated plan to approve, then realisation."),
        Item("n", "create a company with the assistant: org chart designed in conversation, missing agents created."),
        Item("← → ↓ ↑", "the tab opens already in the Companies zone (↑ ↓ picks the company, Enter moves to Missions). Esc leaves the zone; ← → then switches zone (Companies, Missions), ↓ goes to the org chart, ↑ back to Missions, Enter enters the zone."),
        Item("← →", "Missions zone: → unfolds the summary of the selected mission (goal, files, outcome), ← folds it; Space toggles. ↑ ↓ picks the mission."),
        Item("↑ ↓ ← →", "in the org chart (after Enter): ↑ the supervisor, ↓ the first subordinate, ← → the neighbour on the same level (even in another branch). Home/g: the boss."),
        Para("Seat form: ↑ ↓ / Tab changes field (the window scrolls, with a bar), Enter moves to the next field, ← → picks from a list, Ctrl+S or the [Valider les changements] button saves, Esc or [Annuler] discards. Saving and cancelling ask for a check (y = yes, any other key = back to the form)."),
        Item("a", "in the org chart: add a seat under the selected one (form: id, title, agent, supervisor, expertise, ⚒, shell commands)."),
        Item("e", "edit the selected seat: a window offers the simple edit (form; renaming an id updates its subordinates) or the edit with the model (plan-mode conversation that modifies the file). Same choice for an agent, a company or a brief ($EDITOR for the simple edit)."),
        Item("x", "in the org chart: remove the selected seat (confirmation); its subordinates move up. The boss can only leave alone."),
        Item("m", "frame a mission: conversation with the boss (plan mode) who writes the brief."),
        Item("r", "run the selected mission: design, plan to approve, realisation. A mission whose design is finished (cancelled or refused afterwards) resumes at realisation without replaying the design."),
        Item("R", "run the selected mission again from the start, no resume."),
        Item("t, T", "next, previous mission."),
        Item("Enter, o", "in the Missions zone: open the selected mission read-only, even an old one: end-of-mission report, boss report, plan, brief (rendered with glow when installed). While reading: ↑ ↓ scroll, Space / b next / previous page, g top, Esc or q closes."),
        Item("d", "while reading a mission: discuss the report with the model (plan-mode conversation). It proposes the actions to take, an existing company to carry them out or the creation of a new one, then writes actions.md and follow-up briefs once approved. /exit to come back."),
        Item("c, C", "next, previous company."),
        Item("e", "in the Companies or Missions zone: simple edit ($EDITOR; seat forms rewrite the file: TOML comments are lost) or edit with the model."),
    ],
    &[
        Section("TAB 3: RUN"),
        Item("↑ ↓", "pick a node (its log is shown)."),
        Item("s", "cancel the current run."),
        Item("h", "history; Enter reopens a run, D deletes it."),
        Item("Enter, Esc", "at the consolidated plan: start realisation, or stop; ↑ ↓ / PgUp / PgDn scroll the plan. At the end of a mission, compte-rendu.md is written and usage is shown in tokens."),
        Item("e", "at the consolidated plan: open a conversation with the boss to change the plan (it rewrites conception.md); on exit (/exit) the reloaded plan is shown and Enter starts realisation with it."),
    ],
    &[
        Section("TAB 4: SESSIONS"),
        Item("a, Enter", "attach the background session (in tab 5)."),
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
    Code("essaim company run <company.toml> (--brief \"…\" | --brief @file | --mission <id> [--resume]) [--yes]"),
    Blank,
    Section("DESCRIPTION"),
    Para("An agent is a file .claude/agents/<name>.md. They are assembled into a company (org chart: boss, VPs, managers, workers). Everything goes through the claude CLI and your subscription: API keys are ignored and usage is shown in tokens."),
    Para("With no argument, essaim opens the tabbed interface in the current folder (project root). Models are briefed in English and told the language you chose (F10)."),
    Para("Five tabs: 1 Agents, 2 Company, 3 Run, 4 Sessions, 5 Claude (an embedded Claude Code session)."),
    Blank,
];

const TAIL_EN: &[Entry] = &[
    Section("COMMANDS"),
    Item("init", "create example agents and company, without overwriting."),
    Item("agents", "list the agents."),
    Item("company show", "show the org chart and the call estimate."),
    Item("company check", "validate the org chart."),
    Item("company run", "run a mission. --brief @file reads the brief from a file; --mission <id> --resume resumes an interrupted mission (a finished design is not replayed); --yes approves without asking."),
    Para("Without --yes, a consolidated plan asks [o/N/e]: o starts realisation, e changes the plan with the boss, any other answer stops. If checkpoint = false in the TOML, there is no prompt or pause: realisation starts after the design. Execution warning in that case: [TO CONFIRM: text and channel to be set]."),
    Blank,
    Section("FILES"),
    Item(".claude/agents/", "one .md file per agent (standard Claude Code format)."),
    Item(".claude/companies/", "TOML companies (seats, hierarchy, scopes)."),
    Item(".essaim/runs/", "run history."),
    Item(".essaim/missions/", "brief, conception.md, rapport.md, compte-rendu.md (and actions.md after a debrief) of each mission."),
    Item("settings.toml", "language, in the platform's configuration folder; ESSAIM_CONFIG_DIR replaces it."),
    Blank,
    Section("ENVIRONMENT"),
    Item("VISUAL, EDITOR", "editor opened by the e key (vi by default)."),
    Item("glow", "if the glow command is in the PATH (brew install glow), it formats reports; otherwise a built-in renderer is used."),
    Item("ESSAIM_CLAUDE_BIN", "replaces the claude command (tests)."),
    Item("ESSAIM_CONFIG_DIR", "folder of settings.toml (language), instead of the platform default."),
    Item("ANTHROPIC_API_KEY", "ignored: removed from every launched process."),
    Item("ANTHROPIC_AUTH_TOKEN", "ignored: removed from every launched process."),
    Item("CLAUDE_CODE_USE_*", "any variable starting with CLAUDE_CODE_USE_ (BEDROCK, VERTEX, FOUNDRY...): removed from every launched process."),
    Blank,
    Section("EXIT STATUS"),
    Item("0", "success. A realisation refused at the checkpoint also exits with 0."),
    Item("1", "a node or seat failed, or validation failed, or --yes is missing (mission over 30 calls)."),
    Blank,
    Section("EXAMPLES"),
    Code("essaim init && essaim"),
    Code("essaim company check .claude/companies/acme-studio.toml"),
    Code("essaim company run .claude/companies/acme-studio.toml --brief \"Add a landing page\""),
    Code("printf '%s\\n' \"Add a landing page\" > brief.md && essaim company run .claude/companies/acme-studio.toml --brief @brief.md"),
    Code("essaim company run .claude/companies/acme-studio.toml --mission <id> --resume"),
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
