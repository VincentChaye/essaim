//! Exécution d'une mission par une entreprise d'agents.
//!
//! 1. **Plan** : le boss écrit seul le plan (il lit le projet, le web, et pose ses questions à l'humain).
//! 2. **Point de contrôle** : l'humain valide le plan.
//! 3. **Réalisation** : une seule descente. Chaque supérieur découpe sa tâche et confie à chaque subordonné
//!    sa part, avec des pointeurs vers le contexte utile ; les feuilles travaillent ; les rapports remontent.
//! 4. **Revue** : le boss vérifie le résultat. Un écart est présenté à l'humain, qui décide.
//!
//! Tous les postes ont tous les outils. Un poste qui n'aboutit pas ne déclenche aucune boucle de correction :
//! il s'adresse à l'humain (en cours de tâche par l'outil `ask_human`, ou par essaim s'il finit bloqué).
//! Chaque étape est un appel `claude -p` indépendant ; essaim orchestre les niveaux.

use crate::tui::settings::Lang;
use std::path::PathBuf;

mod mission;
mod prompt;
mod schema;
mod seat;
mod shared;

// Chemins publics : `main.rs` et `tui/app.rs` importent ces trois noms depuis `runtime::company_exec`.
pub use mission::{note_refus, run_mission};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Conception,
    Realisation,
}

impl Phase {
    fn key(self) -> &'static str {
        match self {
            Phase::Conception => "conception",
            Phase::Realisation => "realisation",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Phase::Conception => "plan",
            Phase::Realisation => "realisation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Delegate,
    Work,
    Report,
    /// Revue du résultat par le boss : vérification contre l'objectif. Elle constate, l'humain décide.
    Review,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Delegate => "delegate",
            Kind::Work => "work",
            Kind::Report => "report",
            Kind::Review => "review",
        }
    }
}

#[derive(Clone)]
pub struct CompanyOptions {
    /// Dossier du projet : répertoire de travail de tous les appels.
    pub base_dir: PathBuf,
    /// Dossier de la mission : reçoit `conception.md` et `rapport.md`.
    pub mission_dir: Option<PathBuf>,
    /// Langue choisie par l'utilisateur : annoncée aux modèles (les consignes restent en anglais).
    pub lang: Lang,
    /// Reprend une mission interrompue : une conception déjà menée à terme n'est pas rejouée, on repart de son plan.
    pub resume: bool,
    /// Pause du run, partagée avec l'interface : la limite de l'abonnement la ferme, la reprise l'ouvre.
    pub pause: std::sync::Arc<crate::runtime::pause::Gate>,
    /// Droits des agents : profil de sécurité et ouvertures du bac à sable.
    pub access: crate::tui::settings::Access,
    /// Bus des demandes à l'humain (serveur MCP d'essaim) ; absent = les agents n'ont pas l'outil `ask_human`.
    pub bus: Option<std::sync::Arc<crate::runtime::bus::Bus>>,
}

#[cfg(test)]
impl CompanyOptions {
    /// Options d'un run de test : profil dédié, aucun bus (les agents n'ont pas l'outil de demande à l'humain).
    pub fn plain(base_dir: PathBuf, mission_dir: Option<PathBuf>, lang: Lang) -> Self {
        CompanyOptions { base_dir, mission_dir, lang, resume: false, pause: crate::runtime::pause::Gate::new(), access: crate::tui::settings::Access::dedicated(), bus: None }
    }
}

#[derive(Debug, Clone)]
pub struct PhaseOutcome {
    /// Aucun nœud n'a échoué dans toute la phase.
    pub ok: bool,
    /// Rapport du boss (plan consolidé en conception).
    pub report: String,
    /// Blocages restés sans solution à la fin de la phase (vide si `ok`).
    pub blockers: Vec<Blocker>,
    pub tokens: u64,
    pub calls: u32,
}

/// Résultat d'une mission complète.
#[derive(Debug, Clone)]
pub struct MissionOutcome {
    pub ok: bool,
    /// Rapport final du boss (celui de la réalisation si elle a eu lieu, sinon la conception).
    pub report: String,
    pub conception: PhaseOutcome,
    pub realisation: Option<PhaseOutcome>,
    /// `Some(false)` : la réalisation a été refusée au point de contrôle.
    pub approved: Option<bool>,
    pub tokens: u64,
    pub calls: u32,
}

/// Section du gabarit de réponse imposé par le délégant : un champ du schéma de l'employé, de taille bornée.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    /// Clé du champ dans la sortie structurée (alphanumérique, unique dans le gabarit).
    pub key: String,
    pub title: String,
    /// Ce que la section doit contenir.
    pub expect: String,
    pub max_chars: usize,
}

pub const MAX_SECTIONS: usize = 6;
pub const MIN_SECTION_CHARS: usize = 60;
pub const MAX_SECTION_CHARS: usize = 1000;

impl Section {
    /// Gabarit nettoyé : au plus `MAX_SECTIONS` sections, tailles bornées, clés uniques. Entrées vides ignorées.
    pub fn template(raw: impl IntoIterator<Item = (String, String, Option<u64>)>) -> Vec<Section> {
        let mut out: Vec<Section> = vec![];
        for (title, expect, max) in raw {
            let title: String = title.trim().chars().take(60).collect();
            if title.is_empty() || out.len() >= MAX_SECTIONS {
                continue;
            }
            let mut key: String = title.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).take(40).collect();
            if key.trim_matches('_').is_empty() {
                key = format!("section_{}", out.len() + 1);
            }
            while out.iter().any(|s| s.key == key) {
                key.push('_');
            }
            let max_chars = (max.unwrap_or(MAX_SECTION_CHARS as u64) as usize).clamp(MIN_SECTION_CHARS, MAX_SECTION_CHARS);
            out.push(Section { key, title, expect: expect.trim().chars().take(200).collect(), max_chars });
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub to: usize,
    /// Vague du travail chez le délégant (1 = la première). Toutes les affectations d'une vague sont
    /// terminées avant que la suivante démarre : c'est ainsi qu'un poste construit sur le résultat d'un
    /// autre (une spécification, puis la réalisation, puis une relecture) dans une seule délégation.
    pub stage: u32,
    /// Gabarit de réponse voulu par le délégant (vide = rapport libre).
    pub sections: Vec<Section>,
    /// Les étapes confiées à ce poste, dans l'ordre, telles que le délégant les a écrites. Une feuille
    /// les exécute **l'une après l'autre, chacune dans une session neuve** : un contexte qui ne contient
    /// que l'étape en cours coûte bien moins qu'une session unique qui accumule tout (le coût d'une
    /// session vaut tours × contexte moyen, donc il croît quadratiquement avec sa longueur).
    /// Toujours au moins une entrée ; `task` en est la concaténation, pour les rapports et l'historique.
    pub steps: Vec<String>,
    pub task: String,
    /// Périmètre de fichiers (phase de réalisation uniquement).
    pub scope: Vec<String>,
    /// Modèle choisi par le délégant pour ce poste (feuille) ou par défaut pour les feuilles de sa branche.
    pub model: Option<String>,
}

/// Ce qui empêche une tâche d'aboutir : présenté à l'humain, et rappelé dans le rapport du supérieur.
#[derive(Debug, Clone, PartialEq)]
pub struct Blocker {
    /// tests | permission | prerequisite | bug | scope | other
    pub kind: String,
    pub detail: String,
    /// Ce qu'il faudrait pour lever le blocage.
    pub needs: String,
}

impl Blocker {
    pub fn line(&self) -> String {
        let needs = if self.needs.trim().is_empty() { String::new() } else { format!(" — needs: {}", self.needs.trim()) };
        format!("[{}] {}{needs}", self.kind, self.detail.trim())
    }
}

/// Résultat réel d'un poste : une tâche n'est « faite » que si l'agent l'affirme, sans refus ni blocage restant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Done,
    Blocked,
    Failed,
}

#[derive(Debug, Clone)]
struct Outcome {
    status: Status,
    report: String,
    blockers: Vec<Blocker>,
    /// Session `claude` de l'appel : c'est elle qu'on reprend quand l'humain répond à un poste bloqué.
    session: Option<String>,
}

impl Outcome {
    fn failed(msg: impl Into<String>) -> Self {
        let msg = msg.into();
        let detail = msg.lines().next().unwrap_or("").trim().to_string();
        Outcome { status: Status::Failed, blockers: vec![Blocker { kind: "other".into(), detail, needs: String::new() }], report: msg, session: None }
    }
}

struct SeatOut {
    status: Status,
    text: String,
    blockers: Vec<Blocker>,
    /// Le poste n'a pas fini mais l'humain a dit de continuer sans lui : les travaux suivants ne sont pas arrêtés.
    waived: bool,
}

impl SeatOut {
    fn ok(&self) -> bool {
        self.status == Status::Done
    }
    fn failed(msg: impl Into<String>) -> Self {
        Outcome::failed(msg).into()
    }
    /// Texte remonté au supérieur : le rapport, puis les blocages s'il y en a.
    fn for_supervisor(&self) -> String {
        if self.ok() || self.blockers.is_empty() {
            return self.text.clone();
        }
        let list: Vec<String> = self.blockers.iter().map(|b| format!("- {}", b.line())).collect();
        format!("{}\nBlockers:\n{}", self.text, list.join("\n"))
    }
}

impl From<Outcome> for SeatOut {
    fn from(o: Outcome) -> Self {
        SeatOut { status: o.status, text: o.report, blockers: o.blockers, waived: false }
    }
}

/// Résultat d'un poste qui a enchaîné plusieurs étapes : les rapports sont concaténés, étape par étape,
/// et le statut est celui de la dernière étape exécutée (une étape non aboutie arrête les suivantes).
fn join_steps(parts: Vec<(usize, SeatOut)>) -> SeatOut {
    let n = parts.len();
    let last = parts.last().map(|(_, o)| o.status).unwrap_or(Status::Failed);
    let waived = parts.last().is_some_and(|(_, o)| o.waived);
    let mut text = String::new();
    let mut blockers = vec![];
    for (i, o) in parts {
        if n > 1 {
            text.push_str(&format!("### Step {i}
"));
        }
        text.push_str(o.text.trim());
        text.push('\n');
        blockers.extend(o.blockers);
    }
    SeatOut { status: last, text: text.trim_end().to_string(), blockers, waived }
}

// ---------------------------------------------------------------- moteur

#[cfg(test)]
mod tests {
    use super::prompt::*;
    use super::schema::*;
    use super::*;
    use crate::model::agent::AgentDef;
    use crate::model::company::Company;
    use crate::runtime::claude_proc::RunSpec;
    use crate::runtime::executor::{AskKind, HumanAnswer, HumanRequest, NodeResult, NodeStatus, RunEvent, Runner};
    use crate::tui::settings::{Access, Settings};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::{mpsc, watch};

    const SRC: &str = r#"
name = "acme"
max_parallel = 4
checkpoint = false

[[seat]]
id = "ceo"
title = "CEO"
agent = "a"
[[seat]]
id = "cto"
title = "CTO"
agent = "a"
reports_to = "ceo"
expertise = "technique"
[[seat]]
id = "ciso"
title = "CISO"
agent = "a"
reports_to = "ceo"
[[seat]]
id = "mgr"
title = "Engineering Manager"
agent = "a"
reports_to = "cto"
[[seat]]
id = "dev1"
title = "Backend Engineer"
agent = "a"
reports_to = "mgr"
builds = true
[[seat]]
id = "dev2"
title = "Frontend Engineer"
agent = "a"
reports_to = "mgr"
builds = true
[[seat]]
id = "sec"
title = "Security Analyst"
agent = "a"
reports_to = "ciso"
"#;

    /// Flux de base de `SRC` : le plan (1), délégation et rapport des 4 postes internes (8), les 3 feuilles, la revue (1).
    const BASE_CALLS: u32 = 13;

    fn agents() -> Vec<AgentDef> {
        vec![AgentDef { name: "a".into(), description: "d".into(), prompt: "p".into(), ..Default::default() }]
    }

    #[derive(Default)]
    struct Probe {
        started: Mutex<Vec<String>>,
        finished: Mutex<Vec<String>>,
        prompts: Mutex<Vec<(String, String, String)>>, // (id, prompt, system)
        specs: Mutex<Vec<(String, RunSpec)>>,
        /// Chronologie unique : « S:id » au démarrage, « E:id » à la fin.
        timeline: Mutex<Vec<String>>,
        now: AtomicUsize,
        peak: AtomicUsize,
        /// Passages par identifiant d'appel.
        seen: Mutex<std::collections::HashMap<String, usize>>,
        /// Appels déjà refusés une fois pour limite d'abonnement (voir `Behavior::limit_once`).
        limited: Mutex<Vec<String>>,
        /// Appels déjà arrêtés une fois par la pause (voir `Behavior::interrupt_once`).
        interrupted: Mutex<Vec<String>>,
    }

    impl Probe {
        fn started(&self) -> Vec<String> {
            self.started.lock().unwrap().clone()
        }
        fn prompt(&self, id: &str) -> String {
            self.prompts.lock().unwrap().iter().find(|(i, _, _)| i == id).unwrap_or_else(|| panic!("appel jamais lancé : {id}")).1.clone()
        }
        fn system(&self, id: &str) -> String {
            self.prompts.lock().unwrap().iter().find(|(i, _, _)| i == id).unwrap_or_else(|| panic!("appel jamais lancé : {id}")).2.clone()
        }
        fn spec(&self, id: &str) -> RunSpec {
            self.specs.lock().unwrap().iter().find(|(i, _)| i == id).unwrap_or_else(|| panic!("appel jamais lancé : {id}")).1.clone()
        }
        /// L'appel qui a repris la session d'un poste (réponse de l'humain, ou reprise après pause).
        fn resumed(&self, prefix: &str) -> Option<RunSpec> {
            self.specs.lock().unwrap().iter().find(|(id, s)| id.starts_with(prefix) && s.resume.is_some()).map(|(_, s)| s.clone())
        }
    }

    #[derive(Default, Clone)]
    struct Behavior {
        fail: Vec<&'static str>,
        /// seat → sous-ensemble de subordonnés à qui déléguer (sinon tous).
        only: Vec<(&'static str, Vec<&'static str>)>,
        bad_delegation: Vec<&'static str>,
        /// Ces postes ratent leur première délégation seulement.
        bad_delegation_once: Vec<&'static str>,
        /// Tous les subordonnés reçoivent le périmètre de leur supérieur (chevauchement total).
        overlap: bool,
        /// Ces postes attribuent un périmètre hors du leur.
        escape: Vec<&'static str>,
        /// Ces postes ne donnent aucun périmètre.
        no_scope: Vec<&'static str>,
        /// Ces postes joignent des pointeurs de contexte à leurs affectations.
        context: Vec<&'static str>,
        /// Ces appels se déclarent « blocked » à chaque fois (reprises comprises).
        blocked: Vec<&'static str>,
        /// Ces appels se déclarent « blocked » à leur premier passage ; la reprise de leur session aboutit.
        blocked_once: Vec<&'static str>,
        /// La première revue du boss ne valide pas le résultat (la suivante valide).
        review_unmet_once: bool,
        /// La revue du boss ne valide jamais le résultat.
        review_unmet: bool,
        /// (poste délégant, destinataire, modèle) : le délégant choisit le modèle de ce destinataire.
        models: Vec<(&'static str, &'static str, &'static str)>,
        /// Ces appels butent sur la limite de l'abonnement à leur premier passage, puis se comportent normalement.
        limit_once: Vec<&'static str>,
        /// Ces appels ne rendent jamais de sortie structurée : seulement du texte libre (outil `StructuredOutput` en échec).
        no_structured: Vec<&'static str>,
        /// Ces appels sont arrêtés par la pause à leur premier passage (bilan rendu, session connue),
        /// puis se comportent normalement à la reprise.
        interrupt_once: Vec<&'static str>,
        /// Ces postes confient DEUX étapes successives à chacun de leurs subordonnés mobilisés.
        two_steps: Vec<&'static str>,
        /// (poste délégant, destinataire, vague) : le délégant place ce destinataire dans cette vague (sinon la première).
        stages: Vec<(&'static str, &'static str, u32)>,
        /// (poste délégant, destinataire, vague) : une affectation de plus pour ce destinataire, dans cette vague.
        again: Vec<(&'static str, &'static str, u32)>,
    }

    /// Subordonnés directs du poste, lus dans sa fiche (« - id · titre… ») : le schéma de délégation est le
    /// même pour tous les postes, il ne les nomme pas.
    fn kid_ids(system: &str) -> Vec<String> {
        let Some(rest) = system.split("Your direct subordinates (you delegate only to them):\n").nth(1) else { return vec![] };
        rest.lines().map_while(|l| l.strip_prefix("- ")).filter_map(|l| l.split(" · ").next()).map(String::from).collect()
    }

    /// Périmètre du délégant, lu dans son prompt (« Your file scope: `a`, `b`. »).
    fn parent_scope(prompt: &str) -> Vec<String> {
        let Some(rest) = prompt.split("Your file scope: `").nth(1) else { return vec![] };
        let list = rest.split("`. ").next().unwrap_or("");
        list.split("`, `").map(String::from).collect()
    }

    /// Vrai au premier passage de `id` dans `seen`, faux ensuite.
    fn first_time(seen: &Mutex<Vec<String>>, id: &str) -> bool {
        let mut done = seen.lock().unwrap();
        let first = !done.iter().any(|x| x == id);
        if first {
            done.push(id.to_string());
        }
        first
    }

    fn fake(p: Arc<Probe>, b: Behavior) -> Runner {
        Arc::new(move |job, _tx, _c, _p| {
            let (p, b) = (p.clone(), b.clone());
            Box::pin(async move {
                p.started.lock().unwrap().push(job.id.clone());
                p.timeline.lock().unwrap().push(format!("S:{}", job.id));
                let sys = job.spec.append_system_prompt.clone().unwrap_or_default();
                p.prompts.lock().unwrap().push((job.id.clone(), job.spec.prompt.clone(), sys));
                p.specs.lock().unwrap().push((job.id.clone(), job.spec.clone()));
                let n = p.now.fetch_add(1, Ordering::SeqCst) + 1;
                p.peak.fetch_max(n, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(15)).await;
                p.now.fetch_sub(1, Ordering::SeqCst);
                p.finished.lock().unwrap().push(job.id.clone());
                p.timeline.lock().unwrap().push(format!("E:{}", job.id));
                // Identifiant : « poste/phase/type », avec un suffixe d'étape (« work@2 ») et de reprise (« work#7 »).
                let parts: Vec<&str> = job.id.split('/').collect();
                let (seat, brut) = (parts[0].to_string(), parts[2]);
                let resumed = job.spec.resume.is_some();
                let kind = brut.split(['@', '#']).next().unwrap_or(brut).to_string();
                // Identifiant sans le suffixe de reprise ou de rappel : c'est lui que les scénarios désignent.
                let base = job.id.split('#').next().unwrap_or(&job.id).to_string();
                let session = Some(format!("sid-{seat}"));
                if b.limit_once.contains(&base.as_str()) && first_time(&p.limited, &base) {
                    // Ce que renvoie `run_local` quand `claude` refuse l'appel faute de quota.
                    return Ok(NodeResult {
                        output: "Claude AI usage limit reached|1761580800".into(),
                        is_error: true,
                        limit: Some(crate::runtime::claude_proc::UsageLimit { resets_at: Some(crate::runtime::pause::now_s() + 1) }),
                        ..Default::default()
                    });
                }
                if b.interrupt_once.contains(&base.as_str()) && first_time(&p.interrupted, &base) {
                    // Ce que rend `run_local` quand l'agent s'arrête sur le prompt prioritaire de la pause.
                    return Ok(NodeResult {
                        output: "Fait : la moitié. Reste : l'autre.".into(),
                        tokens: 100,
                        session_id: Some("sid-pause".into()),
                        interrupted: Some(crate::runtime::executor::Interrupted { session: Some("sid-pause".into()), note: "Fait : la moitié. Reste : l'autre.".into(), killed: false }),
                        ..Default::default()
                    });
                }
                if b.no_structured.contains(&base.as_str()) {
                    return Ok(NodeResult { output: "analyse en texte libre : trois failles XSS".into(), session_id: session, ..Default::default() });
                }
                if b.fail.contains(&base.as_str()) {
                    return Ok(NodeResult { output: "boum".into(), is_error: true, session_id: session, ..Default::default() });
                }
                let passage = {
                    let mut seen = p.seen.lock().unwrap();
                    let c = seen.entry(base.clone()).or_insert(0);
                    *c += 1;
                    *c
                };
                Ok(match kind.as_str() {
                    "delegate" if b.bad_delegation.contains(&seat.as_str()) || (b.bad_delegation_once.contains(&seat.as_str()) && passage <= 2) => NodeResult {
                        structured: Some(json!({"assignments": [{"to": "inconnu", "task": "x", "scope": ["src/x/**"]}]})),
                        output: "{}".into(),
                        ..Default::default()
                    },
                    "delegate" => {
                        let ids = kid_ids(job.spec.append_system_prompt.as_deref().unwrap_or_default());
                        let parent = parent_scope(&job.spec.prompt);
                        let dir = parent.first().map(|s| s.trim_end_matches("/**").to_string()).filter(|s| s != "**").unwrap_or_else(|| "src".into());
                        let picked: Vec<String> = match b.only.iter().find(|(s, _)| *s == seat) {
                            Some((_, only)) => ids.into_iter().filter(|i| only.contains(&i.as_str())).collect(),
                            None => ids,
                        };
                        let items: Vec<Value> = picked
                            .iter()
                            .map(|k| {
                                let mut a = json!({"to": k, "task": format!("task for {k}")});
                                if let Some((_, _, m)) = b.models.iter().find(|(from, to, _)| *from == seat && to == k) {
                                    a["model"] = json!(m);
                                }
                                if let Some((_, _, st)) = b.stages.iter().find(|(from, to, _)| *from == seat && to == k) {
                                    a["stage"] = json!(st);
                                }
                                if b.context.contains(&seat.as_str()) {
                                    a["context"] = json!([format!("src/{k}.rs:10-40"), "https://exemple.fr/doc"]);
                                }
                                if !b.no_scope.contains(&seat.as_str()) {
                                    let sc = if b.escape.contains(&seat.as_str()) {
                                        "docs/**".to_string()
                                    } else if b.overlap {
                                        format!("{dir}/**")
                                    } else {
                                        format!("{dir}/{k}/**")
                                    };
                                    a["scope"] = json!([sc]);
                                }
                                a
                            })
                            .collect();
                        // Deux affectations pour un même subordonné : deux étapes, exécutées en séquence.
                        let items: Vec<Value> = if b.two_steps.contains(&seat.as_str()) {
                            items
                                .into_iter()
                                .flat_map(|a| {
                                    let mut deux = a.clone();
                                    deux["task"] = json!(format!("etape2 pour {}", a["to"].as_str().unwrap_or_default()));
                                    vec![a, deux]
                                })
                                .collect()
                        } else {
                            items
                        };
                        // Le même subordonné rappelé dans une vague ultérieure (une relecture après la réalisation).
                        let mut items = items;
                        for (_, to, st) in b.again.iter().filter(|(from, _, _)| *from == seat) {
                            items.push(json!({"to": to, "task": format!("suite pour {to}"), "stage": st}));
                        }
                        NodeResult { structured: Some(json!({"assignments": items})), output: "{}".into(), session_id: session, ..Default::default() }
                    }
                    "work" | "report" => {
                        let text = match (kind.as_str(), resumed) {
                            ("work", true) => format!("reprise({seat})"),
                            ("work", false) => format!("travail({seat})"),
                            _ => format!("rapport({seat})"),
                        };
                        let blocked = b.blocked.contains(&base.as_str()) || (!resumed && b.blocked_once.contains(&base.as_str()));
                        let structured = if blocked {
                            json!({"status": "blocked", "report": text, "blockers": [{"kind": "tests", "detail": format!("tests rouges chez {seat}"), "needs": "corriger"}]})
                        } else {
                            json!({"status": "done", "report": text, "blockers": []})
                        };
                        NodeResult { structured: Some(structured), output: text, session_id: session, ..Default::default() }
                    }
                    "review" => {
                        let unmet = b.review_unmet || (b.review_unmet_once && passage == 1);
                        let v = if unmet { json!({"accepted": false, "findings": "critère X non atteint"}) } else { json!({"accepted": true, "findings": "ok"}) };
                        NodeResult { structured: Some(v), output: "{}".into(), session_id: session, ..Default::default() }
                    }
                    k => panic!("type d'appel inattendu : {k} ({})", job.id),
                })
            })
        })
    }

    #[derive(Default, Clone)]
    struct Cfg {
        cancel_at_start: bool,
        /// Réponse au point de contrôle ; None = personne ne répond.
        answer: Option<bool>,
        mission_dir: Option<PathBuf>,
        cancel_after_ms: Option<u64>,
        resume: bool,
        /// Réponses successives de l'humain aux demandes ; une fois épuisées, plus personne ne répond.
        humans: Vec<HumanAnswer>,
        access: Option<Access>,
    }

    fn options(cfg: &Cfg) -> CompanyOptions {
        CompanyOptions { resume: cfg.resume, access: cfg.access.clone().unwrap_or_else(Access::dedicated), ..CompanyOptions::plain(std::env::temp_dir(), cfg.mission_dir.clone(), Lang::Fr) }
    }

    async fn run_cfg(c: &Company, p: Arc<Probe>, b: Behavior, cfg: Cfg) -> (MissionOutcome, Vec<RunEvent>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (ctx, crx) = watch::channel(cfg.cancel_at_start);
        let events = Arc::new(Mutex::new(Vec::<RunEvent>::new()));
        let ev2 = events.clone();
        let answer = cfg.answer;
        let mut script = cfg.humans.clone().into_iter();
        let collector = tokio::spawn(async move {
            while let Some(e) = rx.recv().await {
                if let (RunEvent::Checkpoint { reply, .. }, Some(a)) = (&e, answer) {
                    let _ = reply.send(a);
                }
                if let RunEvent::HumanRequest(req) = &e {
                    let _ = req.reply.send(match script.next() {
                        Some(a) => vec![a],
                        None => req.unattended(),
                    });
                }
                ev2.lock().unwrap().push(e);
            }
        });
        if let Some(ms) = cfg.cancel_after_ms {
            let ctx = ctx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                let _ = ctx.send(true);
            });
        }
        let out = run_mission(c, &agents(), "Brief de test", &options(&cfg), fake(p, b), tx, crx).await.unwrap();
        collector.await.unwrap();
        drop(ctx);
        let evs = std::mem::take(&mut *events.lock().unwrap());
        (out, evs)
    }

    async fn run(c: &Company, p: Arc<Probe>, b: Behavior) -> (MissionOutcome, Vec<RunEvent>) {
        run_cfg(c, p, b, Cfg::default()).await
    }

    /// Demandes adressées à l'humain pendant un run.
    fn requests(evs: &[RunEvent]) -> Vec<HumanRequest> {
        evs.iter().filter_map(|e| if let RunEvent::HumanRequest(r) = e { Some(r.clone()) } else { None }).collect()
    }

    /// Organigramme de test, sans point de contrôle.
    fn company() -> Company {
        Company::parse(SRC).unwrap()
    }

    fn company_checkpoint() -> Company {
        let mut c = company();
        c.checkpoint = true;
        c
    }

    fn tmpdir() -> PathBuf {
        std::env::temp_dir().join(format!("essaim-mission-{}", uuid::Uuid::new_v4()))
    }

    // ----------------------------------------------------------- schémas et lectures

    #[test]
    fn gabarit_de_reponse_schema_et_lecture() {
        let secs = Section::template([
            ("Risques".to_string(), "les 3 principaux".to_string(), Some(5000)),
            ("Risques".to_string(), "doublon".to_string(), Some(1)),
            ("".to_string(), "ignorée".to_string(), None),
        ]);
        assert_eq!(secs.len(), 2);
        assert_ne!(secs[0].key, secs[1].key, "clés uniques");
        assert_eq!((secs[0].max_chars, secs[1].max_chars), (MAX_SECTION_CHARS, MIN_SECTION_CHARS), "tailles bornées");
        let v: serde_json::Value = serde_json::from_str(&schema::outcome_schema()).unwrap();
        // Le schéma ne nomme aucune section : il reste le même d'un appel à l'autre (cache de prompt), et le
        // gabarit du délégant est décrit dans le prompt.
        assert!(v["properties"]["sections"]["properties"].is_null(), "aucune section nommée dans le schéma");
        assert_eq!(v["properties"]["sections"]["additionalProperties"]["type"], "string");
        // Aucune borne non plus : elle ferait échouer l'outil `StructuredOutput` au moindre dépassement.
        assert!(!schema::outcome_schema().contains("maxLength"), "taille jamais imposée par le schéma");
        let hint = work_prompt("t", &secs, None, true, false);
        assert!(hint.contains(&format!("`{}`", secs[0].key)) && hint.contains("1000 chars"), "clé et budget annoncés au modèle : {hint}");
        assert_eq!(v["required"], json!(["status"]));
        // Plus de statut « question » : une question se pose à l'humain par l'outil, pendant la tâche.
        assert_eq!(v["properties"]["status"]["enum"], json!(["done", "blocked", "failed"]));
        let r = NodeResult { structured: Some(json!({"status": "done", "sections": {secs[0].key.clone(): "r1"}})), ..Default::default() };
        let o = parse_outcome(&r, &secs).unwrap();
        assert_eq!(o.report, "### Risques\nr1\n\n### Risques\n—", "ordre du gabarit, section vide visible");
        // Dépassement du budget : tronqué à la lecture (la section la plus courte fait 60 caractères).
        let long = "x".repeat(500);
        let r = NodeResult { structured: Some(json!({"status": "done", "sections": {secs[1].key.clone(): long}})), ..Default::default() };
        let coupe = parse_outcome(&r, &secs).unwrap().report;
        assert!(coupe.contains("chars omitted"), "section trop longue tronquée : {coupe}");
        assert!(coupe.chars().count() < 300, "budget tenu après troncature");
        assert!(Section::template((0..10).map(|i| (format!("s{i}"), String::new(), None))).len() <= MAX_SECTIONS);
        // Le schéma n'impose plus la forme : une réponse rendue dans l'autre champ est gardée, jamais perdue.
        let lu = |v: Value, secs: &[Section]| parse_outcome(&NodeResult { structured: Some(v), ..Default::default() }, secs).unwrap().report;
        assert_eq!(lu(json!({"status": "done", "report": "tout d'un bloc"}), &secs), "tout d'un bloc", "sections demandées, réponse dans `report`");
        assert_eq!(lu(json!({"status": "done", "sections": {"a": "x", "b": "y"}}), &[]), "### a\nx\n\n### b\ny", "rapport libre, réponse dans `sections`");
        assert_eq!(lu(json!({"status": "done", "report": "r", "sections": {"a": "x"}}), &[]), "r", "rapport libre : `report` prime");
    }

    #[test]
    fn sortie_sans_statut_jamais_un_succes() {
        let lu = |v: Value| parse_outcome(&NodeResult { structured: Some(v), session_id: Some("s".into()), ..Default::default() }, &[]);
        assert!(lu(json!({"report": "fait"})).is_none(), "statut absent");
        assert!(lu(json!({"status": "termine", "report": "fait"})).is_none(), "statut inconnu");
        let o = lu(json!({"status": "blocked", "report": "le build casse"})).unwrap();
        assert_eq!(o.status, Status::Blocked);
        assert_eq!(o.blockers.len(), 1, "un blocage est toujours nommé, même si l'agent n'en liste pas");
        assert_eq!(o.session.as_deref(), Some("s"), "la session suit le résultat : c'est elle qu'on reprend");
        let o = parse_outcome(&NodeResult { is_error: true, output: "boum\nsuite".into(), ..Default::default() }, &[]).unwrap();
        assert_eq!((o.status, o.blockers[0].detail.as_str()), (Status::Failed, "boum"));
    }

    /// Le schéma est présenté au modèle comme un outil, et les outils ouvrent le préfixe que le cache de prompt
    /// partage entre les appels : un schéma propre à chaque poste ferait réécrire ce préfixe à chaque appel.
    #[test]
    fn schemas_constants_d_un_poste_a_l_autre() {
        let v: Value = serde_json::from_str(&delegation_schema()).unwrap();
        let item = &v["properties"]["assignments"]["items"];
        assert!(item["properties"]["to"]["enum"].is_null(), "aucun identifiant de poste dans le schéma");
        assert_eq!(item["required"], json!(["to", "task"]), "le périmètre, le contexte et la vague sont facultatifs");
        assert!(item["properties"]["context"].is_object() && item["properties"]["scope"].is_object() && item["properties"]["stage"].is_object());
        let r: Value = serde_json::from_str(&review_schema()).unwrap();
        assert_eq!(r["required"], json!(["accepted", "findings"]));
        assert!(r["properties"]["assignments"].is_null(), "la revue constate, elle ne relance rien");
    }

    #[test]
    fn lecture_des_affectations() {
        let c = company();
        let kids = c.children(c.root().unwrap());
        let all = vec!["**".to_string()];
        let v = json!({"assignments": [
            {"to": "cto", "task": " étape 1 ", "context": ["src/api.rs:10-40", " ", "https://exemple.fr/doc"], "scope": ["src/api/**"]},
            {"to": "inconnu", "task": "x"},
            {"to": "ciso", "task": "  "},
            {"to": "cto", "task": "étape 2", "scope": ["src/api/**", "docs/api.md"]},
        ]});
        let a = parse_assignments(&v, &c, &kids, &all).unwrap();
        assert_eq!(a.len(), 1, "destinataire inconnu et tâche vide écartés, doublon regroupé");
        assert_eq!(a[0].steps.len(), 2, "deux affectations au même poste = deux étapes");
        // Les pointeurs voyagent avec l'étape qui les porte : le subordonné lit lui-même ce qu'ils désignent.
        assert!(a[0].steps[0].starts_with("étape 1\n\nContext pointers") && a[0].steps[0].contains("- src/api.rs:10-40\n- https://exemple.fr/doc"), "{}", a[0].steps[0]);
        assert_eq!(a[0].steps[1], "étape 2");
        assert_eq!(a[0].task, "étape 1\n\nétape 2", "la tâche rappelée au supérieur ne porte pas les pointeurs");
        assert_eq!(a[0].scope, vec!["src/api/**", "docs/api.md"]);
        // Le schéma ne restreint plus `to` : un destinataire inconnu est refusé ici, et l'erreur rendue au
        // délégant nomme ceux qui sont permis.
        let err = parse_assignments(&json!({"assignments": [{"to": "x", "task": "y"}]}), &c, &kids, &all).unwrap_err().to_string();
        assert!(err.contains("cto, ciso"), "{err}");
        assert!(parse_assignments(&json!({}), &c, &kids, &all).is_err());
    }

    #[test]
    fn lecture_des_vagues() {
        let c = company();
        let kids = c.children(c.root().unwrap());
        let all = vec!["**".to_string()];
        let v = json!({"assignments": [
            {"to": "cto", "task": "réaliser", "stage": 2},
            {"to": "ciso", "task": "exigences"},
            {"to": "ciso", "task": "exigences, suite", "stage": 1},
            {"to": "ciso", "task": "relire", "stage": 3},
            {"to": "cto", "task": "sans vague lisible", "stage": "bientôt"},
            {"to": "cto", "task": "trop loin", "stage": 900},
        ]});
        let a = parse_assignments(&v, &c, &kids, &all).unwrap();
        let vu: Vec<(&str, u32, usize)> = a.iter().map(|x| (c.seats[x.to].id.as_str(), x.stage, x.steps.len())).collect();
        // Rangées par vague ; même poste et même vague = étapes ; même poste dans deux vagues = deux affectations.
        assert_eq!(vu, vec![("ciso", 1, 2), ("cto", 1, 1), ("cto", 2, 1), ("ciso", 3, 1), ("cto", 20, 1)], "vague absente ou illisible : la première ; vague bornée");
        // Chaque poste sait où se place sa tâche : sans cela il se dit bloqué pour un travail prévu plus tard.
        assert!(a[0].steps.iter().all(|s| s.contains("(Stage 1 of 4")) && a[0].steps[0].contains("do not report \"blocked\""), "{:?}", a[0].steps);
        assert!(a[4].steps[0].contains("(Stage 4 of 4, the last"), "le rang compte les vagues utilisées, pas leur numéro : {}", a[4].steps[0]);
        assert!(!a[0].task.contains("Stage"), "la tâche rappelée au supérieur reste celle qu'il a écrite");
        // Une seule vague : rien n'est ajouté.
        let a = parse_assignments(&json!({"assignments": [{"to": "cto", "task": "t", "stage": 3}]}), &c, &kids, &all).unwrap();
        assert_eq!(a[0].steps, vec!["t"]);
    }

    /// Des vagues successives pour un seul et même subordonné ne sont que des étapes : une seule descente de sa
    /// branche, au lieu d'une délégation et d'un rapport par vague à chaque niveau.
    #[test]
    fn vagues_d_un_seul_subordonne_fusionnees_en_etapes() {
        let c = company();
        let kids = c.children(c.root().unwrap());
        let all = vec!["**".to_string()];
        let lu = |v: Value| -> Vec<(String, u32, usize)> { parse_assignments(&v, &c, &kids, &all).unwrap().iter().map(|x| (c.seats[x.to].id.clone(), x.stage, x.steps.len())).collect() };
        let a = parse_assignments(&json!({"assignments": [
            {"to": "cto", "task": "écrire", "stage": 1, "scope": ["src/**"]},
            {"to": "cto", "task": "tester", "stage": 2, "scope": ["tests/**"]},
            {"to": "cto", "task": "vérifier", "stage": 3},
        ]}), &c, &kids, &all).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].steps, vec!["écrire", "tester", "vérifier"], "dans l'ordre des vagues, sans note de vague : il n'en reste qu'une");
        // Périmètres réunis ; la troisième affectation, sans périmètre, apporte celui du délégant (`**`).
        assert_eq!((a[0].task.as_str(), a[0].scope.clone()), ("écrire\n\ntester\n\nvérifier", vec!["src/**".to_string(), "tests/**".to_string(), "**".to_string()]));
        // Dès qu'un autre poste intervient entre deux, l'ordre entre postes compte : les vagues restent.
        let v = json!({"assignments": [
            {"to": "cto", "task": "a", "stage": 1}, {"to": "cto", "task": "b", "stage": 2},
            {"to": "ciso", "task": "relire", "stage": 3},
            {"to": "cto", "task": "corriger", "stage": 4}, {"to": "cto", "task": "clore", "stage": 5},
        ]});
        assert_eq!(lu(v), vec![("cto".to_string(), 1, 2), ("ciso".to_string(), 3, 1), ("cto".to_string(), 4, 2)]);
        // Une vague partagée avec un autre poste n'est pas fusionnée avec la suivante.
        let v = json!({"assignments": [{"to": "ciso", "task": "a", "stage": 1}, {"to": "cto", "task": "b", "stage": 1}, {"to": "cto", "task": "c", "stage": 2}]});
        assert_eq!(lu(v), vec![("ciso".to_string(), 1, 1), ("cto".to_string(), 1, 1), ("cto".to_string(), 2, 1)]);
    }

    /// Le périmètre ne donne plus de droits : il ordonne les travaux. Hors de celui du délégant, il est écarté.
    #[test]
    fn lecture_des_perimetres() {
        let c = company();
        let mgr = c.index_of("mgr").unwrap();
        let kids = c.children(mgr);
        let parent = vec!["src/api/**".to_string()];
        let v = json!({"assignments": [
            {"to": "dev1", "task": "t", "scope": ["./src/api/a/**", "docs/**", "../secret/**"]},
            {"to": "dev2", "task": "t"},
        ]});
        let a = parse_assignments(&v, &c, &kids, &parent).unwrap();
        assert_eq!(a[0].scope, vec!["src/api/a/**"], "normalisé ; hors périmètre et remontée écartés");
        assert_eq!(a[1].scope, parent, "un poste qui réalise sans périmètre hérite de celui du délégant : il passera après les autres");
        // Un poste d'analyse sans périmètre n'en a aucun : il ne retient personne.
        let ciso = c.index_of("ciso").unwrap();
        let a = parse_assignments(&json!({"assignments": [{"to": "sec", "task": "t"}]}), &c, &c.children(ciso), &parent).unwrap();
        assert!(a[0].scope.is_empty());
        // Un poste qui dirige une branche garde un périmètre : ses subordonnés en ont besoin pour se répartir.
        let cto = c.index_of("cto").unwrap();
        let a = parse_assignments(&json!({"assignments": [{"to": "mgr", "task": "t"}]}), &c, &c.children(cto), &parent).unwrap();
        assert_eq!(a[0].scope, parent);
    }

    #[test]
    fn lecture_de_la_revue() {
        assert_eq!(parse_review(&json!({"accepted": true, "findings": "ok"})), Review::Accepted);
        assert_eq!(parse_review(&json!({"accepted": false, "findings": " il manque X "})), Review::Unmet("il manque X".into()));
        assert_eq!(parse_review(&json!({"findings": ""})), Review::Unmet("goal not reached".into()), "verdict absent : non validé");
    }

    #[test]
    fn refus_de_realisation_signale() {
        assert!(note_refus(Some(false)).is_some());
        assert!(note_refus(Some(true)).is_none() && note_refus(None).is_none());
    }

    #[test]
    fn rapport_tronque_les_gros_resultats() {
        let gros = "x".repeat(MAX_CHILD_CHARS * 2);
        let p = report_prompt("t", &[("T".into(), "id".into(), "sous-tâche".into(), true, gros)], &[]);
        assert!(p.contains("chars omitted") && p.len() < MAX_CHILD_CHARS * 2);
    }

    /// Un verrou du coût empoisonné par une panique ailleurs reste utilisable (pas de panic en chaîne).
    #[test]
    fn compteur_de_cout_reste_utilisable_apres_empoisonnement() {
        let cost = Arc::new(Mutex::new(5u64));
        let poisoner = cost.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("empoisonnement volontaire du test");
        })
        .join();
        assert!(cost.is_poisoned());
        *super::shared::cost_guard(&cost) += 1;
        assert_eq!(*super::shared::cost_guard(&cost), 6);
    }

    // ----------------------------------------------------------- plan puis descente unique

    #[tokio::test]
    async fn le_boss_planifie_seul_puis_une_seule_descente() {
        let p = Arc::new(Probe::default());
        let (out, evs) = run(&company(), p.clone(), Behavior::default()).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.report, "rapport(ceo)");
        let started = p.started();
        assert_eq!(started.len() as u32, BASE_CALLS, "{started:?}");
        assert_eq!(out.calls, BASE_CALLS);
        assert_eq!(c_estimate(&company()), BASE_CALLS, "l'estimation annonce le flux de base");
        // Le plan : un appel, celui du boss. Personne d'autre ne « conçoit ».
        assert_eq!(started.iter().filter(|id| id.contains("/conception/")).cloned().collect::<Vec<_>>(), vec!["ceo/conception/work"]);
        assert_eq!(started[0], "ceo/conception/work", "le plan précède toute délégation");
        assert_eq!(out.conception.report, "travail(ceo)", "le plan est le livrable du boss");
        // La descente mobilise tout l'organigramme, postes d'analyse compris.
        for id in ["ceo/realisation/delegate", "mgr/realisation/delegate", "dev1/realisation/work", "sec/realisation/work", "ciso/realisation/report", "ceo/realisation/review", "ceo/realisation/report"] {
            assert!(started.iter().any(|s| s == id), "{id} absent : {started:?}");
        }
        assert!(out.realisation.is_some() && out.approved == Some(true));
        assert!(requests(&evs).is_empty(), "rien n'a bloqué : aucune demande à l'humain");
        assert!(matches!(evs.last(), Some(RunEvent::RunFinished { ok: true, .. })));
    }

    fn c_estimate(c: &Company) -> u32 {
        c.estimate_calls().total()
    }

    #[tokio::test]
    async fn le_plan_demande_les_questions_avant_d_ecrire_et_part_au_boss() {
        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior::default()).await;
        let plan = p.prompt("ceo/conception/work");
        assert!(plan.contains("BEFORE writing, ask the human") && plan.contains("mcp__essaim__ask_human"), "{plan}");
        assert!(plan.contains("Backend Engineer") && plan.contains("Security Analyst"), "le boss voit tout l'organigramme pour répartir : {plan}");
        assert!(p.system("ceo/conception/work").contains("PLAN PHASE"));
        // Le plan validé revient au boss pour la descente.
        let root = p.prompt("ceo/realisation/delegate");
        assert!(root.contains("approved by the human") && root.contains("travail(ceo)"), "{root}");
    }

    #[tokio::test]
    async fn ordre_haut_en_bas_puis_bas_en_haut() {
        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior::default()).await;
        let started = p.started();
        let finished = p.finished.lock().unwrap().clone();
        let s = |id: &str| started.iter().position(|x| x == id).unwrap_or_else(|| panic!("{id} jamais lancé : {started:?}"));
        let f = |id: &str| finished.iter().position(|x| x == id).unwrap();
        assert!(f("ceo/conception/work") < s("ceo/realisation/delegate"));
        assert!(f("ceo/realisation/delegate") < s("cto/realisation/delegate"));
        assert!(f("cto/realisation/delegate") < s("mgr/realisation/delegate"));
        assert!(f("mgr/realisation/delegate") < s("dev1/realisation/work"));
        assert!(f("dev1/realisation/work") < s("mgr/realisation/report"));
        assert!(f("dev2/realisation/work") < s("mgr/realisation/report"));
        assert!(f("mgr/realisation/report") < s("cto/realisation/report"));
        assert!(f("sec/realisation/work") < s("ciso/realisation/report"));
        assert!(f("cto/realisation/report") < s("ceo/realisation/review"), "la revue porte sur les résultats remontés");
        assert!(f("ceo/realisation/review") < s("ceo/realisation/report"));
    }

    #[tokio::test]
    async fn entreprise_d_un_seul_poste_le_boss_planifie_puis_realise() {
        let c = Company::parse("name = \"solo\"\ncheckpoint = false\n[[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"a\"\n").unwrap();
        let p = Arc::new(Probe::default());
        let (out, evs) = run(&c, p.clone(), Behavior::default()).await;
        assert!(out.ok);
        assert_eq!(p.started(), vec!["ceo/conception/work", "ceo/realisation/work"], "ni délégation, ni rapport, ni revue");
        assert_eq!(c_estimate(&c), 2);
        assert!(p.system("ceo/realisation/work").contains("You carry out the work in the project yourself"));
        assert!(p.prompt("ceo/realisation/work").contains("Carry it out in the project"));
        assert!(evs.iter().any(|e| matches!(e, RunEvent::Label { label, .. } if label == "CEO · builds ⚒")));
    }

    // ----------------------------------------------------------- distribution de l'information

    #[tokio::test]
    async fn chaque_poste_recoit_sa_tache_et_va_chercher_le_reste() {
        let dir = tmpdir();
        let p = Arc::new(Probe::default());
        let b = Behavior { context: vec!["mgr"], ..Default::default() };
        // La mission est rangée dans le projet (ici le dossier temporaire) : les chemins annoncés aux postes sont relatifs.
        let (out, _) = run_cfg(&company(), p.clone(), b, Cfg { mission_dir: Some(dir.clone()), ..Default::default() }).await;
        assert!(out.ok);
        let nom = dir.file_name().unwrap().to_str().unwrap().to_string();

        // Le brief ne va qu'au boss ; un autre poste sait seulement où le trouver s'il en a besoin.
        let sys = p.system("mgr/realisation/delegate");
        assert!(p.system("ceo/realisation/delegate").contains("Brief de test"), "le boss garde le brief");
        assert!(!sys.contains("Brief de test"), "le brief ne descend pas sous le boss");
        assert!(sys.contains("You do not see the company brief") && sys.contains(&format!("the brief is `{nom}/brief.md`")), "{sys}");
        assert!(sys.contains("Engineering Manager") && sys.contains("N-2") && sys.contains("CEO → CTO → Engineering Manager (you)"));
        assert!(sys.contains("dev1 · Backend Engineer · builds in the project"), "{sys}");

        // Un fait se cherche, une intention se demande à l'humain, on ne demande rien au supérieur.
        for regle in ["a FACT", "you look up yourself", "An INTENT", "mcp__essaim__ask_human", "Never ask your supervisor anything", "at most two attempts at the same failure"] {
            assert!(sys.contains(regle), "règle absente de la fiche de poste : {regle}");
        }
        // Notes partagées et décisions de l'humain : annoncées à tous, créées avec la mission.
        assert!(sys.contains(&format!("`{nom}/notes/`")) && sys.contains(&format!("`{nom}/notes/decisions.md`")), "{sys}");
        assert!(dir.join("notes").is_dir(), "dossier de notes créé au lancement");

        // Le délégant donne des pointeurs, pas des copies ; ils arrivent dans la tâche du subordonné.
        let del = p.prompt("mgr/realisation/delegate");
        assert!(del.contains("`context`") && del.contains("instead of copying their content"), "{del}");
        let work = p.prompt("dev1/realisation/work");
        assert!(work.contains("task for dev1") && work.contains("Context pointers from your supervisor") && work.contains("- src/dev1.rs:10-40"), "{work}");
        // Le rapport du supérieur rappelle la tâche, sans les pointeurs.
        let rep = p.prompt("mgr/realisation/report");
        assert!(rep.contains("travail(dev1)") && rep.contains("travail(dev2)") && rep.contains("Assigned task: task for dev1") && !rep.contains("src/dev1.rs"), "{rep}");
        let ceo = p.prompt("ceo/realisation/report");
        assert!(ceo.contains("rapport(cto)") && ceo.contains("rapport(ciso)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn un_poste_qui_realise_et_un_poste_qui_conseille_n_ont_pas_la_meme_consigne() {
        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior::default()).await;
        assert!(p.prompt("dev1/realisation/work").contains("Carry it out in the project"));
        assert!(p.system("dev1/realisation/work").contains("File scope assigned to you: src/cto/mgr/dev1/**"));
        let sec = p.prompt("sec/realisation/work");
        assert!(sec.contains("Produce your deliverable") && !sec.contains("Carry it out"), "{sec}");
        assert!(p.system("sec/realisation/work").contains("You analyse and advise"));
        assert!(p.system("cto/realisation/delegate").contains("You coordinate"));
    }

    // ----------------------------------------------------------- droits

    #[tokio::test]
    async fn tous_les_postes_ont_tous_les_outils_sans_aucun_refus() {
        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior::default()).await;
        for id in ["ceo/conception/work", "ceo/realisation/delegate", "mgr/realisation/delegate", "dev1/realisation/work", "sec/realisation/work", "mgr/realisation/report", "ceo/realisation/review"] {
            let s = p.spec(id);
            assert_eq!(s.tools.as_deref(), Some("Read,Grep,Glob,Edit,Write,Bash,WebSearch,WebFetch"), "{id}");
            assert_eq!(s.permission_mode.as_deref(), Some("bypassPermissions"), "{id} : profil dédié, rien n'est refusé");
            assert!(s.allowed_tools.is_empty() && s.settings.is_none() && s.permission_prompt_tool.is_none(), "{id}");
        }
    }

    #[tokio::test]
    async fn profil_protege_bac_a_sable_et_demandes_a_l_humain() {
        let p = Arc::new(Probe::default());
        let cfg = Cfg { access: Some(Settings::default().access()), ..Default::default() };
        run_cfg(&company(), p.clone(), Behavior::default(), cfg).await;
        let s = p.spec("dev1/realisation/work");
        assert_eq!(s.tools.as_deref(), Some("Read,Grep,Glob,Edit,Write,Bash,WebSearch,WebFetch"), "mêmes outils : c'est la machine qui est protégée");
        assert_eq!(s.permission_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(s.allowed_tools, vec!["WebSearch"]);
        let v: Value = serde_json::from_str(s.settings.as_deref().expect("réglages du bac à sable")).unwrap();
        assert_eq!(v["sandbox"]["enabled"], true);
        assert_eq!(v["permissions"]["blockReadsOutsideWorkingDirectories"], true);
        // Sans bus (pas d'humain joignable), aucun outil de permission n'est annoncé : `claude` refuse de lui-même.
        assert!(s.permission_prompt_tool.is_none() && s.mcp_config.is_none());
    }

    #[tokio::test]
    async fn la_liste_d_outils_de_l_agent_ne_restreint_plus_le_poste() {
        let p = Arc::new(Probe::default());
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_ctx, crx) = watch::channel(false);
        let ag = vec![AgentDef { name: "a".into(), description: "d".into(), prompt: "p".into(), tools: Some(vec!["Read".into()]), ..Default::default() }];
        run_mission(&company(), &ag, "Brief", &options(&Cfg::default()), fake(p.clone(), Behavior::default()), tx, crx).await.unwrap();
        assert!(p.spec("dev1/realisation/work").agents["a"]["tools"].is_null(), "essaim équipe les postes, pas le fichier d'agent");
    }

    #[tokio::test]
    async fn assistants_legers_avec_un_modele_moindre() {
        let mut c = company();
        let dev1 = c.index_of("dev1").unwrap();
        c.seats[dev1].helpers = Some("haiku".into());
        let p = Arc::new(Probe::default());
        run(&c, p.clone(), Behavior::default()).await;
        let s = p.spec("dev1/realisation/work");
        assert!(s.tools.as_deref().unwrap().ends_with(",Agent"));
        let h = &s.agents["essaim-assistant"];
        assert_eq!(h["model"], "haiku");
        assert_eq!(h["tools"], json!(["Read", "Grep", "Glob", "Bash"]), "l'assistant lit et lance, il n'écrit pas de fichiers");
        assert!(p.system("dev1/realisation/work").contains("light assistants"));
        assert!(!p.spec("dev2/realisation/work").agents.contains_key("essaim-assistant"), "seulement pour le poste qui en déclare");
    }

    // ----------------------------------------------------------- blocages : l'humain, et lui seul

    #[tokio::test]
    async fn poste_bloque_l_humain_repond_et_le_poste_reprend_sa_session() {
        let p = Arc::new(Probe::default());
        let b = Behavior { blocked_once: vec!["dev1/realisation/work"], ..Default::default() };
        let cfg = Cfg { humans: vec![HumanAnswer::Text("le port est 8080".into())], ..Default::default() };
        let (out, evs) = run_cfg(&company(), p.clone(), b, cfg).await;
        assert!(out.ok, "la réponse de l'humain a débloqué le poste : {out:?}");
        let reqs = requests(&evs);
        assert_eq!(reqs.len(), 1, "une seule demande, directement à l'humain");
        assert_eq!((reqs[0].kind, reqs[0].seat.as_str()), (AskKind::Blocked, "Backend Engineer"));
        assert!(reqs[0].questions[0].text.contains("tests rouges chez dev1") && reqs[0].questions[0].text.contains("needs: corriger"), "{}", reqs[0].questions[0].text);
        assert_eq!(reqs[0].questions[0].options.len(), 3, "relancer, continuer sans lui, arrêter");
        // Le poste repart dans SA session, avec la réponse : rien n'est refait, aucun supérieur n'est appelé.
        let reprise = p.resumed("dev1/realisation/work").expect("un appel de reprise");
        assert_eq!(reprise.resume.as_deref(), Some("sid-dev1"));
        assert!(reprise.prompt.contains("le port est 8080") && reprise.prompt.contains("Do not redo what is already done"), "{}", reprise.prompt);
        assert_eq!(out.calls, BASE_CALLS + 1, "un appel de plus : la reprise, rien d'autre");
        let started = p.started();
        assert!(!started.iter().any(|id| id.contains("/fix") || id.contains("/answer")), "aucune boucle de correction : {started:?}");
        assert!(p.prompt("mgr/realisation/report").contains("reprise(dev1)"), "le supérieur reçoit le résultat abouti");
    }

    #[tokio::test]
    async fn poste_bloque_sans_humain_reste_bloque_et_rien_ne_boucle() {
        let p = Arc::new(Probe::default());
        let b = Behavior { blocked: vec!["dev1/realisation/work"], ..Default::default() };
        let (out, evs) = run(&company(), p.clone(), b).await;
        assert!(!out.ok);
        assert_eq!(requests(&evs).len(), 1, "la demande est posée une fois, pas à chaque échelon");
        assert_eq!(p.started().iter().filter(|id| id.starts_with("dev1/")).count(), 1, "aucun nouvel essai automatique");
        let r = out.realisation.unwrap();
        assert!(r.blockers.iter().any(|b| b.kind == "tests" && b.detail.contains("Backend Engineer") && b.detail.contains("tests rouges chez dev1")), "{:?}", r.blockers);
        let mgr = p.prompt("mgr/realisation/report");
        assert!(mgr.contains("NOT COMPLETED") && mgr.contains("tests rouges chez dev1"), "{mgr}");
        // Un résultat que la branche sait incomplet n'est pas soumis à la revue : ce qui manque est déjà connu.
        assert!(!p.started().iter().any(|id| id == "ceo/realisation/review"));
        assert!(evs.iter().any(|e| matches!(e, RunEvent::NodeFinished { id, status: NodeStatus::Blocked(_), .. } if id == "dev1/realisation/work")));
    }

    #[tokio::test]
    async fn chaque_nouvel_essai_est_une_decision_de_l_humain() {
        let p = Arc::new(Probe::default());
        let b = Behavior { blocked: vec!["dev1/realisation/work"], ..Default::default() };
        // L'humain relance deux fois (une consigne, puis l'option « relancer »), puis plus personne ne répond.
        let cfg = Cfg { humans: vec![HumanAnswer::Text("essaie avec --locked".into()), HumanAnswer::Option(0)], ..Default::default() };
        let (out, evs) = run_cfg(&company(), p.clone(), b, cfg).await;
        assert!(!out.ok);
        assert_eq!(requests(&evs).len(), 3, "une demande par issue non aboutie");
        assert_eq!(p.started().iter().filter(|id| id.starts_with("dev1/")).count(), 3, "le premier appel et deux reprises demandées");
        let reprises: Vec<String> = p.specs.lock().unwrap().iter().filter(|(id, s)| id.starts_with("dev1/") && s.resume.is_some()).map(|(_, s)| s.prompt.clone()).collect();
        assert!(reprises[0].contains("essaie avec --locked"));
        assert!(reprises[1].starts_with("The human asks you to try again."), "{}", reprises[1]);
    }

    #[tokio::test]
    async fn sans_l_accord_de_l_humain_les_travaux_suivants_ne_partent_pas() {
        let p = Arc::new(Probe::default());
        let b = Behavior { overlap: true, blocked: vec!["dev1/realisation/work"], ..Default::default() };
        let (out, _) = run(&company(), p.clone(), b).await;
        assert!(!out.ok);
        let started = p.started();
        assert!(!started.iter().any(|id| id == "dev2/realisation/work"), "dev2 passait après dev1 (même périmètre) : il ne part pas");
        assert!(out.realisation.unwrap().blockers.iter().any(|b| b.kind == "prerequisite" && b.detail.contains("Frontend Engineer")));
    }

    #[tokio::test]
    async fn l_humain_dit_de_continuer_sans_le_poste() {
        let p = Arc::new(Probe::default());
        let b = Behavior { overlap: true, blocked: vec!["dev1/realisation/work"], ..Default::default() };
        let cfg = Cfg { humans: vec![HumanAnswer::Option(1)], ..Default::default() };
        let (out, evs) = run_cfg(&company(), p.clone(), b, cfg).await;
        assert!(!out.ok, "le poste n'a pas fini : la mission n'est pas déclarée réussie");
        assert_eq!(requests(&evs).len(), 1);
        assert!(p.started().iter().any(|id| id == "dev2/realisation/work"), "le travail suivant part quand même");
        // La branche bloquée n'arrête pas non plus ses voisines : le CISO, lancé après le CTO, travaille.
        assert!(p.started().iter().any(|id| id == "sec/realisation/work"));
    }

    #[tokio::test]
    async fn l_humain_arrete_la_mission() {
        let p = Arc::new(Probe::default());
        let b = Behavior { overlap: true, blocked: vec!["dev1/realisation/work"], ..Default::default() };
        let cfg = Cfg { humans: vec![HumanAnswer::Option(2)], ..Default::default() };
        let (out, evs) = run_cfg(&company(), p.clone(), b, cfg).await;
        assert!(!out.ok);
        let started = p.started();
        let apres: Vec<&String> = started.iter().skip_while(|id| *id != "dev1/realisation/work").skip(1).collect();
        assert!(apres.is_empty(), "plus aucun appel après l'arrêt demandé : {apres:?}");
        assert!(matches!(evs.last(), Some(RunEvent::RunFinished { ok: false, .. })));
    }

    #[tokio::test]
    async fn sortie_structuree_perdue_le_texte_produit_est_montre_a_l_humain() {
        let p = Arc::new(Probe::default());
        let b = Behavior { no_structured: vec!["sec/realisation/work"], ..Default::default() };
        let (out, evs) = run(&company(), p.clone(), b).await;
        assert!(!out.ok);
        let ids: Vec<String> = p.started().into_iter().filter(|id| id.starts_with("sec/")).collect();
        assert_eq!(ids.len(), 2, "le format est redemandé une fois, dans la même session : {ids:?}");
        assert_eq!(p.resumed("sec/realisation/work").and_then(|s| s.resume).as_deref(), Some("sid-sec"));
        let reqs = requests(&evs);
        assert!(reqs[0].questions[0].text.contains("trois failles XSS"), "{}", reqs[0].questions[0].text);
        assert!(p.prompt("ciso/realisation/report").contains("trois failles XSS"), "le texte produit n'est pas perdu");
    }

    #[tokio::test]
    async fn plan_bloque_l_humain_est_consulte_et_rien_n_est_realise_sans_plan() {
        let p = Arc::new(Probe::default());
        let b = Behavior { blocked: vec!["ceo/conception/work"], ..Default::default() };
        let (out, evs) = run(&company(), p.clone(), b).await;
        assert!(!out.ok && out.realisation.is_none());
        assert_eq!(requests(&evs).len(), 1);
        assert_eq!(p.started(), vec!["ceo/conception/work"]);
    }

    #[tokio::test]
    async fn delegation_illisible_l_humain_decide() {
        // Sans humain : la branche échoue, le supérieur le rapporte.
        let p = Arc::new(Probe::default());
        let b = Behavior { bad_delegation: vec!["mgr"], ..Default::default() };
        let (out, evs) = run(&company(), p.clone(), b).await;
        assert!(!out.ok);
        let started = p.started();
        assert_eq!(started.iter().filter(|id| id.starts_with("mgr/realisation/delegate")).count(), 2, "un nouvel essai de format, dans la même session");
        assert!(!started.iter().any(|id| id.starts_with("dev")), "personne n'a reçu de tâche");
        assert_eq!(requests(&evs).len(), 1);
        assert!(p.prompt("cto/realisation/report").contains("delegation impossible"));

        // Avec une consigne de l'humain : la délégation repart, avec cette consigne.
        let p = Arc::new(Probe::default());
        let b = Behavior { bad_delegation_once: vec!["mgr"], ..Default::default() };
        let cfg = Cfg { humans: vec![HumanAnswer::Text("confie tout à dev1".into())], ..Default::default() };
        let (out, _) = run_cfg(&company(), p.clone(), b, cfg).await;
        assert!(out.ok, "{out:?}");
        let relance = p.prompts.lock().unwrap().iter().filter(|(id, _, _)| id.starts_with("mgr/realisation/delegate")).map(|(_, pr, _)| pr.clone()).next_back().unwrap();
        assert!(relance.contains("Instruction from the human:\nconfie tout à dev1"), "{relance}");
    }

    // ----------------------------------------------------------- revue du boss : elle constate, l'humain décide

    #[tokio::test]
    async fn revue_non_conforme_l_humain_relance_une_descente_corrective() {
        let p = Arc::new(Probe::default());
        let b = Behavior { review_unmet_once: true, ..Default::default() };
        let cfg = Cfg { humans: vec![HumanAnswer::Text("priorité au critère X".into())], ..Default::default() };
        let (out, evs) = run_cfg(&company(), p.clone(), b, cfg).await;
        assert!(out.ok, "{out:?}");
        let reqs = requests(&evs);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].kind, AskKind::Review);
        assert!(reqs[0].questions[0].text.contains("critère X non atteint"));
        let started = p.started();
        // Les appels de la descente corrective sont des nœuds distincts (« …/review#n ») : leur coût s'ajoute
        // à celui des premiers au lieu de l'écraser.
        assert_eq!(started.iter().filter(|id| id.starts_with("ceo/realisation/review")).count(), 2, "revue, descente corrective, nouvelle revue");
        assert_eq!(started.iter().collect::<std::collections::BTreeSet<_>>().len(), started.len(), "aucun nom de nœud en double : {started:?}");
        let delegations: Vec<String> = p.prompts.lock().unwrap().iter().filter(|(id, _, _)| id.starts_with("ceo/realisation/delegate")).map(|(_, pr, _)| pr.clone()).collect();
        assert_eq!(delegations.len(), 2);
        assert!(delegations[1].contains("critère X non atteint") && delegations[1].contains("priorité au critère X") && delegations[1].contains("assign only what is needed"), "{}", delegations[1]);
        assert_eq!(started.iter().filter(|id| *id == "ceo/realisation/report").count(), 1, "un seul rapport final");
    }

    #[tokio::test]
    async fn revue_non_conforme_sans_humain_la_mission_n_est_pas_reussie() {
        let p = Arc::new(Probe::default());
        let b = Behavior { review_unmet: true, ..Default::default() };
        let (out, evs) = run(&company(), p.clone(), b).await;
        assert!(!out.ok);
        assert_eq!(requests(&evs).len(), 1);
        assert_eq!(p.started().iter().filter(|id| *id == "ceo/realisation/review").count(), 1, "aucune relance automatique");
        assert!(out.realisation.unwrap().blockers.iter().any(|b| b.detail.contains("critère X non atteint")));
    }

    #[tokio::test]
    async fn revue_non_conforme_acceptee_telle_quelle_par_l_humain() {
        let p = Arc::new(Probe::default());
        let b = Behavior { review_unmet: true, ..Default::default() };
        let cfg = Cfg { humans: vec![HumanAnswer::Option(1)], ..Default::default() };
        let (out, _) = run_cfg(&company(), p.clone(), b, cfg).await;
        assert!(out.ok, "l'humain a accepté le résultat");
        assert_eq!(p.started().iter().filter(|id| *id == "ceo/realisation/review").count(), 1);
    }

    #[tokio::test]
    async fn revue_desactivee() {
        let reglages: [fn(&mut Company); 2] = [|c| c.review = Some(false), |c| c.max_review_rounds = 0];
        for regler in reglages {
            let mut c = company();
            regler(&mut c);
            let p = Arc::new(Probe::default());
            let (out, _) = run(&c, p.clone(), Behavior::default()).await;
            assert!(out.ok);
            assert!(!p.started().iter().any(|id| id.contains("/review")));
            assert_eq!(out.calls, BASE_CALLS - 1);
            assert_eq!(c_estimate(&c), BASE_CALLS - 1);
        }
    }

    // ----------------------------------------------------------- étapes, périmètres, parallélisme

    /// Deux affectations au même subordonné s'exécutent comme deux étapes **successives**, chacune dans
    /// un appel `claude` neuf : c'est ce qui garde les contextes petits.
    #[tokio::test]
    async fn deux_etapes_pour_un_meme_poste_sont_deux_appels_successifs() {
        let p = Arc::new(Probe::default());
        let b = Behavior { two_steps: vec!["mgr"], ..Default::default() };
        let (out, _) = run(&company(), p.clone(), b).await;
        assert!(out.ok, "{out:?}");
        let ids: Vec<String> = p.started().into_iter().filter(|id| id.starts_with("dev1/")).collect();
        assert_eq!(ids, vec!["dev1/realisation/work@1", "dev1/realisation/work@2"], "deux étapes = deux nœuds nommés");
        let tl = p.timeline.lock().unwrap().clone();
        let pos = |x: &str| tl.iter().position(|e| e == x).unwrap_or_else(|| panic!("absent de la chronologie : {x}\n{tl:?}"));
        assert!(pos("E:dev1/realisation/work@1") < pos("S:dev1/realisation/work@2"), "{tl:?}");
        let (un, deux) = (p.prompt("dev1/realisation/work@1"), p.prompt("dev1/realisation/work@2"));
        assert!(un.contains("task for dev1") && !un.contains("etape2 pour dev1"), "{un}");
        assert!(deux.contains("etape2 pour dev1") && !deux.contains("task for dev1"), "{deux}");
        assert!(un.contains("Step 1 of 2") && deux.contains("Step 2 of 2"), "{un}\n---\n{deux}");
        assert!(!un.contains("one commit"), "pas de commit imposé sans `commit_steps`");
    }

    #[tokio::test]
    async fn commit_par_etape_seulement_si_l_entreprise_le_demande() {
        let mut c = company();
        c.commit_steps = true;
        let p = Arc::new(Probe::default());
        run(&c, p.clone(), Behavior { two_steps: vec!["mgr"], ..Default::default() }).await;
        assert!(p.prompt("dev1/realisation/work@1").contains("End this step with one commit covering exactly it."));
    }

    #[tokio::test]
    async fn une_etape_en_echec_arrete_les_suivantes() {
        let p = Arc::new(Probe::default());
        let b = Behavior { two_steps: vec!["mgr"], fail: vec!["dev1/realisation/work@1"], ..Default::default() };
        run(&company(), p.clone(), b).await;
        let ids: Vec<String> = p.started().into_iter().filter(|id| id.starts_with("dev1/")).collect();
        assert_eq!(ids, vec!["dev1/realisation/work@1"], "la 2e étape ne part pas après l'échec de la 1re");
    }

    #[tokio::test]
    async fn perimetres_disjoints_en_parallele_chevauchement_en_sequence() {
        let chrono = |tl: &[String], a: &str, b: &str| {
            let pos = |x: String| tl.iter().position(|e| *e == x).unwrap();
            (pos(format!("S:{a}")), pos(format!("E:{a}")), pos(format!("S:{b}")), pos(format!("E:{b}")))
        };
        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior::default()).await;
        let tl = p.timeline.lock().unwrap().clone();
        let (s1, e1, s2, e2) = chrono(&tl, "dev1/realisation/work", "dev2/realisation/work");
        assert!(s1 < e2 && s2 < e1, "périmètres disjoints : en même temps ({tl:?})");

        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior { overlap: true, ..Default::default() }).await;
        let tl = p.timeline.lock().unwrap().clone();
        let (s1, e1, s2, e2) = chrono(&tl, "dev1/realisation/work", "dev2/realisation/work");
        assert!(e1 < s2 || e2 < s1, "même périmètre : l'un après l'autre ({tl:?})");
    }

    /// Une seule délégation, mais un ordre entre subordonnés : la vague 2 ne démarre qu'une fois la vague 1
    /// terminée (réaliser après une spécification), et un poste rappelé en vague 3 refait une descente.
    #[tokio::test]
    async fn les_vagues_d_une_delegation_se_suivent() {
        let p = Arc::new(Probe::default());
        let b = Behavior { stages: vec![("ceo", "cto", 2)], again: vec![("ceo", "ciso", 3)], ..Default::default() };
        let (out, _) = run(&company(), p.clone(), b).await;
        assert!(out.ok, "{out:?}");
        let tl = p.timeline.lock().unwrap().clone();
        let pos = |x: &str| tl.iter().position(|e| e == x).unwrap_or_else(|| panic!("absent : {x} ({tl:?})"));
        assert!(pos("E:ciso/realisation/report") < pos("S:cto/realisation/delegate"), "vague 1 terminée avant la vague 2 ({tl:?})");
        // Vague 3 : le CISO est rappelé. Ses appels sont des nœuds distincts, sinon leur coût écraserait celui des premiers.
        let rappels: Vec<&String> = tl.iter().filter(|e| e.starts_with("S:ciso/realisation/delegate#") || e.starts_with("S:sec/realisation/work#") || e.starts_with("S:ciso/realisation/report#")).collect();
        assert_eq!(rappels.len(), 3, "délégation, travail et rapport de la vague 3 ({tl:?})");
        assert!(pos("E:cto/realisation/report") < pos(rappels[0]), "vague 2 terminée avant la vague 3 ({tl:?})");
        assert!(p.prompts.lock().unwrap().iter().any(|(id, pr, _)| id.starts_with("ciso/realisation/delegate#") && pr.contains("suite pour ciso")), "la vague 3 porte sa propre tâche");
        // Aucun appel de plus pour le délégant : une délégation, une revue, un rapport.
        let started = p.started();
        assert_eq!(started.iter().filter(|id| id.starts_with("ceo/realisation/delegate")).count(), 1, "une seule délégation du boss");
        assert_eq!(started.len() as u32, BASE_CALLS + 3, "{started:?}");
    }

    #[tokio::test]
    async fn une_vague_en_echec_arrete_les_suivantes() {
        let p = Arc::new(Probe::default());
        let b = Behavior { stages: vec![("ceo", "cto", 2)], fail: vec!["sec/realisation/work"], ..Default::default() };
        let (out, _) = run(&company(), p.clone(), b).await;
        assert!(!out.ok);
        let started = p.started();
        assert!(!started.iter().any(|id| id.starts_with("cto/") || id.starts_with("dev")), "on ne construit pas sur une vague cassée : {started:?}");
        let restes = out.realisation.map(|r| r.blockers).unwrap_or_default();
        assert!(restes.iter().any(|b| b.detail.contains("not started")), "{restes:?}");
    }

    #[test]
    fn la_delegation_dit_qu_elle_est_unique_et_comment_ordonner() {
        let d = delegate_prompt("t", &["**".to_string()]);
        assert!(d.contains("ONLY delegation") && d.contains("`stage`") && d.contains("shared notes"), "{d}");
    }

    #[tokio::test]
    async fn perimetre_absent_ou_hors_de_celui_du_delegant() {
        // Hors périmètre : écarté ; le poste qui réalise hérite de celui du délégant, et travaille quand même.
        let p = Arc::new(Probe::default());
        let (out, _) = run(&company(), p.clone(), Behavior { escape: vec!["mgr"], ..Default::default() }).await;
        assert!(out.ok);
        assert!(p.system("dev1/realisation/work").contains("File scope assigned to you: src/cto/mgr/**"), "{}", p.system("dev1/realisation/work"));
        // Un poste d'analyse sans périmètre : aucun fichier du projet à modifier, et il ne retient personne.
        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior { no_scope: vec!["ciso"], ..Default::default() }).await;
        assert!(p.system("sec/realisation/work").contains("No file scope was assigned to you"));
    }

    #[tokio::test]
    async fn parallelisme_borne_par_max_parallel() {
        let mut c = company();
        c.max_parallel = 1;
        let p = Arc::new(Probe::default());
        run(&c, p.clone(), Behavior::default()).await;
        assert_eq!(p.peak.load(Ordering::SeqCst), 1);

        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior::default()).await;
        let peak = p.peak.load(Ordering::SeqCst);
        assert!(peak > 1 && peak <= 4, "pic = {peak}");
    }

    #[tokio::test]
    async fn delegation_partielle_ne_mobilise_que_les_choisis() {
        let p = Arc::new(Probe::default());
        let b = Behavior { only: vec![("ceo", vec!["cto"])], ..Default::default() };
        let (out, _) = run(&company(), p.clone(), b).await;
        assert!(out.ok);
        let started = p.started();
        assert!(!started.iter().any(|id| id.starts_with("ciso/") || id.starts_with("sec/")), "{started:?}");
        assert_eq!(started.len(), 1 + 2 + 2 + 2 + 2 + 1, "plan ; ceo, cto, mgr (2 appels chacun) ; dev1, dev2 ; revue");
    }

    #[tokio::test]
    async fn evenements_etiquetes_et_indentes() {
        let p = Arc::new(Probe::default());
        let (_, evs) = run(&company(), p, Behavior::default()).await;
        let labels: Vec<(String, usize)> = evs.iter().filter_map(|e| if let RunEvent::Label { label, depth, .. } = e { Some((label.clone(), *depth)) } else { None }).collect();
        for attendu in [("CEO · plans", 0), ("CEO · delegates", 0), ("Engineering Manager · reports", 2), ("Backend Engineer · builds ⚒", 3), ("Security Analyst · analyses", 2), ("CEO · reviews", 0)] {
            assert!(labels.contains(&(attendu.0.to_string(), attendu.1)), "{attendu:?} absent de {labels:?}");
        }
    }

    // ----------------------------------------------------------- plafond, annulation, pause

    #[tokio::test]
    async fn plafond_d_appels_partage_par_toute_la_mission() {
        let mut c = company();
        c.max_calls = 4;
        let p = Arc::new(Probe::default());
        let (out, evs) = run(&c, p.clone(), Behavior::default()).await;
        assert!(!out.ok);
        assert_eq!(p.started().len(), 4, "jamais plus que max_calls");
        assert_eq!(out.calls, 4);
        assert!(requests(&evs).is_empty(), "plafond atteint : aucune relance n'est possible, on ne demande rien à l'humain");
    }

    #[tokio::test]
    async fn annulation_avant_depart() {
        let p = Arc::new(Probe::default());
        let (out, evs) = run_cfg(&company(), p.clone(), Behavior::default(), Cfg { cancel_at_start: true, ..Default::default() }).await;
        assert!(!out.ok);
        assert!(p.started().is_empty());
        assert!(requests(&evs).is_empty());
        assert!(matches!(evs.last(), Some(RunEvent::RunFinished { ok: false, .. })));
    }

    /// R1 : un livrable qui ne peut pas être écrit n'est plus avalé : le run se ferme en échec et l'erreur remonte.
    #[tokio::test]
    async fn echec_d_ecriture_du_livrable_ferme_le_run_en_echec() {
        let base = tmpdir();
        std::fs::create_dir_all(&base).unwrap();
        let bloque = base.join("bloque");
        std::fs::write(&bloque, "x").unwrap(); // un fichier à la place d'un dossier : la création du dossier de mission échoue
        let opts = CompanyOptions::plain(base.clone(), Some(bloque.join("mission")), Lang::Fr);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (_ctx, crx) = watch::channel(false);
        let res = run_mission(&company(), &agents(), "Brief", &opts, fake(Arc::new(Probe::default()), Behavior::default()), tx, crx).await;
        assert!(res.is_err(), "l'erreur d'écriture remonte à l'appelant");
        let mut evs = vec![];
        while let Ok(e) = rx.try_recv() {
            evs.push(e);
        }
        assert!(matches!(evs.last(), Some(RunEvent::RunFinished { ok: false, .. })), "le run est clos en échec");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Limite de l'abonnement atteinte : le run se met en pause au lieu d'échouer, reprend seul à l'heure
    /// annoncée, rejoue l'appel refusé, et ne lui compte ni coût ni appel.
    #[tokio::test(start_paused = true)]
    async fn limite_d_abonnement_met_le_run_en_pause_puis_le_reprend() {
        let dir = tmpdir();
        std::fs::create_dir_all(&dir).unwrap();
        let p = Arc::new(Probe::default());
        let b = Behavior { limit_once: vec!["dev1/realisation/work"], ..Default::default() };
        let (out, evs) = run_cfg(&company(), p.clone(), b, Cfg { mission_dir: Some(dir.clone()), ..Default::default() }).await;
        assert!(out.ok, "la limite ne fait pas échouer la mission");
        assert_eq!(out.calls, BASE_CALLS, "l'appel refusé ne consomme pas le plafond");
        assert_eq!(p.started().iter().filter(|id| *id == "dev1/realisation/work").count(), 2, "appel rejoué à la reprise");
        assert_eq!(evs.iter().filter(|e| matches!(e, RunEvent::Paused { .. })).count(), 1, "une seule annonce de pause");
        assert!(evs.iter().any(|e| matches!(e, RunEvent::Paused { why: crate::runtime::pause::Why::Limit, resets_at: Some(_) })));
        assert_eq!(evs.iter().filter(|e| matches!(e, RunEvent::Resumed)).count(), 1, "une seule annonce de reprise");
        let finis: Vec<_> = evs.iter().filter_map(|e| if let RunEvent::NodeFinished { id, status, .. } = e { (id == "dev1/realisation/work").then(|| status.clone()) } else { None }).collect();
        assert_eq!(finis, vec![NodeStatus::Succeeded], "le nœud n'est clos qu'une fois, en réussite");
        assert!(requests(&evs).is_empty(), "la limite n'est pas un blocage : rien n'est demandé à l'humain");
        assert!(crate::runtime::missions::read_pause(&dir).is_none(), "marque de pause levée à la reprise");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pause demandée pendant qu'un poste travaille : il s'arrête avec un bilan, le poste n'échoue pas,
    /// l'appel ne consomme pas le plafond, et il reprend **sa propre session** avec son bilan en main.
    #[tokio::test]
    async fn un_appel_arrete_par_la_pause_reprend_sa_session() {
        let p = Arc::new(Probe::default());
        let b = Behavior { interrupt_once: vec!["dev1/realisation/work"], ..Default::default() };
        let (out, evs) = run(&company(), p.clone(), b).await;
        assert!(out.ok, "un arrêt demandé ne fait pas échouer la mission");
        assert_eq!(out.calls, BASE_CALLS, "l'appel arrêté ne consomme pas le plafond");
        let finis: Vec<_> = evs.iter().filter_map(|e| if let RunEvent::NodeFinished { id, status, .. } = e { id.starts_with("dev1/realisation/work").then(|| status.clone()) } else { None }).collect();
        assert_eq!(finis, vec![NodeStatus::Succeeded]);
        let reprise = p.resumed("dev1/realisation/work").expect("un appel de reprise");
        assert_eq!(reprise.resume.as_deref(), Some("sid-pause"));
        assert!(reprise.prompt.contains("Reste : l'autre.") && reprise.prompt.contains("Do not redo what you already completed"), "{}", reprise.prompt);
        assert!(out.tokens >= 100, "les jetons de l'appel arrêté sont comptés : {}", out.tokens);
        assert!(requests(&evs).is_empty());
    }

    /// Run annulé pendant la pause (c'est ce qui se passe en quittant essaim) : la mission garde sa marque
    /// de pause, donc elle est reprenable plus tard.
    #[tokio::test(start_paused = true)]
    async fn annulation_pendant_la_pause_laisse_la_mission_reprenable() {
        let dir = tmpdir();
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (ctx, crx) = watch::channel(false);
        // Dès l'annonce de la pause, on annule : c'est la séquence « q » pendant une pause.
        let stop = ctx.clone();
        let collector = tokio::spawn(async move {
            let mut evs = vec![];
            while let Some(e) = rx.recv().await {
                if matches!(e, RunEvent::Paused { .. }) {
                    let _ = stop.send(true);
                }
                evs.push(e);
            }
            evs
        });
        let opts = CompanyOptions::plain(std::env::temp_dir(), Some(dir.clone()), Lang::Fr);
        let b = Behavior { limit_once: vec!["ceo/conception/work"], ..Default::default() };
        let out = run_mission(&company(), &agents(), "Brief", &opts, fake(Arc::new(Probe::default()), b), tx, crx).await.unwrap();
        let evs = collector.await.unwrap();
        assert!(!out.ok, "mission interrompue");
        assert!(evs.iter().any(|e| matches!(e, RunEvent::Paused { .. })));
        assert!(!evs.iter().any(|e| matches!(e, RunEvent::Resumed)), "annulée, pas reprise");
        assert_eq!(crate::runtime::missions::read_pause(&dir).map(|p| p.resets_at.is_some()), Some(true), "mission toujours en pause, avec son heure de reprise");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ----------------------------------------------------------- point de contrôle et reprise

    #[tokio::test]
    async fn point_de_controle_accepte_ecrit_les_livrables() {
        let dir = tmpdir();
        let p = Arc::new(Probe::default());
        let cfg = Cfg { answer: Some(true), mission_dir: Some(dir.clone()), ..Default::default() };
        let (out, evs) = run_cfg(&company_checkpoint(), p.clone(), Behavior::default(), cfg).await;
        assert_eq!(out.approved, Some(true));
        let plan = evs.iter().find_map(|e| if let RunEvent::Checkpoint { plan, .. } = e { Some(plan.clone()) } else { None }).expect("point de contrôle");
        assert_eq!(plan, "travail(ceo)", "c'est le plan du boss qui est soumis à l'humain");
        assert_eq!(std::fs::read_to_string(dir.join("conception.md")).unwrap(), "travail(ceo)");
        assert_eq!(std::fs::read_to_string(dir.join("rapport.md")).unwrap(), "rapport(ceo)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn point_de_controle_refuse_arrete_sur_le_plan() {
        let dir = tmpdir();
        let p = Arc::new(Probe::default());
        let cfg = Cfg { answer: Some(false), mission_dir: Some(dir.clone()), ..Default::default() };
        let (out, evs) = run_cfg(&company_checkpoint(), p.clone(), Behavior::default(), cfg).await;
        assert_eq!(out.approved, Some(false));
        assert!(out.ok, "refuser n'est pas un échec");
        assert!(out.realisation.is_none());
        assert_eq!(p.started(), vec!["ceo/conception/work"], "rien n'est réalisé sans accord");
        assert!(dir.join("conception.md").is_file() && !dir.join("rapport.md").exists());
        assert!(!dir.join("reussie").exists(), "un plan refusé n'est pas une mission réussie");
        assert!(matches!(evs.last(), Some(RunEvent::RunFinished { ok: true, .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reprise_saute_le_plan_deja_ecrit() {
        let dir = tmpdir();
        let c = company_checkpoint();
        // 1er run : plan écrit, réalisation refusée (= run interrompu avant la réalisation).
        let cfg = Cfg { answer: Some(false), mission_dir: Some(dir.clone()), ..Default::default() };
        run_cfg(&c, Arc::new(Probe::default()), Behavior::default(), cfg).await;
        assert!(crate::runtime::missions::resumable_plan(&dir).is_some());
        // Reprise : le plan n'est pas réécrit, il alimente la descente.
        let p = Arc::new(Probe::default());
        let cfg = Cfg { answer: Some(true), mission_dir: Some(dir.clone()), resume: true, ..Default::default() };
        let (out, _) = run_cfg(&c, p.clone(), Behavior::default(), cfg).await;
        assert!(out.ok && out.approved == Some(true) && out.realisation.is_some());
        let started = p.started();
        assert!(!started.iter().any(|id| id.contains("/conception/")), "{started:?}");
        assert!(p.prompt("ceo/realisation/delegate").contains("travail(ceo)"));
        assert!(dir.join("rapport.md").is_file());
        // Sans reprise, le plan est réécrit.
        let p = Arc::new(Probe::default());
        let cfg = Cfg { answer: Some(false), mission_dir: Some(dir.clone()), ..Default::default() };
        run_cfg(&c, p.clone(), Behavior::default(), cfg).await;
        assert!(p.started().iter().any(|id| id.contains("/conception/")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn plan_retouche_pendant_le_point_de_controle_est_celui_de_la_realisation() {
        let dir = tmpdir();
        let p = Arc::new(Probe::default());
        // Personne ne répond automatiquement : on retouche le plan puis on valide, comme la TUI.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (_ctx, crx) = watch::channel(false);
        let opts = CompanyOptions::plain(std::env::temp_dir(), Some(dir.clone()), Lang::Fr);
        let d2 = dir.clone();
        let collector = tokio::spawn(async move {
            while let Some(e) = rx.recv().await {
                if let RunEvent::Checkpoint { reply, .. } = e {
                    std::fs::write(d2.join("conception.md"), "PLAN RETOUCHE").unwrap();
                    let _ = reply.send(true);
                }
            }
        });
        run_mission(&company_checkpoint(), &agents(), "Brief", &opts, fake(p.clone(), Behavior::default()), tx, crx).await.unwrap();
        collector.await.unwrap();
        let root = p.prompt("ceo/realisation/delegate");
        assert!(root.contains("PLAN RETOUCHE") && !root.contains("travail(ceo)"), "{root}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn plan_annule_non_reprenable() {
        let dir = tmpdir();
        let cfg = Cfg { cancel_at_start: true, mission_dir: Some(dir.clone()), ..Default::default() };
        run_cfg(&company(), Arc::new(Probe::default()), Behavior::default(), cfg).await;
        assert!(crate::runtime::missions::resumable_plan(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn annulation_pendant_le_point_de_controle() {
        let p = Arc::new(Probe::default());
        // Personne ne répond ; l'annulation arrive pendant l'attente.
        let cfg = Cfg { answer: None, cancel_after_ms: Some(400), ..Default::default() };
        let (out, _) = tokio::time::timeout(Duration::from_secs(10), run_cfg(&company_checkpoint(), p.clone(), Behavior::default(), cfg)).await.expect("ne doit pas rester bloqué");
        assert_eq!(out.approved, Some(false));
        assert!(!p.started().iter().any(|id| id.contains("/realisation/")));
    }

    // ----------------------------------------------------------- modèle choisi par les délégants

    #[tokio::test]
    async fn le_delegant_choisit_le_modele_des_feuilles() {
        let p = Arc::new(Probe::default());
        let mut c = company();
        let dev2 = c.index_of("dev2").unwrap();
        c.seats[dev2].model = Some("opus".into()); // épinglé dans le .toml : le choix dynamique ne le change pas
        // Le CEO met la branche du CTO en haiku ; le manager relève dev1 en sonnet.
        let b = Behavior { models: vec![("ceo", "cto", "haiku"), ("mgr", "dev1", "sonnet")], ..Default::default() };
        let (out, evs) = run(&c, p.clone(), b).await;
        assert!(out.ok);
        let model = |id: &str| p.spec(id).agents["a"]["model"].as_str().map(String::from);
        assert_eq!(model("dev1/realisation/work").as_deref(), Some("sonnet"), "choix du manager sur sa feuille");
        assert_eq!(model("dev2/realisation/work").as_deref(), Some("opus"), "modèle épinglé du poste");
        assert_eq!(model("cto/realisation/report"), None, "un manager garde son modèle");
        assert!(p.prompt("ceo/realisation/delegate").contains("cheapest level"));
        assert!(p.system("mgr/realisation/delegate").contains("model pinned: opus"));
        assert!(evs.iter().any(|e| matches!(e, RunEvent::Label { id, label, .. } if id == "dev1/realisation/work" && label.contains("[sonnet]"))));
    }

    #[tokio::test]
    async fn modele_de_branche_herite_par_les_feuilles_sans_choix() {
        let p = Arc::new(Probe::default());
        let b = Behavior { models: vec![("ceo", "cto", "haiku")], ..Default::default() };
        run(&company(), p.clone(), b).await;
        let m = |id: &str| p.spec(id).agents["a"]["model"].as_str().map(String::from);
        assert_eq!(m("dev1/realisation/work").as_deref(), Some("haiku"));
        assert_eq!(m("dev2/realisation/work").as_deref(), Some("haiku"));
        assert_eq!(m("sec/realisation/work"), None, "autre branche : modèle par défaut de l'agent");
    }
}
