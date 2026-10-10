use crate::model::agent::{self, AgentDef};
use crate::model::company::{Company, Seat, graph_nav};
use crate::model::roles;
use crate::runtime::claude_proc::ClaudeEvent;
use crate::runtime::company_exec::{CompanyOptions, run_mission};
use crate::runtime::executor::{EscalationReply, NodeStatus, RunEvent, claude_runner};
use crate::runtime::missions::{self, Mission};
use crate::runtime::sessions::{self, Session};
use crate::runtime::store::{self, NodeRecord, RunRecord};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::path::PathBuf;
use tokio::sync::{mpsc, watch};

use super::editor::TextBuf;
use super::settings::{self, Lang, Settings, tr};
use crate::tf;
use super::term::{TermSession, encode_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Agents,
    Company,
    Run,
    Sessions,
    /// Terminal intégré : session Claude Code (assistant ou attache).
    Terminal,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Agents, Tab::Company, Tab::Run, Tab::Sessions, Tab::Terminal];

    pub fn title(self, lang: Lang) -> &'static str {
        match self {
            Tab::Agents => "Agents",
            Tab::Company => lang.t("Entreprise", "Company"),
            Tab::Run => "Run",
            Tab::Sessions => "Sessions",
            Tab::Terminal => "Claude",
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// Nature de l'assistant conversationnel lancé depuis la TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantKind {
    Agent,
    /// Brief d'une mission, cadré avec le boss.
    Mission,
    /// Organigramme d'une entreprise (et ses agents manquants).
    Company,
    /// Révision du plan consolidé d'un run en attente au point de contrôle.
    Plan,
    /// Modification d'un élément existant (agent, poste, entreprise, brief) avec le modèle.
    Edit,
    /// Discussion sur le compte rendu d'une mission pour en tirer des actions.
    Debrief,
    /// Discussion avec le CEO d'une mission bloquée, pour décider comment la débloquer.
    Escalation,
}

/// Élément dont on choisit le mode d'édition (touche `e`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditWhat {
    Agent(String),
    /// Poste : id.
    Seat(String),
    Company,
    Mission,
}

/// Choix entre édition simple (formulaire ou $EDITOR) et édition avec le modèle (conversation).
#[derive(Debug, Clone)]
pub struct EditChoice {
    pub what: EditWhat,
    /// Fichier concerné (agent, entreprise ou brief).
    pub file: PathBuf,
    /// Intitulé affiché dans la fenêtre.
    pub label: String,
    /// 0 = édition simple, 1 = avec le modèle.
    pub sel: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompZone {
    Companies,
    Missions,
    Graph,
}

/// Navigation entre onglets par raccourci global (valable aussi dans le terminal intégré).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabNav {
    Goto(Tab),
    Next,
    Prev,
}

/// Raccourcis qui n'entrent pas en conflit avec Claude Code et fonctionnent sur QWERTY comme sur AZERTY :
/// `F1`–`F5` · `Alt`+`1`–`5` (ou `& é " ' (` sur AZERTY) · `Ctrl+PageUp` / `Ctrl+PageDown`.
pub fn tab_shortcut(k: &KeyEvent) -> Option<TabNav> {
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::F(n @ 1..=5) if k.modifiers.is_empty() => Some(TabNav::Goto(Tab::ALL[n as usize - 1])),
        KeyCode::Char(c) if alt && !ctrl => {
            let i = match c {
                '1' | '&' => 0,
                '2' | 'é' => 1,
                '3' | '"' => 2,
                '4' | '\'' => 3,
                '5' | '(' => 4,
                _ => return None,
            };
            Some(TabNav::Goto(Tab::ALL[i]))
        }
        KeyCode::PageUp if ctrl => Some(TabNav::Prev),
        KeyCode::PageDown if ctrl => Some(TabNav::Next),
        _ => None,
    }
}

pub enum AppEvent {
    Key(KeyEvent),
    Run(RunEvent),
    Tick,
    /// Résultat d'un rafraîchissement de la liste des sessions (None = échec, voir `error`).
    Sessions { list: Option<Vec<Session>>, error: Option<String> },
    /// Message de fin d'une action asynchrone (stop, suppression…).
    Notice(String),
    /// Le terminal intégré a produit de la sortie (simple demande de redessin).
    TermOutput,
    /// Le processus du terminal intégré (identifié par son id) s'est terminé.
    TermExit(u64),
    /// Texte collé dans le terminal de l'outil.
    Paste(String),
}

/// Modèles proposés (alias acceptés par `claude --model`). La chaîne vide = modèle par défaut.
pub const MODEL_CHOICES: [&str; 5] = ["", "haiku", "sonnet", "opus", "fable"];

pub fn form_labels() -> [&'static str; 5] {
    [
        tr("Nom", "Name"),
        "Description",
        tr("Modèle  (← → pour choisir)", "Model  (← → to pick)"),
        tr("Outils, séparés par des virgules (vide = tous)", "Tools, comma-separated (empty = all)"),
        tr("Prompt système", "System prompt"),
    ]
}

/// Champ de formulaire : saisie libre ou choix dans une liste.
pub enum Field {
    Text(TextBuf),
    Choice { options: Vec<String>, idx: usize },
}

impl Field {
    /// Liste de choix ; une valeur absente de la liste (modèle personnalisé) est conservée.
    fn choice(options: &[&str], current: &str) -> Self {
        let mut options: Vec<String> = options.iter().map(|o| o.to_string()).collect();
        let idx = match options.iter().position(|o| o == current) {
            Some(i) => i,
            None => {
                options.push(current.to_string());
                options.len() - 1
            }
        };
        Field::Choice { options, idx }
    }

    /// Liste de choix construite dynamiquement (agents, postes…).
    fn choice_of(options: Vec<String>, current: &str) -> Self {
        let refs: Vec<&str> = options.iter().map(|s| s.as_str()).collect();
        Field::choice(&refs, current)
    }

    pub fn text(&self) -> String {
        match self {
            Field::Text(b) => b.text(),
            Field::Choice { options, idx } => options[*idx].clone(),
        }
    }

    fn handle_key(&mut self, k: KeyEvent) -> bool {
        match self {
            Field::Text(b) => b.handle_key(k),
            Field::Choice { options, idx } => {
                let n = options.len();
                match k.code {
                    KeyCode::Left | KeyCode::Up => *idx = (*idx + n - 1) % n,
                    KeyCode::Right | KeyCode::Down | KeyCode::Char(' ') => *idx = (*idx + 1) % n,
                    _ => return false,
                }
                true
            }
        }
    }
}

pub struct AgentForm {
    pub fields: Vec<Field>,
    pub focus: usize,
    /// Nom de l'agent édité (None = création) pour gérer le renommage.
    pub original: Option<String>,
}

impl AgentForm {
    fn new(a: Option<&AgentDef>) -> Self {
        let d = AgentDef::default();
        let a = a.unwrap_or(&d);
        Self {
            fields: vec![
                Field::Text(TextBuf::new(&a.name, false)),
                Field::Text(TextBuf::new(&a.description, false)),
                Field::choice(&MODEL_CHOICES, a.model.as_deref().unwrap_or("")),
                Field::Text(TextBuf::new(&a.tools.as_ref().map(|t| t.join(", ")).unwrap_or_default(), false)),
                Field::Text(TextBuf::new(&a.prompt, true)),
            ],
            focus: 0,
            original: (!a.name.is_empty()).then(|| a.name.clone()),
        }
    }

    fn to_def(&self) -> AgentDef {
        let t = |i: usize| self.fields[i].text().trim().to_string();
        let tools: Vec<String> = t(3).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        AgentDef {
            name: t(0),
            description: t(1),
            model: Some(t(2)).filter(|m| !m.is_empty()),
            tools: (!tools.is_empty()).then_some(tools),
            prompt: self.fields[4].text().trim_matches('\n').to_string(),
        }
    }
}

/// Formulaire de poste (entreprise) : id, intitulé, agent, supérieur, expertise, réalisation, commandes shell.
pub struct SeatForm {
    pub fields: Vec<Field>,
    pub labels: Vec<String>,
    pub focus: usize,
    /// Id du poste édité (None = création), pour gérer le renommage.
    pub original: Option<String>,
    pub path: PathBuf,
    /// Poste tel qu'ouvert, pour savoir si des changements seraient perdus.
    initial: Seat,
    /// Demande de vérification en cours : `Some(true)` = valider, `Some(false)` = annuler.
    pub confirm: Option<bool>,
}

/// Boutons sous les champs : `focus == fields.len()` = Valider, `+ 1` = Annuler.
const SEAT_BUTTONS: usize = 2;

fn descendants(c: &Company, i: usize) -> Vec<usize> {
    let mut out = vec![];
    let mut stack = c.children(i);
    while let Some(j) = stack.pop() {
        out.push(j);
        stack.extend(c.children(j));
    }
    out
}

impl SeatForm {
    /// `seat` = poste édité ; sinon création sous `parent`.
    fn new(c: &Company, path: PathBuf, agents: &[String], seat: Option<usize>, parent: Option<usize>) -> Self {
        let s = seat.map(|i| c.seats[i].clone()).unwrap_or_else(|| Seat {
            id: String::new(),
            title: String::new(),
            agent: agents.first().cloned().unwrap_or_default(),
            model: None,
            reports_to: parent.map(|p| c.seats[p].id.clone()),
            expertise: None,
            builds: false,
            allow_bash: vec![],
        });
        let (depth, is_leaf) = match (seat, parent) {
            (Some(i), _) => (c.depth(i), c.is_leaf(i)),
            (None, Some(p)) => (c.depth(p) + 1, true),
            (None, None) => (0, true),
        };
        let hint = roles::title_suggestions(depth, is_leaf).iter().take(4).copied().collect::<Vec<_>>().join(", ");
        let sup: Vec<String> = match seat {
            Some(i) if c.parent(i).is_none() => vec![String::new()],
            _ if c.seats.is_empty() => vec![String::new()],
            _ => {
                let banned: Vec<usize> = seat.map(|i| [vec![i], descendants(c, i)].concat()).unwrap_or_default();
                (0..c.seats.len()).filter(|j| !banned.contains(j)).map(|j| c.seats[j].id.clone()).collect()
            }
        };
        let level = roles::level_label(depth);
        let mut form = Self {
            fields: vec![
                Field::Text(TextBuf::new(&s.id, false)),
                Field::Text(TextBuf::new(&s.title, false)),
                Field::choice_of(agents.to_vec(), &s.agent),
                Field::choice(&MODEL_CHOICES, s.model.as_deref().unwrap_or("")),
                Field::choice_of(sup, s.reports_to.as_deref().unwrap_or("")),
                Field::Text(TextBuf::new(s.expertise.as_deref().unwrap_or(""), false)),
                Field::choice_of(vec!["non".into(), "oui".into()], if s.builds { "oui" } else { "non" }),
                Field::Text(TextBuf::new(&s.allow_bash.join(", "), false)),
            ],
            labels: vec![
                tr("Id (lettres, chiffres, - et _)", "Id (letters, digits, - and _)").into(),
                tf!("Intitulé du poste, niveau {level}  (ex. {hint})", "Job title, level {level}  (e.g. {hint})"),
                tr("Agent  (← → pour choisir)", "Agent  (← → to pick)").into(),
                tr("Modèle du poste  (← → ; vide = celui de l'agent)", "Seat model  (← → ; empty = the agent's)").into(),
                tr("Supérieur  (← → pour choisir)", "Supervisor  (← → to pick)").into(),
                tr("Expertise (ce que le supérieur sait pouvoir lui déléguer)", "Expertise (what the supervisor knows can be delegated to it)").into(),
                tr("Poste de réalisation ⚒  (← → ; modifie le projet, feuille uniquement)", "Realisation seat ⚒  (← → ; modifies the project, leaves only)").into(),
                tr("Commandes shell autorisées, séparées par des virgules (⚒ seulement)", "Allowed shell commands, comma-separated (⚒ only)").into(),
            ],
            focus: 0,
            original: seat.map(|_| s.id.clone()),
            path,
            initial: s.clone(),
            confirm: None,
        };
        form.initial = form.to_seat();
        form
    }

    fn changed(&self) -> bool {
        self.to_seat() != self.initial
    }

    fn to_seat(&self) -> Seat {
        let t = |i: usize| self.fields[i].text().trim().to_string();
        Seat {
            id: t(0),
            title: t(1),
            agent: t(2),
            model: Some(t(3)).filter(|m| !m.is_empty()),
            reports_to: Some(t(4)).filter(|r| !r.is_empty()),
            expertise: Some(t(5)).filter(|e| !e.is_empty()),
            builds: t(6) == "oui",
            allow_bash: t(7).split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        }
    }
}

pub enum Mode {
    Normal,
    EditAgent(AgentForm),
    EditSeat(SeatForm),
    /// Choix du mode d'édition : simple ou avec le modèle.
    ChooseEdit(EditChoice),
    /// Action destructive ou irréversible à confirmer (suppression, arrêt, annulation, écrasement).
    ConfirmAction(PendingAction),
}

/// Action potentiellement destructive, exécutée seulement après un `o` (ou `y`) ; toute autre touche l'annule.
pub enum PendingAction {
    /// Suppression d'un agent : nom.
    DeleteAgent(String),
    /// Suppression d'un poste : (fichier de l'entreprise, id, intitulé).
    DeleteSeat(PathBuf, String, String),
    /// Suppression d'une entrée d'historique : (id, nom du run).
    DeleteHistory(String, String),
    /// Annulation du run en cours : nom.
    CancelRun(String),
    /// Sortie de l'outil alors qu'un run est en cours (il sera annulé) : nom du run.
    QuitRun(String),
    /// Mission relancée alors qu'elle écraserait conception / rapport : (titre, reprise).
    StartMission(String, bool),
    /// Fin de la session Claude intégrée.
    FinishTerm,
    /// Suppression d'une session background : (id, nom).
    RemoveSession(String, String),
    /// Arrêt d'une session background : (id, nom).
    StopSession(String, String),
}

/// Gabarit commun des confirmations : « Supprimer {objet} « {nom} » ? (o/n) ».
fn ask_delete(objet_fr: &str, objet_en: &str, nom: &str) -> String {
    tf!("Supprimer {objet_fr} « {nom} » ? (o/n)", "Delete {objet_en} \"{nom}\"? (o/n)")
}

impl PendingAction {
    pub fn question(&self) -> String {
        match self {
            Self::DeleteAgent(name) => ask_delete("l'agent", "the agent", name),
            Self::DeleteSeat(_, _, title) => ask_delete("le poste", "the seat", title),
            Self::DeleteHistory(_, name) => ask_delete("le run", "the run", name),
            Self::RemoveSession(_, name) => ask_delete("la session", "the session", name),
            Self::CancelRun(name) => tf!("Annuler le run « {name} » ? (o/n)", "Cancel the run \"{name}\"? (o/n)"),
            Self::QuitRun(name) => tf!("Quitter et annuler le run « {name} » ? (o/n)", "Quit and cancel the run \"{name}\"? (o/n)"),
            Self::StartMission(title, _) => tf!("Relancer « {title} » depuis le début ? Le plan et le rapport précédents seront remplacés. (o/n)", "Restart \"{title}\" from scratch? The previous plan and report will be replaced. (o/n)"),
            Self::FinishTerm => tr("Terminer la session Claude en cours ? (o/n)", "End the current Claude session? (o/n)").into(),
            Self::StopSession(_, name) => tf!("Arrêter la session « {name} » ? (o/n)", "Stop the session \"{name}\"? (o/n)"),
        }
    }

    /// Précision affichée sous la question (conséquences, postes concernés).
    pub fn detail(&self, users: &[String]) -> Option<String> {
        match self {
            Self::DeleteAgent(_) if !users.is_empty() => Some(tf!("⚠ Postes utilisant cet agent : {}", "⚠ Seats using this agent: {}", users.join(", "))),
            Self::DeleteSeat(..) => Some(tr("Ses subordonnés remontent d'un cran.", "Its subordinates move up one level.").into()),
            Self::DeleteHistory(..) => Some(tr("L'historique du run est effacé ; les missions restent.", "The run history is erased; missions are kept.").into()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeState {
    Waiting,
    Running,
    Done(NodeStatus),
}

pub struct NodeView {
    pub id: String,
    pub agent: String,
    /// Libellé affiché (« CTO · délègue ») et indentation hiérarchique ; l'id par défaut.
    pub label: String,
    pub depth: usize,
    pub state: NodeState,
    pub log: Vec<String>,
    pub tokens: u64,
}

impl NodeView {
    pub fn new(id: &str, agent: &str) -> Self {
        Self { id: id.into(), agent: agent.into(), label: id.into(), depth: 0, state: NodeState::Waiting, log: vec![], tokens: 0 }
    }
}

/// Menu des réglages (langue). `first_run` : choix imposé au tout premier lancement.
pub struct SettingsMenu {
    pub first_run: bool,
    pub sel: usize,
}

/// Mission passée ouverte en lecture : brief, plan et rapport.
pub struct MissionView {
    pub title: String,
    pub dir: PathBuf,
    pub text: Vec<ratatui::text::Line<'static>>,
    pub scroll: u16,
}

/// Plan consolidé en attente de validation avant la réalisation.
pub struct CheckpointState {
    pub plan: String,
    pub reply: mpsc::UnboundedSender<bool>,
    pub scroll: u16,
}

/// Mission bloquée que le CEO n'a pas pu débloquer : le run attend la décision de l'utilisateur.
pub struct EscalationState {
    pub summary: String,
    pub reply: mpsc::UnboundedSender<EscalationReply>,
    pub scroll: u16,
}

pub struct RunView {
    pub name: String,
    pub nodes: Vec<NodeView>,
    pub sel: usize,
    pub tokens: u64,
    /// None = en cours ; Some(ok) = terminé.
    pub finished: Option<bool>,
    pub cancel: watch::Sender<bool>,
}

pub struct CompanyEntry {
    pub path: PathBuf,
    pub file: String,
    pub parsed: Result<Company, String>,
    /// Problème de validation (agent inconnu, deux boss, cycle…), si le TOML est lisible.
    pub problem: Option<String>,
}

pub struct App {
    pub root: PathBuf,
    pub tab: Tab,
    pub mode: Mode,
    pub agents: Vec<AgentDef>,
    pub agent_sel: usize,
    pub companies: Vec<CompanyEntry>,
    pub comp_sel: usize,
    pub missions: Vec<Mission>,
    pub mission_sel: usize,
    /// Dossier créé pour la mission en cours de cadrage (supprimé s'il reste vide).
    pub pending_mission_dir: Option<PathBuf>,
    /// Ligne sélectionnée dans l'arbre de l'organigramme.
    pub seat_sel: usize,
    /// Zone de l'onglet Entreprise sous le curseur, et si l'on est « entré » dedans (Entrée / Échap).
    pub zone: CompZone,
    pub zone_in: bool,
    pub run: Option<RunView>,
    pub checkpoint: Option<CheckpointState>,
    pub escalation: Option<EscalationState>,
    /// Entreprise et dossier de la mission du run en cours : permettent de réviser le plan au point de contrôle.
    pub run_ctx: Option<(Company, PathBuf)>,
    pub mission_view: Option<MissionView>,
    /// Missions dépliées dans la liste (id → résumé).
    pub mission_open: std::collections::HashMap<String, missions::Summary>,
    /// Fenêtre d'aide (Ctrl+K) et son défilement.
    pub help_open: bool,
    /// Aide : manuel complet (`m`) au lieu de l'aide de l'onglet courant.
    pub help_full: bool,
    pub lang: Lang,
    pub settings_path: Option<PathBuf>,
    pub settings_menu: Option<SettingsMenu>,
    pub help_scroll: u16,
    /// Défilement maximal et hauteur de page, mis à jour par le dessin (`u16::MAX` tant qu'inconnu).
    pub help_max: std::cell::Cell<u16>,
    pub help_page: std::cell::Cell<u16>,
    pub status: String,
    pub quit: bool,
    pub tick: u64,
    /// Fichier à ouvrir dans $EDITOR (traité par la boucle principale, qui suspend la TUI).
    pub editor_request: Option<PathBuf>,
    /// Id d'une session à rattacher via `claude attach` (traité comme `editor_request`).
    pub attach_request: Option<String>,
    /// Arguments d'une session d'assistant interactive (agent, mission ou entreprise) à lancer.
    pub assistant_request: Option<(AssistantKind, Vec<String>)>,
    /// Session Claude Code intégrée (assistant ou attache) et focus clavier.
    pub term: Option<TermSession>,
    pub term_focus: bool,
    term_seq: u64,
    /// Taille (lignes, colonnes) de la zone centrale, pour dimensionner le terminal intégré.
    pub body_size: (u16, u16),
    /// Historique des runs (plus récent d'abord) et vue associée dans l'onglet Run.
    pub history: Vec<RunRecord>,
    pub show_history: bool,
    pub hist_sel: usize,
    /// Run affiché ou en cours : identifiant et date de début, pour la sauvegarde.
    pub run_id: Option<String>,
    pub run_started_ms: u64,
    pub sessions: Vec<Session>,
    pub sess_sel: usize,
    /// Transcription lisible de la session sélectionnée.
    pub sess_detail: Vec<String>,
    pub sess_error: Option<String>,
    refreshing: bool,
    tx: mpsc::UnboundedSender<AppEvent>,
}

const MAX_LOG: usize = 2000;

impl App {
    pub fn new(root: PathBuf, tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        let mut app = Self {
            root,
            tab: Tab::Agents,
            mode: Mode::Normal,
            agents: vec![],
            agent_sel: 0,
            companies: vec![],
            comp_sel: 0,
            missions: vec![],
            mission_sel: 0,
            pending_mission_dir: None,
            seat_sel: 0,
            zone: CompZone::Companies,
            zone_in: true,
            run: None,
            checkpoint: None,
            escalation: None,
            run_ctx: None,
            mission_view: None,
            mission_open: Default::default(),
            help_open: false,
            help_full: false,
            lang: Lang::Fr,
            settings_path: None,
            settings_menu: None,
            help_scroll: 0,
            help_max: std::cell::Cell::new(u16::MAX),
            help_page: std::cell::Cell::new(20),
            status: String::new(),
            quit: false,
            tick: 0,
            editor_request: None,
            attach_request: None,
            assistant_request: None,
            term: None,
            term_focus: false,
            term_seq: 0,
            body_size: (24, 80),
            history: vec![],
            show_history: false,
            hist_sel: 0,
            run_id: None,
            run_started_ms: 0,
            sessions: vec![],
            sess_sel: 0,
            sess_detail: vec![],
            sess_error: None,
            refreshing: false,
            tx,
        };
        app.reload();
        app
    }

    pub fn companies_dir(&self) -> PathBuf {
        self.root.join(".claude/companies")
    }

    fn load_companies(root: &std::path::Path, agent_names: &[String]) -> Vec<CompanyEntry> {
        let Ok(rd) = std::fs::read_dir(root.join(".claude/companies")) else { return vec![] };
        let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml")).collect();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let parsed = Company::load(&path).map_err(|e| format!("{e:#}"));
                let problem = parsed.as_ref().ok().and_then(|c| c.validate(agent_names).err()).map(|e| format!("{e:#}"));
                CompanyEntry { path, file, parsed, problem }
            })
            .collect()
    }

    /// Missions de l'entreprise sélectionnée (index dans `missions`) : les siennes et celles que personne n'a encore lancées.
    pub fn visible_missions(&self) -> Vec<usize> {
        let file = self.companies.get(self.comp_sel).map(|c| c.file.as_str());
        (0..self.missions.len()).filter(|&i| self.missions[i].company.as_deref().is_none_or(|c| Some(c) == file)).collect()
    }

    /// Missions lancées avant le rattachement : retrouve leur entreprise via le nom du run (« entreprise · titre »).
    fn backfill_mission_companies(&mut self) {
        let mut changed = false;
        for m in self.missions.iter().filter(|m| m.company.is_none()) {
            let found = self.companies.iter().find(|c| {
                let Ok(co) = &c.parsed else { return false };
                let name = format!("{} · {}", co.name, m.title);
                self.history.iter().any(|r| r.pipeline == name)
            });
            if let Some(c) = found {
                changed |= missions::set_company(&m.dir, &c.file).is_ok();
            }
        }
        if changed {
            self.missions = missions::list(&self.root);
        }
    }

    /// Ramène la sélection de mission sur une mission visible pour l'entreprise courante.
    pub fn snap_mission_sel(&mut self) {
        let v = self.visible_missions();
        if !v.contains(&self.mission_sel) {
            self.mission_sel = v.first().copied().unwrap_or(0);
        }
    }

    /// Relit agents, entreprises et missions depuis le disque.
    pub fn reload(&mut self) {
        self.agents = agent::load_all(&self.root);
        let names: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
        self.companies = Self::load_companies(&self.root, &names);
        self.comp_sel = self.comp_sel.min(self.companies.len().saturating_sub(1));
        self.missions = missions::list(&self.root);
        self.mission_sel = self.mission_sel.min(self.missions.len().saturating_sub(1));
        self.history = store::load_all(&self.root);
        self.backfill_mission_companies();
        self.snap_mission_sel();
        self.hist_sel = self.hist_sel.min(self.history.len().saturating_sub(1));
        self.agent_sel = self.agent_sel.min(self.agents.len().saturating_sub(1));
    }

    /// Noms (sans `.toml`) des entreprises du projet.
    pub fn company_files(&self) -> Vec<String> {
        self.companies.iter().map(|c| c.file.trim_end_matches(".toml").to_string()).collect()
    }

    /// Éléments existants avant une session d'assistant (noms d'agents ou d'entreprises).
    pub fn names_for(&self, kind: AssistantKind) -> Vec<String> {
        match kind {
            AssistantKind::Agent => self.agents.iter().map(|a| a.name.clone()).collect(),
            AssistantKind::Mission | AssistantKind::Debrief => self.missions.iter().map(|m| m.id.clone()).collect(),
            AssistantKind::Company => self.company_files(),
            AssistantKind::Plan | AssistantKind::Edit | AssistantKind::Escalation => vec![],
        }
    }

    /// Au retour de l'assistant : recharge, sélectionne ce qui a été créé et signale les problèmes.
    pub fn after_assistant(&mut self, kind: AssistantKind, before: &[String]) {
        self.reload();
        match kind {
            AssistantKind::Agent => {
                let created: Vec<String> = self.agents.iter().filter(|a| !before.contains(&a.name)).map(|a| a.name.clone()).collect();
                let invalid = agent::invalid_files(&self.root);
                self.status = if let Some(name) = created.first() {
                    if let Some(i) = self.agents.iter().position(|a| &a.name == name) {
                        self.agent_sel = i;
                    }
                    tf!("agent « {name} » créé", "agent \"{name}\" created")
                } else if let Some((file, err)) = invalid.first() {
                    tf!("⚠ {file} est illisible : {err}", "⚠ {file} is unreadable: {err}")
                } else {
                    tr("aucun agent créé", "no agent created").into()
                };
            }
            AssistantKind::Company => {
                let invalid = agent::invalid_files(&self.root);
                let created = self.companies.iter().position(|c| !before.contains(&c.file.trim_end_matches(".toml").to_string()));
                self.status = match created {
                    Some(i) => {
                        self.comp_sel = i;
                        self.seat_sel = 0;
                        let e = &self.companies[i];
                        let name = e.file.trim_end_matches(".toml").to_string();
                        match (&e.parsed, &e.problem) {
                            (Err(err), _) => tf!("⚠ {} est illisible : {err}", "⚠ {} is unreadable: {err}", e.file),
                            (_, Some(pb)) => tf!("⚠ {} créée mais invalide : {pb}", "⚠ {} created but invalid: {pb}", e.file),
                            (Ok(c), None) => {
                                let n = c.seats.len();
                                let budget = c.estimate_calls().total();
                                let extra = if invalid.is_empty() { String::new() } else { tf!(" ⚠ agent illisible : {}", " ⚠ unreadable agent: {}", invalid[0].0) };
                                tf!("entreprise « {name} » créée : {n} postes, jusqu'à {budget} appels par mission — m pour cadrer une mission{extra}", "company \"{name}\" created: {n} seats, up to {budget} calls per mission — m to frame a mission{extra}")
                            }
                        }
                    }
                    None => match invalid.first() {
                        Some((file, err)) => tf!("⚠ {file} est illisible : {err}", "⚠ {file} is unreadable: {err}"),
                        None => tr("aucune entreprise créée", "no company created").into(),
                    },
                };
            }
            AssistantKind::Plan => self.after_plan_revision(),
            AssistantKind::Escalation => self.after_escalation_chat(),
            AssistantKind::Edit => {
                self.status = match agent::invalid_files(&self.root).first() {
                    Some((file, err)) => tf!("⚠ {file} est illisible : {err}", "⚠ {file} is unreadable: {err}"),
                    None => tr("modifications rechargées", "changes reloaded").into(),
                };
            }
            AssistantKind::Debrief => {
                let before_n = before.len();
                self.status = match self.missions.len().checked_sub(before_n) {
                    Some(n) if n > 0 => tf!("{n} nouvelle(s) mission(s) créée(s) — voir la liste des missions", "{n} new mission(s) created — see the missions list"),
                    _ => tr("discussion terminée — actions.md à jour si des actions ont été validées", "discussion over — actions.md is up to date if actions were approved").into(),
                };
            }
            AssistantKind::Mission => {
                if let Some(dir) = self.pending_mission_dir.take() {
                    let _ = std::fs::remove_dir(dir); // sans effet si un brief y a été écrit
                }
                self.status = match self.missions.iter().position(|m| !before.contains(&m.id)) {
                    Some(i) => {
                        self.mission_sel = i;
                        tf!("mission « {} » cadrée — r pour la lancer", "mission \"{}\" framed — r to run it", self.missions[i].title)
                    }
                    None => tr("aucun brief écrit : mission non créée", "no brief written: mission not created").into(),
                };
            }
        }
    }

    /// Au retour de la conversation de révision : le plan affiché au point de contrôle est relu depuis le fichier.
    fn after_plan_revision(&mut self) {
        let Some(dir) = self.run_ctx.as_ref().map(|(_, d)| d.clone()) else { return };
        let Some(cp) = self.checkpoint.as_mut() else {
            self.status = tr("le run n'attend plus de validation : le plan n'a pas été relu", "the run is no longer waiting for approval: the plan was not reloaded").into();
            return;
        };
        match std::fs::read_to_string(dir.join("conception.md")) {
            Ok(text) if !text.trim().is_empty() => {
                self.status = if text == cp.plan {
                    tr("plan inchangé — Entrée lance la réalisation", "plan unchanged — Enter starts realisation").into()
                } else {
                    cp.plan = text;
                    cp.scroll = 0;
                    tr("plan révisé — Entrée lance la réalisation avec ce plan", "plan revised — Enter starts realisation with this plan").into()
                };
            }
            _ => self.status = tr("⚠ conception.md illisible ou vide : plan d'origine conservé", "⚠ conception.md unreadable or empty: original plan kept").into(),
        }
    }

    /// `e` au point de contrôle : le boss discute le plan avec l'utilisateur et le réécrit.
    fn request_plan_revision(&mut self) {
        let Some((company, dir)) = self.run_ctx.clone() else { return };
        let Some(boss) = company.root() else { return };
        let seat = &company.seats[boss];
        let prompt = self.agents.iter().find(|a| a.name == seat.agent).map(|a| a.prompt.clone()).unwrap_or_default();
        let rel = |name: &str| dir.strip_prefix(&self.root).unwrap_or(&dir).join(name).display().to_string();
        let args = super::assistant::plan_revision_args(self.lang, &company.name, &seat.title, &prompt, &company.outline(), &rel("brief.md"), &rel("conception.md"));
        self.assistant_request = Some((AssistantKind::Plan, args));
    }

    /// Au retour de la discussion avec le CEO : dit si une décision a été écrite pour le prochain tour de correction.
    fn after_escalation_chat(&mut self) {
        let written = self.run_ctx.as_ref().is_some_and(|(_, d)| d.join(missions::RESOLUTION_FILE).is_file());
        self.status = if written {
            tr("décision enregistrée — Entrée relance la correction avec elle, Échap arrête", "decision saved — Enter retries the correction with it, Esc stops").into()
        } else {
            tr("aucune décision écrite — Entrée relance quand même, Échap arrête, e rouvre la discussion", "no decision written — Enter retries anyway, Esc stops, e reopens the discussion").into()
        };
    }

    /// `e` sur un blocage : le CEO en discute avec l'utilisateur et écrit sa décision.
    fn request_escalation_chat(&mut self) {
        let Some((company, dir)) = self.run_ctx.clone() else { return };
        let Some(es) = self.escalation.as_ref() else { return };
        let Some(boss) = company.root() else { return };
        let seat = &company.seats[boss];
        let prompt = self.agents.iter().find(|a| a.name == seat.agent).map(|a| a.prompt.clone()).unwrap_or_default();
        let rel = |name: &str| dir.strip_prefix(&self.root).unwrap_or(&dir).join(name).display().to_string();
        let args = super::assistant::escalation_args(self.lang, &company.name, &seat.title, &prompt, &company.outline(), &rel("brief.md"), &rel(missions::RESOLUTION_FILE), &es.summary);
        self.assistant_request = Some((AssistantKind::Escalation, args));
    }

    fn key_escalation(&mut self, k: KeyEvent) {
        let Some(es) = self.escalation.as_mut() else { return };
        match k.code {
            KeyCode::Enter => {
                let _ = es.reply.send(EscalationReply::Retry);
                self.escalation = None;
                self.status = tr("correction relancée", "correction retried").into();
            }
            KeyCode::Esc => {
                let _ = es.reply.send(EscalationReply::Stop);
                self.escalation = None;
                self.status = tr("mission arrêtée sur le blocage", "mission stopped on the blockage").into();
            }
            KeyCode::Char('e') => self.request_escalation_chat(),
            KeyCode::Down | KeyCode::Char('j') => es.scroll = es.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => es.scroll = es.scroll.saturating_sub(1),
            KeyCode::PageDown => es.scroll = es.scroll.saturating_add(10),
            KeyCode::PageUp => es.scroll = es.scroll.saturating_sub(10),
            _ => {}
        }
    }

    /// Charge la langue mémorisée ; sans réglage, le menu de langue s'ouvre avant tout le reste.
    pub fn load_settings(&mut self, path: Option<PathBuf>) {
        if let Some(p) = &path {
            match settings::load(p).lang {
                Some(l) => {
                    self.lang = l;
                    settings::set_current(l);
                }
                None => self.settings_menu = Some(SettingsMenu { first_run: true, sel: 0 }),
            }
        }
        self.settings_path = path;
    }

    fn open_settings(&mut self) {
        self.settings_menu = Some(SettingsMenu { first_run: false, sel: Lang::ALL.iter().position(|l| *l == self.lang).unwrap_or(0) });
    }

    fn key_settings(&mut self, k: KeyEvent) {
        let Some(m) = self.settings_menu.as_mut() else { return };
        let n = Lang::ALL.len();
        match k.code {
            KeyCode::Down | KeyCode::Right | KeyCode::Char('j') => m.sel = (m.sel + 1) % n,
            KeyCode::Up | KeyCode::Left | KeyCode::Char('k') => m.sel = (m.sel + n - 1) % n,
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < n => {
                m.sel = c as usize - '1' as usize;
                self.confirm_settings();
            }
            KeyCode::Enter => self.confirm_settings(),
            KeyCode::Esc if !m.first_run => self.settings_menu = None,
            _ => {}
        }
    }

    fn confirm_settings(&mut self) {
        let Some(m) = self.settings_menu.take() else { return };
        self.lang = Lang::ALL[m.sel];
        settings::set_current(self.lang);
        if let Some(p) = &self.settings_path {
            if let Err(e) = settings::save(p, &Settings { lang: Some(self.lang) }) {
                self.status = tf!("réglages non enregistrés : {e:#}", "settings not saved: {e:#}");
            }
        }
    }

    fn help_scroll_by(&mut self, delta: i32) {
        let max = self.help_max.get() as i32;
        self.help_scroll = (self.help_scroll as i32 + delta).clamp(0, max) as u16;
    }

    pub fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.finished.is_none())
    }

    pub fn on_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Key(k) => self.on_key(k),
            AppEvent::Run(r) => self.on_run_event(r),
            AppEvent::Tick => {
                self.tick = self.tick.wrapping_add(1);
                // Rafraîchissement automatique toutes les ~3 s sur l'onglet Sessions.
                if self.tab == Tab::Sessions && self.tick % 15 == 0 {
                    self.refresh_sessions();
                }
            }
            AppEvent::Sessions { list, error } => self.on_sessions(list, error),
            AppEvent::TermOutput => {}
            AppEvent::TermExit(id) => self.on_term_exit(id),
            AppEvent::Paste(text) => {
                if let (true, Tab::Terminal, Some(t)) = (self.term_focus, self.tab, self.term.as_mut()) {
                    t.paste(&text);
                }
            }
            AppEvent::Notice(msg) => {
                self.status = msg;
                self.refresh_sessions();
            }
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        // Comme un volet de terminal : on y entre avec le focus, on le perd en le quittant.
        self.term_focus = tab == Tab::Terminal && self.term.as_ref().is_some_and(|t| !t.exited);
        if tab == Tab::Sessions {
            self.refresh_sessions();
        }
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.sess_sel)
    }

    /// Relit la transcription de la session sélectionnée (lecture locale, rapide).
    fn load_detail(&mut self) {
        self.sess_detail = self
            .selected_session()
            .and_then(|s| s.session_id.as_deref())
            .and_then(sessions::find_transcript)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|src| sessions::transcript_lines(&src))
            .unwrap_or_default();
    }

    /// Lance `claude agents --json --all` en tâche de fond ; sans effet hors runtime tokio (tests).
    pub fn refresh_sessions(&mut self) {
        if self.refreshing || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        self.refreshing = true;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let ev = match sessions::list().await {
                Ok(list) => AppEvent::Sessions { list: Some(list), error: None },
                Err(e) => AppEvent::Sessions { list: None, error: Some(format!("{e:#}")) },
            };
            let _ = tx.send(ev);
        });
    }

    fn on_sessions(&mut self, list: Option<Vec<Session>>, error: Option<String>) {
        self.refreshing = false;
        self.sess_error = error;
        if let Some(list) = list {
            let prev = self.selected_session().and_then(|s| s.id.clone());
            self.sessions = list;
            self.sess_sel = prev
                .and_then(|id| self.sessions.iter().position(|s| s.id.as_deref() == Some(id.as_str())))
                .unwrap_or(self.sess_sel)
                .min(self.sessions.len().saturating_sub(1));
            self.load_detail();
        }
    }

    /// Exécute une action sur une session puis notifie l'UI.
    fn session_action(&mut self, id: String, label: &'static str, remove: bool) {
        if tokio::runtime::Handle::try_current().is_err() {
            self.status = tr("action indisponible : pas de runtime asynchrone", "action unavailable: no async runtime").into();
            return;
        }
        let tx = self.tx.clone();
        let lang = self.lang;
        tokio::spawn(async move {
            let res = if remove { sessions::remove(&id).await } else { sessions::stop(&id).await };
            settings::set_current(lang); // fil du runtime : la langue est posée juste avant de formater
            let msg = match res {
                Ok(()) => tf!("session {id} {label}", "session {id} {label}"),
                Err(e) => erreur(&e),
            };
            let _ = tx.send(AppEvent::Notice(msg));
        });
    }

    /// Dimensionne le terminal intégré sur la zone centrale de l'écran.
    pub fn set_body_size(&mut self, cols: u16, rows: u16) {
        self.body_size = (rows, cols);
        if let Some(t) = self.term.as_mut() {
            t.resize(rows, cols);
        }
    }

    /// Ouvre la session demandée (assistant ou attache) dans le terminal intégré.
    pub fn launch_pending(&mut self) {
        let assistant = self.assistant_request.take();
        let attach = self.attach_request.take();
        if assistant.is_none() && attach.is_none() {
            return;
        }
        if self.term.is_some() {
            self.status = tr("une session Claude est déjà ouverte (onglet 5)", "a Claude session is already open (tab 5)").into();
            return;
        }
        let (kind, args, title, origin) = match (assistant, attach) {
            (Some((k, args)), _) => {
                let (title, origin) = match k {
                    AssistantKind::Agent => ("nouvel agent", Tab::Agents),
                    AssistantKind::Mission => ("brief de mission", Tab::Company),
                    AssistantKind::Company => ("nouvelle entreprise", Tab::Company),
                    AssistantKind::Plan => ("révision du plan", Tab::Run),
                    AssistantKind::Edit => ("modification", self.tab),
                    AssistantKind::Debrief => ("compte rendu", Tab::Company),
                    AssistantKind::Escalation => ("discussion avec le CEO", Tab::Run),
                };
                (Some(k), args, title.to_string(), origin)
            }
            (None, Some(id)) => (None, vec!["attach".to_string(), id.clone()], format!("attache {id}"), Tab::Sessions),
            (None, None) => return,
        };
        let before = kind.map(|k| self.names_for(k)).unwrap_or_default();
        let bin = std::env::var("ESSAIM_CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
        self.term_seq += 1;
        match TermSession::spawn(&bin, &args, &self.root, self.body_size, self.term_seq, self.tx.clone()) {
            Ok(mut t) => {
                t.title = title;
                t.kind = kind;
                t.origin = origin;
                t.before = before;
                self.term = Some(t);
                self.term_focus = true;
                self.tab = Tab::Terminal;
                self.status.clear();
            }
            Err(e) => self.status = erreur(&e),
        }
    }

    fn on_term_exit(&mut self, id: u64) {
        let Some(t) = self.term.as_mut().filter(|t| t.id == id) else { return };
        t.mark_exited();
        if t.ok() && t.started.elapsed() > std::time::Duration::from_secs(2) {
            self.finish_term();
        } else {
            // Sortie rapide ou en erreur : on garde l'écran pour que le message reste lisible.
            self.term_focus = false;
        }
    }

    /// Ferme le terminal intégré, revient à l'onglet d'origine et exploite le résultat.
    fn finish_term(&mut self) {
        let Some(mut t) = self.term.take() else { return };
        t.kill();
        self.term_focus = false;
        self.tab = t.origin;
        match t.kind {
            Some(kind) => self.after_assistant(kind, &t.before),
            None => self.refresh_sessions(),
        }
        if t.exited && !t.ok() {
            let code = t.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
            self.status = tf!("{} (la session s'est terminée avec le code {code})", "{} (the session ended with code {code})", self.status);
        }
    }

    /// Touches destinées au terminal intégré. Renvoie `true` si la touche est consommée.
    fn term_key(&mut self, k: KeyEvent) -> bool {
        if self.tab != Tab::Terminal {
            return false;
        }
        let Some(t) = self.term.as_mut() else { return false };
        if t.exited {
            self.finish_term();
            return true;
        }
        if !self.term_focus {
            return false;
        }
        // Ctrl+\ : crossterm le livre sous la forme Ctrl+'4' (octet 0x1c) sur Unix.
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(k.code, KeyCode::Char('\\') | KeyCode::Char('4')) {
            self.term_focus = false;
            return true;
        }
        if let Some(bytes) = encode_key(k, t.app_cursor()) {
            t.send(&bytes);
        }
        true
    }

    /// `q` / Ctrl+C : sortie directe, sauf si un run est en cours (confirmation, il serait annulé).
    fn request_quit(&mut self) {
        if let Some(r) = self.run.as_ref().filter(|r| r.finished.is_none()) {
            self.mode = Mode::ConfirmAction(PendingAction::QuitRun(r.name.clone()));
            return;
        }
        self.quit_now();
    }

    fn quit_now(&mut self) {
        if let Some(t) = self.term.as_mut() {
            t.kill();
        }
        if let Some(r) = &self.run {
            let _ = r.cancel.send(true);
        }
        self.quit = true;
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        settings::set_current(self.lang);
        if k.kind == KeyEventKind::Release {
            return;
        }
        // Aide : Ctrl+K l'ouvre/la ferme partout (même dans la session Claude) ; ouverte, elle capte les touches.
        if self.settings_menu.is_some() {
            self.key_settings(k);
            return;
        }
        let ctrl_k = k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('k') | KeyCode::Char('K'));
        if self.help_open {
            let page = self.help_page.get().max(1) as i32;
            match k.code {
                _ if ctrl_k => self.help_open = false,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => self.help_open = false,
                KeyCode::Char('m') => {
                    self.help_full = !self.help_full;
                    self.help_scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Enter => self.help_scroll_by(1),
                KeyCode::Up | KeyCode::Char('k') => self.help_scroll_by(-1),
                KeyCode::PageDown | KeyCode::Char(' ') | KeyCode::Char('f') => self.help_scroll_by(page - 1),
                KeyCode::PageUp | KeyCode::Char('b') => self.help_scroll_by(-(page - 1)),
                KeyCode::Char('d') => self.help_scroll_by(page / 2),
                KeyCode::Char('u') => self.help_scroll_by(-(page / 2)),
                KeyCode::Home | KeyCode::Char('g') => self.help_scroll = 0,
                KeyCode::End | KeyCode::Char('G') => self.help_scroll = self.help_max.get(),
                _ => {}
            }
            return;
        }
        if ctrl_k {
            self.help_open = true;
            self.help_full = false;
            self.help_scroll = 0;
            return;
        }
        if k.code == KeyCode::F(10) && matches!(self.mode, Mode::Normal) {
            self.open_settings();
            return;
        }
        // Raccourcis d'onglet : prioritaires, même quand le terminal intégré a le focus.
        if matches!(self.mode, Mode::Normal) {
            if let Some(nav) = tab_shortcut(&k) {
                match nav {
                    TabNav::Goto(t) => self.set_tab(t),
                    TabNav::Next => self.shift_tab(1),
                    TabNav::Prev => self.shift_tab(Tab::ALL.len() - 1),
                }
                return;
            }
        }
        // Dans le terminal intégré, tout (Ctrl+C compris) va au processus, sauf Ctrl+\ et les raccourcis ci-dessus.
        if self.term_key(k) {
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.request_quit();
            return;
        }
        if self.mission_view.is_some() {
            self.key_mission_view(k);
            return;
        }
        // Le point de contrôle ne concerne que l'onglet Run (il y bascule à son arrivée).
        if self.checkpoint.is_some() && self.tab == Tab::Run {
            self.key_checkpoint(k);
            return;
        }
        if self.escalation.is_some() && self.tab == Tab::Run {
            self.key_escalation(k);
            return;
        }
        self.status.clear();
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::EditAgent(form) => self.key_form(form, k),
            Mode::EditSeat(form) => self.key_seat_form(form, k),
            Mode::ChooseEdit(c) => self.key_choose_edit(c, k),
            Mode::ConfirmAction(a) => match k.code {
                KeyCode::Char('o' | 'O' | 'y' | 'Y') => self.run_action(a),
                // Refus, Échap ou toute autre touche : par défaut, l'action n'a pas lieu.
                _ => self.status = tr("Annulé.", "Cancelled.").into(),
            },
            Mode::Normal => self.key_normal(k),
        }
    }

    /// Onglet Entreprise : les flèches changent de zone ; Entrée y entre (flèches locales), Échap en sort.
    fn company_nav(&mut self, k: KeyEvent) {
        let (up, down, left, right) = (
            matches!(k.code, KeyCode::Up | KeyCode::Char('k')),
            matches!(k.code, KeyCode::Down | KeyCode::Char('j')),
            matches!(k.code, KeyCode::Left | KeyCode::Char('h')),
            matches!(k.code, KeyCode::Right | KeyCode::Char('l')),
        );
        if k.code == KeyCode::Esc {
            self.zone_in = false;
            return;
        }
        if !self.zone_in {
            match k.code {
                KeyCode::Enter => self.zone_in = true,
                _ if left => self.zone = match self.zone { CompZone::Missions => CompZone::Companies, z => z },
                _ if right => self.zone = match self.zone { CompZone::Companies => CompZone::Missions, z => z },
                _ if down => self.zone = CompZone::Graph,
                _ if up && self.zone == CompZone::Graph => self.zone = CompZone::Missions,
                _ => {}
            }
            return;
        }
        match self.zone {
            CompZone::Companies => {
                if (down || up) && !self.companies.is_empty() {
                    let n = self.companies.len();
                    self.comp_sel = if down { (self.comp_sel + 1) % n } else { (self.comp_sel + n - 1) % n };
                    self.seat_sel = 0;
                    self.snap_mission_sel();
                }
                if k.code == KeyCode::Enter {
                    self.zone = CompZone::Missions;
                }
            }
            CompZone::Missions => {
                let v = self.visible_missions();
                if let Some(pos) = v.iter().position(|&i| i == self.mission_sel) {
                    let n = v.len();
                    if down {
                        self.mission_sel = v[(pos + 1) % n];
                    } else if up {
                        self.mission_sel = v[(pos + n - 1) % n];
                    }
                }
                if k.code == KeyCode::Enter {
                    self.open_mission();
                } else if k.code == KeyCode::Char(' ') || left || right {
                    self.toggle_summary(if left { Some(false) } else if right { Some(true) } else { None });
                }
            }
            CompZone::Graph => {
                // ↑ supérieur · ↓ premier subordonné · ← → voisin du même niveau.
                let graph = self.companies.get(self.comp_sel).and_then(|c| c.parsed.as_ref().ok()).map(|c| c.graph()).unwrap_or_default();
                let sel = self.seat_sel.min(graph.len().saturating_sub(1));
                let target = match k.code {
                    _ if up => graph_nav::parent(&graph, sel),
                    _ if down => graph_nav::first_child(&graph, sel),
                    _ if left => graph_nav::side(&graph, sel, false),
                    _ if right => graph_nav::side(&graph, sel, true),
                    KeyCode::Home | KeyCode::Char('g') => (!graph.is_empty()).then_some(0),
                    _ => None,
                };
                if let Some(t) = target {
                    self.seat_sel = t;
                }
                if k.code == KeyCode::Enter {
                    self.open_seat_form(true);
                }
            }
        }
    }

    fn key_normal(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('q') => return self.request_quit(),
            KeyCode::Tab => return self.shift_tab(1),
            KeyCode::BackTab => return self.shift_tab(Tab::ALL.len() - 1),
            KeyCode::Char(c @ '1'..='5') => {
                self.set_tab(Tab::ALL[c as usize - '1' as usize]);
                return;
            }
            _ => {}
        }
        let down = matches!(k.code, KeyCode::Char('j') | KeyCode::Down);
        let up = matches!(k.code, KeyCode::Char('k') | KeyCode::Up);
        match self.tab {
            Tab::Agents => {
                if down || up {
                    self.agent_sel = step(self.agent_sel, self.agents.len(), down);
                }
                match k.code {
                    KeyCode::Char('n') => {
                        let names: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
                        self.assistant_request = Some((AssistantKind::Agent, super::assistant::agent_creation_args(self.lang, &names)));
                    }
                    KeyCode::Char('N') => self.mode = Mode::EditAgent(AgentForm::new(None)),
                    KeyCode::Char('e') | KeyCode::Enter => {
                        if let Some(a) = self.agents.get(self.agent_sel) {
                            let file = self.root.join(".claude/agents").join(format!("{}.md", a.name));
                            self.mode = Mode::ChooseEdit(EditChoice { what: EditWhat::Agent(a.name.clone()), file, label: tf!("l'agent « {} »", "the agent \"{}\"", a.name), sel: 0 });
                        }
                    }
                    KeyCode::Char('D') => {
                        if let Some(a) = self.agents.get(self.agent_sel) {
                            self.mode = Mode::ConfirmAction(PendingAction::DeleteAgent(a.name.clone()));
                        }
                    }
                    _ => {}
                }
            }
            Tab::Company => {
                self.company_nav(k);
                match k.code {
                    KeyCode::Char('c') if !self.companies.is_empty() => {
                        self.comp_sel = (self.comp_sel + 1) % self.companies.len();
                        self.seat_sel = 0;
                        self.snap_mission_sel();
                    }
                    KeyCode::Char('C') if !self.companies.is_empty() => {
                        self.comp_sel = (self.comp_sel + self.companies.len() - 1) % self.companies.len();
                        self.seat_sel = 0;
                        self.snap_mission_sel();
                    }
                    KeyCode::Char('n') => {
                        let agents: Vec<(String, String)> = self.agents.iter().map(|a| (a.name.clone(), a.description.clone())).collect();
                        let files = self.company_files();
                        self.assistant_request = Some((AssistantKind::Company, super::assistant::company_creation_args(self.lang, &agents, &files)));
                    }
                    // L'édition dépend de la zone où l'on est entré : poste (organigramme), entreprise ou mission.
                    KeyCode::Char('a') if self.zone_in && self.zone == CompZone::Graph => self.open_seat_form(false),
                    KeyCode::Char('e') if self.zone_in => match self.zone {
                        CompZone::Graph => {
                            if let Some((path, c, i)) = self.selected_seat() {
                                let s = &c.seats[i];
                                self.mode = Mode::ChooseEdit(EditChoice { what: EditWhat::Seat(s.id.clone()), file: path, label: tf!("le poste « {} »", "the seat \"{}\"", s.title), sel: 0 });
                            }
                        }
                        CompZone::Companies => {
                            if let Some(c) = self.companies.get(self.comp_sel) {
                                self.mode = Mode::ChooseEdit(EditChoice { what: EditWhat::Company, file: c.path.clone(), label: tf!("l'entreprise « {} »", "the company \"{}\"", c.file), sel: 0 });
                            }
                        }
                        CompZone::Missions => {
                            if self.visible_missions().contains(&self.mission_sel) {
                                if let Some(m) = self.missions.get(self.mission_sel) {
                                    self.mode = Mode::ChooseEdit(EditChoice { what: EditWhat::Mission, file: m.dir.join("brief.md"), label: tf!("le brief « {} »", "the brief \"{}\"", m.title), sel: 0 });
                                }
                            }
                        }
                    },
                    KeyCode::Char('x') if self.zone_in && self.zone == CompZone::Graph => {
                        if let Some((path, c, i)) = self.selected_seat() {
                            let s = &c.seats[i];
                            self.mode = Mode::ConfirmAction(PendingAction::DeleteSeat(path, s.id.clone(), s.title.clone()));
                        }
                    }
                    KeyCode::Char('m') => self.request_mission_brief(),
                    KeyCode::Char('o') => self.open_mission(),
                    KeyCode::Char('r') => self.start_mission(true),
                    KeyCode::Char('R') => self.start_mission(false),
                    KeyCode::Char('t' | 'T') => {
                        let v = self.visible_missions();
                        if let Some(pos) = v.iter().position(|&i| i == self.mission_sel) {
                            let n = v.len();
                            self.mission_sel = v[if k.code == KeyCode::Char('t') { (pos + 1) % n } else { (pos + n - 1) % n }];
                        }
                    }
                    _ => {}
                }
            }
            Tab::Run => {
                if k.code == KeyCode::Char('h') {
                    self.show_history = !self.show_history;
                    return;
                }
                if self.show_history {
                    if down || up {
                        self.hist_sel = step(self.hist_sel, self.history.len(), down);
                    }
                    match k.code {
                        KeyCode::Enter => self.open_history(),
                        KeyCode::Char('D') => {
                            if let Some(h) = self.history.get(self.hist_sel) {
                                self.mode = Mode::ConfirmAction(PendingAction::DeleteHistory(h.id.clone(), h.pipeline.clone()));
                            }
                        }
                        _ => {}
                    }
                } else if let Some(r) = &mut self.run {
                    if down || up {
                        r.sel = step(r.sel, r.nodes.len(), down);
                    }
                    if k.code == KeyCode::Char('s') && r.finished.is_none() {
                        self.mode = Mode::ConfirmAction(PendingAction::CancelRun(r.name.clone()));
                    }
                }
            }
            Tab::Terminal => match k.code {
                KeyCode::Enter | KeyCode::Char('i') if self.term.is_some() => self.term_focus = true,
                KeyCode::Char('x') if self.term.is_some() => self.mode = Mode::ConfirmAction(PendingAction::FinishTerm),
                _ => {}
            },
            Tab::Sessions => {
                if down || up {
                    self.sess_sel = step(self.sess_sel, self.sessions.len(), down);
                    self.load_detail();
                }
                let Some(s) = self.selected_session().cloned() else {
                    if k.code == KeyCode::Char('r') {
                        self.refresh_sessions();
                    }
                    return;
                };
                let (id, name) = (s.id.clone().unwrap_or_default(), s.label());
                match k.code {
                    KeyCode::Char('r') => self.refresh_sessions(),
                    KeyCode::Char('s') => {
                        self.mode = Mode::ConfirmAction(PendingAction::StopSession(id, name));
                    }
                    KeyCode::Char('x') => self.mode = Mode::ConfirmAction(PendingAction::RemoveSession(id, name)),
                    KeyCode::Char('a') | KeyCode::Enter => self.attach_request = Some(id),
                    _ => {}
                }
            }
        }
    }

    /// Validation du plan avant la réalisation : Entrée lance, Échap arrête, flèches défilent.
    /// `Entrée` / `o` : ouvre la mission sélectionnée (brief, plan, rapport) en lecture.
    fn open_mission(&mut self) {
        let Some(m) = self.visible_missions().contains(&self.mission_sel).then(|| self.missions.get(self.mission_sel)).flatten() else {
            self.status = tr("aucune mission : m pour en cadrer une avec le boss", "no mission: m to frame one with the boss").into();
            return;
        };
        let mut md = String::new();
        for (name, label) in [
            ("compte-rendu.md", ""),
            ("rapport.md", tr("Rapport du boss", "Boss report")),
            ("conception.md", tr("Plan (conception)", "Plan (design)")),
            ("brief.md", "Brief"),
        ] {
            if let Ok(t) = std::fs::read_to_string(m.dir.join(name)) {
                // Le compte rendu cite déjà le rapport final : on ne le répète pas.
                if name == "rapport.md" && md.contains("compte-rendu") {
                    continue;
                }
                if !label.is_empty() {
                    md.push_str(&format!("# {label}\n\n"));
                }
                md.push_str(t.trim_end());
                md.push_str("\n\n---\n\n");
            }
        }
        if md.is_empty() {
            md = tr("_Cette mission n'a encore aucun contenu._", "_This mission has no content yet._").into();
        }
        let width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(100).min(96).saturating_sub(4);
        self.mission_view = Some(MissionView { title: m.title.clone(), dir: m.dir.clone(), text: super::markdown::render(&md, width), scroll: 0 });
    }

    /// Déplie / replie le résumé de la mission sélectionnée (`want` : Some(true) déplie, Some(false) replie).
    fn toggle_summary(&mut self, want: Option<bool>) {
        let Some(m) = self.visible_missions().contains(&self.mission_sel).then(|| self.missions.get(self.mission_sel)).flatten() else { return };
        let open = self.mission_open.contains_key(&m.id);
        if want.unwrap_or(!open) {
            if !open {
                let s = missions::summarize(&m.dir);
                self.mission_open.insert(m.id.clone(), s);
            }
        } else {
            self.mission_open.remove(&m.id);
        }
    }

    /// `d` dans la vue d'une mission : conversation avec le modèle sur son compte rendu.
    fn start_debrief(&mut self, dir: &std::path::Path) {
        if !["compte-rendu.md", "rapport.md", "conception.md"].iter().any(|n| dir.join(n).exists()) {
            self.status = tr("rien à discuter : la mission n'a pas encore été lancée", "nothing to discuss: the mission has not run yet").into();
            return;
        }
        let rel = dir.strip_prefix(&self.root).unwrap_or(dir).display().to_string();
        let followup = missions::new_id(store::now_ms() + 1);
        let agents: Vec<(String, String)> = self.agents.iter().map(|a| (a.name.clone(), a.description.clone())).collect();
        let companies: Vec<(String, Vec<String>)> = self.companies.iter().filter_map(|c| c.parsed.as_ref().ok().map(|p| (c.file.clone(), p.outline()))).collect();
        let args = super::assistant::debrief_args(self.lang, &rel, &followup, &agents, &companies);
        self.assistant_request = Some((AssistantKind::Debrief, args));
    }

    fn key_mission_view(&mut self, k: KeyEvent) {
        let Some(v) = self.mission_view.as_mut() else { return };
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('o') => self.mission_view = None,
            KeyCode::Char('d') => {
                let dir = v.dir.clone();
                self.mission_view = None;
                self.start_debrief(&dir);
            }
            KeyCode::Down | KeyCode::Char('j') => v.scroll = v.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => v.scroll = v.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => v.scroll = v.scroll.saturating_add(15),
            KeyCode::PageUp | KeyCode::Char('b') => v.scroll = v.scroll.saturating_sub(15),
            KeyCode::Home | KeyCode::Char('g') => v.scroll = 0,
            _ => {}
        }
    }

    fn key_checkpoint(&mut self, k: KeyEvent) {
        let Some(cp) = self.checkpoint.as_mut() else { return };
        match k.code {
            KeyCode::Enter => {
                let _ = cp.reply.send(true);
                self.checkpoint = None;
                self.status = tr("réalisation lancée", "realisation started").into();
            }
            KeyCode::Esc => {
                let _ = cp.reply.send(false);
                self.checkpoint = None;
                self.status = tr("réalisation refusée : la mission s'arrête sur le plan", "realisation refused: the mission stops at the plan").into();
            }
            KeyCode::Char('e') => self.request_plan_revision(),
            KeyCode::Down | KeyCode::Char('j') => cp.scroll = cp.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => cp.scroll = cp.scroll.saturating_sub(1),
            KeyCode::PageDown => cp.scroll = cp.scroll.saturating_add(10),
            KeyCode::PageUp => cp.scroll = cp.scroll.saturating_sub(10),
            _ => {}
        }
    }

    fn shift_tab(&mut self, by: usize) {
        self.set_tab(Tab::ALL[(self.tab.index() + by) % Tab::ALL.len()]);
    }

    fn key_form(&mut self, mut form: AgentForm, k: KeyEvent) {
        let n = form.fields.len();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => return, // annule (mode déjà remis à Normal)
            KeyCode::Char('s') if ctrl => {
                // Renommer un agent reporte le nouveau nom dans les postes qui le citent.
                let new_name = form.to_def().name;
                let renamed_from = form.original.clone().filter(|old| !old.is_empty() && *old != new_name);
                match self.save_form(&form) {
                    Ok(name) => {
                        let (done, failed) = renamed_from.as_deref().map(|o| self.rewire_agent(o, &name)).unwrap_or_default();
                        self.status = match (done.is_empty(), failed.is_empty()) {
                            (true, true) => tf!("agent « {name} » enregistré", "agent \"{name}\" saved"),
                            (false, true) => tf!("agent « {name} » enregistré · postes mis à jour : {}", "agent \"{name}\" saved · seats updated: {}", done.join(", ")),
                            (_, false) => tf!("agent « {name} » enregistré ⚠ postes NON mis à jour : {}", "agent \"{name}\" saved ⚠ seats NOT updated: {}", failed.join(", ")),
                        };
                    }
                    Err(e) => {
                        self.status = erreur(&e);
                        self.mode = Mode::EditAgent(form);
                    }
                }
                return;
            }
            KeyCode::Tab => form.focus = (form.focus + 1) % n,
            KeyCode::BackTab => form.focus = (form.focus + n - 1) % n,
            KeyCode::Enter if form.focus < n - 1 => form.focus += 1,
            _ => {
                form.fields[form.focus].handle_key(k);
            }
        }
        self.mode = Mode::EditAgent(form);
    }

    fn save_form(&mut self, form: &AgentForm) -> anyhow::Result<String> {
        let def = form.to_def();
        if def.description.is_empty() {
            anyhow::bail!("{}", tr("la description est obligatoire", "the description is required"));
        }
        agent::save(&self.root, &def)?;
        if let Some(old) = &form.original {
            if old != &def.name {
                agent::delete(&self.root, old)?;
            }
        }
        self.reload();
        if let Some(i) = self.agents.iter().position(|a| a.name == def.name) {
            self.agent_sel = i;
        }
        Ok(def.name)
    }

    /// Entreprise lisible sélectionnée et poste sélectionné dans l'arbre.
    fn selected_seat(&mut self) -> Option<(PathBuf, Company, usize)> {
        let Some(entry) = self.companies.get(self.comp_sel) else {
            self.status = tr("aucune entreprise : `n` en crée une avec l'assistant", "no company: `n` creates one with the assistant").into();
            return None;
        };
        let Ok(c) = &entry.parsed else {
            self.status = tr("entreprise illisible : corrigez-la d'abord avec E", "unreadable company: fix it first with E").into();
            return None;
        };
        let i = c.tree_rows().get(self.seat_sel).map(|r| r.idx)?;
        Some((entry.path.clone(), c.clone(), i))
    }

    /// `a` : nouveau poste sous le poste sélectionné ; `e` : édite le poste sélectionné.
    /// Fenêtre de choix du mode d'édition : ←→/↑↓/Tab changent, Entrée valide, 1 et 2 choisissent, Échap annule.
    fn key_choose_edit(&mut self, mut c: EditChoice, k: KeyEvent) {
        let go = match k.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('h' | 'j' | 'k' | 'l') => {
                c.sel = 1 - c.sel;
                self.mode = Mode::ChooseEdit(c);
                return;
            }
            KeyCode::Char('1' | 's' | 'S') => 0,
            KeyCode::Char('2' | 'm' | 'M') => 1,
            KeyCode::Enter => c.sel,
            _ => {
                self.status = tr("Annulé.", "Cancelled.").into();
                return;
            }
        };
        if go == 1 {
            let what = match &c.what {
                EditWhat::Agent(n) => format!("the subagent \"{n}\" (a Markdown file with YAML frontmatter: name, description, model, tools; then the system prompt)"),
                EditWhat::Seat(id) => format!("the seat with id \"{id}\" in the company file (a TOML file: [[seats]] tables; keep every other seat unchanged and keep reports_to links consistent)"),
                EditWhat::Company => "the company file (a TOML file: a tree of [[seats]]; keep reports_to links consistent)".to_string(),
                EditWhat::Mission => "the mission brief (a Markdown file)".to_string(),
            };
            let agents: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
            let args = super::assistant::edit_args(self.lang, &what, &c.file.to_string_lossy(), &agents);
            self.assistant_request = Some((AssistantKind::Edit, args));
            return;
        }
        match c.what {
            EditWhat::Agent(name) => {
                if let Some(a) = self.agents.iter().find(|a| a.name == name) {
                    self.mode = Mode::EditAgent(AgentForm::new(Some(a)));
                }
            }
            EditWhat::Seat(_) => self.open_seat_form(true),
            EditWhat::Company | EditWhat::Mission => self.editor_request = Some(c.file),
        }
    }

    fn open_seat_form(&mut self, edit: bool) {
        let Some((path, c, i)) = self.selected_seat() else { return };
        let agents: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
        if agents.is_empty() {
            self.status = tr("aucun agent : créez-en un dans l'onglet Agents", "no agent: create one in the Agents tab").into();
            return;
        }
        let form = if edit { SeatForm::new(&c, path, &agents, Some(i), None) } else { SeatForm::new(&c, path, &agents, None, Some(i)) };
        self.mode = Mode::EditSeat(form);
    }

    fn key_seat_form(&mut self, mut form: SeatForm, k: KeyEvent) {
        let n = form.fields.len();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // Vérification en cours : « o » confirme, toute autre touche revient au formulaire.
        if let Some(validate) = form.confirm.take() {
            if matches!(k.code, KeyCode::Char('o' | 'O' | 'y' | 'Y')) {
                if validate {
                    return self.submit_seat_form(form);
                }
                self.status = tr("Modifications abandonnées.", "Changes discarded.").into();
                return;
            }
            self.mode = Mode::EditSeat(form);
            return;
        }
        match k.code {
            KeyCode::Esc if form.changed() => form.confirm = Some(false),
            KeyCode::Esc => return,
            KeyCode::Char('s') if ctrl => form.confirm = Some(true),
            KeyCode::Up => form.focus = form.focus.saturating_sub(1),
            KeyCode::Down => form.focus = (form.focus + 1).min(n + SEAT_BUTTONS - 1),
            KeyCode::Tab => form.focus = (form.focus + 1) % (n + SEAT_BUTTONS),
            KeyCode::BackTab => form.focus = (form.focus + n + SEAT_BUTTONS - 1) % (n + SEAT_BUTTONS),
            KeyCode::Left | KeyCode::Right if form.focus >= n => form.focus = if form.focus == n { n + 1 } else { n },
            KeyCode::Enter if form.focus == n => form.confirm = Some(true),
            KeyCode::Enter if form.focus == n + 1 => {
                if !form.changed() {
                    return;
                }
                form.confirm = Some(false);
            }
            KeyCode::Enter => form.focus += 1,
            _ if form.focus >= n => {}
            _ => {
                form.fields[form.focus].handle_key(k);
            }
        }
        self.mode = Mode::EditSeat(form);
    }

    fn submit_seat_form(&mut self, form: SeatForm) {
        match self.save_seat(&form) {
            Ok(id) => self.status = tf!("poste « {id} » enregistré", "seat \"{id}\" saved"),
            Err(e) => {
                self.status = erreur(&e);
                self.mode = Mode::EditSeat(form);
            }
        }
    }

    fn save_seat(&mut self, form: &SeatForm) -> anyhow::Result<String> {
        let mut c = Company::load(&form.path)?;
        let seat = form.to_seat();
        if seat.title.is_empty() {
            anyhow::bail!("{}", tr("l'intitulé du poste est obligatoire", "the job title is required"));
        }
        match form.original.as_deref().and_then(|o| c.index_of(o)) {
            Some(i) => {
                let old = c.seats[i].id.clone();
                if old != seat.id {
                    for other in c.seats.iter_mut().filter(|s| s.reports_to.as_deref() == Some(old.as_str())) {
                        other.reports_to = Some(seat.id.clone());
                    }
                }
                c.seats[i] = seat.clone();
            }
            None => c.seats.push(seat.clone()),
        }
        let names: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
        c.validate(&names)?;
        c.save(&form.path)?;
        self.reload();
        if let Some(entry) = self.companies.iter().position(|e| e.path == form.path) {
            self.comp_sel = entry;
        }
        if let Some(Ok(c)) = self.companies.get(self.comp_sel).map(|e| &e.parsed) {
            if let Some(row) = c.tree_rows().iter().position(|r| c.seats[r.idx].id == seat.id) {
                self.seat_sel = row;
            }
        }
        Ok(seat.id)
    }

    /// Supprime un poste ; ses subordonnés remontent d'un cran. Le boss ne part que s'il est seul.
    fn delete_seat(&mut self, path: &std::path::Path, id: &str) -> anyhow::Result<()> {
        let mut c = Company::load(path)?;
        let i = c.index_of(id).ok_or_else(|| anyhow::anyhow!("{}", tr("poste introuvable", "seat not found")))?;
        let parent = c.seats[i].reports_to.clone();
        if parent.is_none() && c.seats.len() > 1 {
            anyhow::bail!("{}", tr("le boss ne peut pas être supprimé tant qu'il a des subordonnés", "the boss cannot be deleted while it has subordinates"));
        }
        for s in c.seats.iter_mut().filter(|s| s.reports_to.as_deref() == Some(id)) {
            s.reports_to = parent.clone();
        }
        c.seats.remove(i);
        c.save(path)?;
        self.reload();
        let rows = self.companies.get(self.comp_sel).and_then(|e| e.parsed.as_ref().ok()).map(|c| c.tree_rows().len()).unwrap_or(0);
        self.seat_sel = self.seat_sel.min(rows.saturating_sub(1));
        Ok(())
    }

    /// Postes (« entreprise · poste ») qui utilisent l'agent `name`, d'après les entreprises lisibles.
    /// Reporte un renommage d'agent dans les postes des entreprises : (postes mis à jour, échecs).
    fn rewire_agent(&mut self, old: &str, new: &str) -> (Vec<String>, Vec<String>) {
        let (mut done, mut failed) = (vec![], vec![]);
        for e in &self.companies {
            let Ok(c) = &e.parsed else { continue };
            if !c.seats.iter().any(|s| s.agent == old) {
                continue;
            }
            let mut c = c.clone();
            let file = e.file.trim_end_matches(".toml").to_string();
            let ids: Vec<String> = c.seats.iter().filter(|s| s.agent == old).map(|s| format!("{file} · {}", s.id)).collect();
            for s in c.seats.iter_mut().filter(|s| s.agent == old) {
                s.agent = new.to_string();
            }
            match c.save(&e.path) {
                Ok(()) => done.extend(ids),
                Err(_) => failed.extend(ids),
            }
        }
        self.reload();
        (done, failed)
    }

    pub fn seats_using_agent(&self, name: &str) -> Vec<String> {
        self.companies
            .iter()
            .filter_map(|e| e.parsed.as_ref().ok().map(|c| (e.file.trim_end_matches(".toml"), c)))
            .flat_map(|(file, c)| c.seats.iter().filter(move |s| s.agent == name).map(move |s| format!("{file} · {}", s.id)))
            .collect()
    }

    /// Exécute l'action confirmée. Chaque action rafraîchit l'état affiché et annonce son résultat.
    fn run_action(&mut self, a: PendingAction) {
        match a {
            PendingAction::DeleteAgent(name) => {
                match agent::delete(&self.root, &name) {
                    Ok(true) => self.status = tf!("agent « {name} » supprimé", "agent \"{name}\" deleted"),
                    Ok(false) => self.status = tf!("« {name} » vient du dossier utilisateur (~/.claude/agents) : supprimez-le à la main", "\"{name}\" comes from the user folder (~/.claude/agents): delete it by hand"),
                    Err(e) => self.status = erreur(&e),
                }
                self.reload();
            }
            PendingAction::DeleteSeat(path, id, title) => {
                match self.delete_seat(&path, &id) {
                    Ok(()) => self.status = tf!("poste « {title} » supprimé", "seat \"{title}\" deleted"),
                    Err(e) => self.status = erreur(&e),
                }
            }
            PendingAction::DeleteHistory(id, _) => {
                match store::delete(&self.root, &id) {
                    Ok(()) => self.status = tr("run supprimé de l'historique", "run removed from history").into(),
                    Err(e) => self.status = erreur(&e),
                }
                self.reload();
            }
            PendingAction::CancelRun(_) => {
                if let Some(r) = &self.run {
                    let _ = r.cancel.send(true);
                    self.status = tr("annulation demandée…", "cancellation requested…").into();
                }
            }
            PendingAction::QuitRun(_) => self.quit_now(),
            PendingAction::StartMission(_, resume) => self.launch_mission(resume),
            PendingAction::FinishTerm => self.finish_term(),
            PendingAction::RemoveSession(id, name) => {
                self.status = tf!("suppression de « {name} »…", "deleting \"{name}\"…");
                self.session_action(id, tr("supprimée", "deleted"), true);
            }
            PendingAction::StopSession(id, name) => {
                self.status = tf!("arrêt de « {name} »…", "stopping \"{name}\"…");
                self.session_action(id, tr("arrêtée", "stopped"), false);
            }
        }
    }

    /// Entreprise sélectionnée si elle est lisible et valide ; sinon explique pourquoi dans la barre d'état.
    fn usable_company(&mut self) -> Option<Company> {
        let Some(entry) = self.companies.get(self.comp_sel) else {
            self.status = tr("aucune entreprise : lancez `essaim init` ou créez-en une", "no company: run `essaim init` or create one").into();
            return None;
        };
        match (&entry.parsed, &entry.problem) {
            (Err(e), _) => self.status = tf!("entreprise illisible : {e}", "unreadable company: {e}"),
            (_, Some(p)) => self.status = tf!("entreprise invalide : {p}", "invalid company: {p}"),
            (Ok(c), None) => return Some(c.clone()),
        }
        None
    }

    /// `m` : ouvre le boss en conversation (mode plan) pour cadrer une mission et écrire son brief.
    fn request_mission_brief(&mut self) {
        let Some(company) = self.usable_company() else { return };
        let Some(boss) = company.root() else { return };
        let seat = &company.seats[boss];
        let prompt = self.agents.iter().find(|a| a.name == seat.agent).map(|a| a.prompt.clone()).unwrap_or_default();
        let id = missions::new_id(store::now_ms());
        let dir = match missions::create(&self.root, &id) {
            Ok(d) => d,
            Err(e) => {
                self.status = erreur(&e);
                return;
            }
        };
        self.pending_mission_dir = Some(dir);
        let brief_path = format!(".essaim/missions/{id}/brief.md");
        let has_builders = company.seats.iter().any(|s| s.builds);
        let args = super::assistant::mission_brief_args(self.lang, &company.name, &seat.title, &prompt, &company.outline(), has_builders, &brief_path);
        self.assistant_request = Some((AssistantKind::Mission, args));
    }

    /// `r` / `R` sur l'onglet Entreprise : lance la mission sélectionnée avec l'entreprise sélectionnée.
    /// `resume` : une mission dont la conception est terminée repart de son plan au lieu de tout rejouer.
    /// Un lancement qui écraserait conception ou rapport existants demande d'abord confirmation.
    fn start_mission(&mut self, resume: bool) {
        if self.running() {
            self.status = tr("un run est déjà en cours", "a run is already in progress").into();
            return;
        }
        let selected = self.visible_missions().contains(&self.mission_sel).then(|| self.missions.get(self.mission_sel)).flatten();
        if let Some(m) = selected {
            if (m.has_conception || m.has_report) && !(resume && m.resumable) {
                self.mode = Mode::ConfirmAction(PendingAction::StartMission(m.title.clone(), resume));
                return;
            }
        }
        self.launch_mission(resume);
    }

    /// Lance la mission sélectionnée, sans confirmation (voir `start_mission`).
    fn launch_mission(&mut self, resume: bool) {
        if self.running() {
            self.status = tr("un run est déjà en cours", "a run is already in progress").into();
            return;
        }
        let Some(company) = self.usable_company() else { return };
        let Some(mission) = self.visible_missions().contains(&self.mission_sel).then(|| self.missions.get(self.mission_sel).cloned()).flatten() else {
            self.status = tr("aucune mission : m pour en cadrer une avec le boss", "no mission: m to frame one with the boss").into();
            return;
        };
        let brief = match missions::read_brief(&mission) {
            Ok(b) => b,
            Err(e) => {
                self.status = tf!("brief illisible : {e:#}", "unreadable brief: {e:#}");
                return;
            }
        };
        if let Some(file) = self.companies.get(self.comp_sel).map(|c| c.file.clone()) {
            let _ = missions::set_company(&mission.dir, &file);
            self.missions = missions::list(&self.root);
        }
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.run = Some(RunView {
            name: format!("{} · {}", company.name, mission.title),
            nodes: vec![],
            sel: 0,
            tokens: 0,
            finished: None,
            cancel: cancel_tx,
        });
        self.tab = Tab::Run;
        self.term_focus = false;
        self.show_history = false;
        self.run_started_ms = store::now_ms();
        self.run_id = Some(store::new_id(self.run_started_ms));
        self.persist_run();

        let (rtx, mut rrx) = mpsc::unbounded_channel();
        let app_tx = self.tx.clone();
        tokio::spawn(async move {
            while let Some(e) = rrx.recv().await {
                let _ = app_tx.send(AppEvent::Run(e));
            }
        });
        self.run_ctx = Some((company.clone(), mission.dir.clone()));
        let agents = self.agents.clone();
        let opts = CompanyOptions { base_dir: self.root.clone(), mission_dir: Some(mission.dir.clone()), lang: self.lang, resume: resume && mission.resumable };
        if resume && mission.resumable {
            self.status = tr("reprise depuis le plan validé (R : tout recommencer)", "resuming from the validated plan (R: start over)").into();
        }
        let (fail_tx, notice_tx) = (rtx.clone(), self.tx.clone());
        let dir = mission.dir.display().to_string();
        let lang = self.lang;
        tokio::spawn(async move {
            let result = run_mission(&company, &agents, &brief, &opts, claude_runner(), rtx, cancel_rx).await;
            settings::set_current(lang);
            match result {
                Ok(out) => {
                    let verdict = match (out.ok, out.approved) {
                        (_, Some(false)) => tr("arrêtée sur le plan (réalisation refusée)", "stopped at the plan (realisation refused)"),
                        (true, _) => tr("terminée", "completed"),
                        (false, _) if !out.conception.ok || out.realisation.as_ref().is_some_and(|r| !r.blockers.is_empty()) => {
                            tr("bloquée : blocages non levés (voir le compte rendu)", "blocked: blockers not cleared (see the report)")
                        }
                        (false, _) => tr("terminée avec des échecs", "completed with failures"),
                    };
                    let _ = notice_tx.send(AppEvent::Notice(tf!("mission {verdict} — livrables dans {dir}", "mission {verdict} — deliverables in {dir}")));
                }
                Err(e) => {
                    let _ = fail_tx.send(RunEvent::RunFinished { ok: false, tokens: 0 });
                    let _ = notice_tx.send(AppEvent::Notice(tf!("mission impossible : {e:#}", "mission impossible: {e:#}")));
                }
            }
        });
    }

    pub fn on_run_event(&mut self, ev: RunEvent) {
        if let RunEvent::Checkpoint { plan, reply } = ev {
            self.checkpoint = Some(CheckpointState { plan, reply, scroll: 0 });
            self.status = tr("plan consolidé prêt : Entrée lance la réalisation, Échap l'arrête", "consolidated plan ready: Enter starts realisation, Esc stops it").into();
            self.set_tab(Tab::Run);
            return;
        }
        if let RunEvent::Escalation { summary, reply } = ev {
            self.escalation = Some(EscalationState { summary, reply, scroll: 0 });
            self.status = tr("mission bloquée : e discute avec le CEO, Entrée relance la correction, Échap arrête", "mission blocked: e talks with the CEO, Enter retries the correction, Esc stops").into();
            self.set_tab(Tab::Run);
            return;
        }
        if matches!(ev, RunEvent::RunFinished { .. }) {
            self.escalation = None;
            self.checkpoint = None; // run terminé ou annulé pendant l'attente
            self.write_compte_rendu(&ev);
            self.missions = missions::list(&self.root); // conception.md / rapport.md viennent d'apparaître
        }
        let Some(run) = &mut self.run else { return };
        let node = |run: &mut RunView, id: &str| run.nodes.iter().position(|n| n.id == id);
        match ev {
            RunEvent::Label { id, label, agent, depth } => {
                // Nœud créé dynamiquement (run d'entreprise) : on l'ajoute à la volée.
                match node(run, &id) {
                    Some(i) => {
                        run.nodes[i].label = label;
                        run.nodes[i].depth = depth;
                    }
                    None => {
                        let mut n = NodeView::new(&id, &agent);
                        n.label = label;
                        n.depth = depth;
                        run.nodes.push(n);
                    }
                }
            }
            RunEvent::Checkpoint { .. } | RunEvent::Escalation { .. } => {} // traités avant le match
            RunEvent::NodeStarted { id } => {
                if let Some(i) = node(run, &id) {
                    run.nodes[i].state = NodeState::Running;
                }
            }
            RunEvent::Node { id, event } => {
                let Some(i) = node(run, &id) else { return };
                let log = &mut run.nodes[i].log;
                match event {
                    ClaudeEvent::Text(t) => log.extend(t.lines().map(String::from)),
                    ClaudeEvent::ToolUse { name, .. } => log.push(tf!("⚙ outil : {name}", "⚙ tool: {name}")),
                    ClaudeEvent::ToolResult { is_error: true } => log.push(tr("  ↳ erreur de l'outil", "  ↳ tool error").into()),
                    ClaudeEvent::Result { is_error: true, text, .. } => log.push(format!("✖ {text}")),
                    _ => {}
                }
                if log.len() > MAX_LOG {
                    log.drain(..log.len() - MAX_LOG);
                }
            }
            RunEvent::NodeFinished { id, status, tokens } => {
                if let Some(i) = node(run, &id) {
                    let n = &mut run.nodes[i];
                    if let NodeStatus::Failed(m) | NodeStatus::Blocked(m) | NodeStatus::Skipped(m) = &status {
                        n.log.push(format!("— {m}"));
                    }
                    n.tokens = tokens;
                    n.state = NodeState::Done(status);
                }
            }
            RunEvent::RunFinished { ok, tokens } => {
                run.tokens = tokens;
                run.finished = Some(ok);
            }
        }
        if self.run.as_ref().is_some_and(|r| r.finished.is_some()) {
            self.persist_run();
        }
    }

    /// Écrit `compte-rendu.md` dans la mission quand son run se termine (rien à écrire pour un pipeline).
    fn write_compte_rendu(&mut self, ev: &RunEvent) {
        let RunEvent::RunFinished { ok, tokens } = ev else { return };
        let (Some(run), Some((company, dir))) = (&self.run, &self.run_ctx) else { return };
        let read = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap_or_default();
        let realised = run.nodes.iter().any(|n| n.id.contains("/realisation/"));
        let steps: Vec<_> = run
            .nodes
            .iter()
            .filter(|n| matches!(n.state, NodeState::Done(_)))
            .map(|n| missions::StepLine {
                label: n.label.clone(),
                agent: n.agent.clone(),
                status: match &n.state {
                    NodeState::Done(NodeStatus::Succeeded) => "✔".into(),
                    NodeState::Done(NodeStatus::Failed(_)) => "✖".into(),
                    NodeState::Done(NodeStatus::Blocked(_)) => "⚠".into(),
                    NodeState::Done(NodeStatus::Skipped(_)) => "⊘".into(),
                    _ => "—".into(),
                },
                tokens: n.tokens,
            })
            .collect();
        let issues = run
            .nodes
            .iter()
            .filter_map(|n| match &n.state {
                NodeState::Done(NodeStatus::Failed(m) | NodeStatus::Blocked(m) | NodeStatus::Skipped(m)) => Some((n.label.clone(), m.clone())),
                _ => None,
            })
            .collect();
        let blocked = run.nodes.iter().any(|n| matches!(n.state, NodeState::Done(NodeStatus::Blocked(_))));
        let final_report = if realised { read("rapport.md") } else { read("conception.md") };
        let title = run.name.split(" · ").nth(1).unwrap_or(&run.name).to_string();
        let date = RunRecord { id: String::new(), pipeline: String::new(), started_ms: self.run_started_ms, finished: None, tokens: 0, nodes: vec![] }.started_label();
        let md = missions::compte_rendu(
            &missions::ReportData { title: &title, company: &company.name, date: &date, ok: *ok, blocked, realised, tokens: *tokens, steps, issues, final_report: &final_report },
            self.lang == Lang::Fr,
        );
        let _ = missions::write_output(dir, "compte-rendu.md", &md);
    }

    /// Enregistre l'état du run courant (au démarrage, puis à la fin).
    fn persist_run(&mut self) {
        let (Some(run), Some(id)) = (&self.run, &self.run_id) else { return };
        let rec = RunRecord {
            id: id.clone(),
            pipeline: run.name.clone(),
            started_ms: self.run_started_ms,
            finished: run.finished,
            tokens: if run.finished.is_some() { run.tokens } else { run.nodes.iter().map(|n| n.tokens).sum() },
            nodes: run
                .nodes
                .iter()
                .map(|n| NodeRecord {
                    id: n.id.clone(),
                    agent: n.agent.clone(),
                    label: n.label.clone(),
                    depth: n.depth,
                    status: match &n.state {
                        NodeState::Done(s) => Some(s.clone()),
                        _ => None,
                    },
                    tokens: n.tokens,
                    log: n.log.clone(),
                })
                .collect(),
        };
        if let Err(e) = store::save(&self.root, &rec) {
            self.status = tf!("historique non enregistré : {e:#}", "history not saved: {e:#}");
            return;
        }
        self.history = store::load_all(&self.root);
    }

    /// Recharge un run passé dans la vue Run (lecture seule).
    fn open_history(&mut self) {
        if self.running() {
            self.status = tr("un run est en cours : attendez sa fin", "a run is in progress: wait for it to finish").into();
            return;
        }
        let Some(h) = self.history.get(self.hist_sel).cloned() else { return };
        let (cancel, _) = watch::channel(false);
        self.run = Some(RunView {
            name: h.pipeline.clone(),
            nodes: h
                .nodes
                .iter()
                .map(|n| NodeView {
                    id: n.id.clone(),
                    agent: n.agent.clone(),
                    label: if n.label.is_empty() { n.id.clone() } else { n.label.clone() },
                    depth: n.depth,
                    // Un nœud sans statut dans un run terminé a été interrompu.
                    state: NodeState::Done(n.status.clone().unwrap_or(NodeStatus::Cancelled)),
                    log: n.log.clone(),
                    tokens: n.tokens,
                })
                .collect(),
            sel: 0,
            tokens: h.tokens,
            finished: Some(h.finished.unwrap_or(false)),
            cancel,
        });
        self.run_id = None; // lecture seule : ne pas réécrire l'historique
        self.show_history = false;
    }
}

/// Message d'erreur affiché dans la barre d'état, selon la langue courante.
pub fn erreur(e: &anyhow::Error) -> String {
    tf!("erreur : {e:#}", "error: {e:#}")
}

fn step(i: usize, len: usize, down: bool) -> usize {
    if len == 0 {
        0
    } else if down {
        (i + 1).min(len - 1)
    } else {
        i.saturating_sub(1)
    }
}
