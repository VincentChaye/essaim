//! Exécution d'une mission par une entreprise d'agents : délégation en cascade (boss → … → workers),
//! puis remontée de rapports synthétisés à chaque niveau. Chaque étape est un appel `claude -p`
//! indépendant ; essaim orchestre les niveaux (les sous-agents natifs ont une profondeur limitée).

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
            Phase::Conception => "design",
            Phase::Realisation => "realisation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Delegate,
    Work,
    Report,
    /// Plan de correction après l'échec ou le blocage de subordonnés.
    Fix,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Delegate => "delegate",
            Kind::Work => "work",
            Kind::Report => "report",
            Kind::Fix => "fix",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompanyOptions {
    /// Dossier du projet : répertoire de travail de tous les appels.
    pub base_dir: PathBuf,
    /// Dossier de la mission : reçoit `conception.md` et `rapport.md`.
    pub mission_dir: Option<PathBuf>,
    /// Langue choisie par l'utilisateur : annoncée aux modèles (les consignes restent en anglais).
    pub lang: Lang,
    /// Reprend une mission interrompue : une conception déjà menée à terme n'est pas rejouée, on repart de son plan.
    pub resume: bool,
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

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub to: usize,
    pub task: String,
    /// Périmètre de fichiers (phase de réalisation uniquement).
    pub scope: Vec<String>,
}

/// Ce qui empêche une tâche d'aboutir : le supérieur le lit pour corriger ou rediriger.
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
}

impl Outcome {
    fn failed(msg: impl Into<String>) -> Self {
        let msg = msg.into();
        let detail = msg.lines().next().unwrap_or("").trim().to_string();
        Outcome { status: Status::Failed, blockers: vec![Blocker { kind: "other".into(), detail, needs: String::new() }], report: msg }
    }
}

struct SeatOut {
    status: Status,
    text: String,
    blockers: Vec<Blocker>,
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
        SeatOut { status: o.status, text: o.report, blockers: o.blockers }
    }
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
    use crate::runtime::executor::{EscalationReply, NodeResult, NodeStatus, RunEvent, Runner};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::{mpsc, watch};
    use std::time::Duration;

    const SRC: &str = r#"
name = "acme"
max_parallel = 4

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
allow_bash = ["cargo test"]
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
    }

    #[derive(Default, Clone)]
    struct Behavior {
        fail: Vec<&'static str>,
        /// seat → sous-ensemble de subordonnés à qui déléguer (sinon tous).
        only: Vec<(&'static str, Vec<&'static str>)>,
        bad_delegation: Vec<&'static str>,
        /// Tous les subordonnés reçoivent le périmètre de leur supérieur (chevauchement total).
        overlap: bool,
        /// Ces postes attribuent un périmètre hors du leur.
        escape: Vec<&'static str>,
        /// Ces appels se déclarent « blocked » à chaque fois.
        blocked: Vec<&'static str>,
        /// Ces appels se déclarent « blocked » à leur premier passage seulement.
        blocked_once: Vec<&'static str>,
        /// Ces appels disent « done » mais ont subi un refus de permission.
        deny: Vec<&'static str>,
        /// Ces postes réassignent la correction à tous leurs subordonnés (sinon : ils remontent).
        fix_assign: Vec<&'static str>,
    }

    fn enum_ids(schema: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(schema).unwrap();
        v["properties"]["assignments"]["items"]["properties"]["to"]["enum"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()
    }

    /// Périmètre du délégant, lu dans son prompt (« Ton périmètre de fichiers : `a`, `b`. »).
    fn parent_scope(prompt: &str) -> Vec<String> {
        let Some(rest) = prompt.split("Your file scope: `").nth(1) else { return vec![] };
        let list = rest.split("`. ").next().unwrap_or("");
        list.split("`, `").map(String::from).collect()
    }

    fn fake(p: Arc<Probe>, b: Behavior) -> Runner {
        Arc::new(move |job, _tx, _c| {
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
                let (seat, kind) = {
                    let parts: Vec<&str> = job.id.split('/').collect();
                    (parts[0].to_string(), parts[2].to_string())
                };
                if b.fail.contains(&job.id.as_str()) {
                    return Ok(NodeResult { output: "boum".into(), is_error: true, ..Default::default() });
                }
                Ok(match kind.as_str() {
                    "delegate" if b.bad_delegation.contains(&seat.as_str()) => NodeResult {
                        structured: Some(json!({"assignments": [{"to": "inconnu", "task": "x", "scope": ["src/x/**"]}]})),
                        output: "{}".into(),
                        ..Default::default()
                    },
                    "delegate" => {
                        let schema = job.spec.json_schema.as_deref().unwrap();
                        let ids = enum_ids(schema);
                        let with_scope = schema.contains("\"scope\"");
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
                                if with_scope {
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
                        NodeResult { structured: Some(json!({"assignments": items})), output: "{}".into(), tokens: 0, ..Default::default() }
                    }
                    "work" | "report" => {
                        let n = {
                            let mut seen = p.seen.lock().unwrap();
                            let c = seen.entry(job.id.clone()).or_insert(0);
                            *c += 1;
                            *c
                        };
                        let text = if kind == "work" { format!("travail({seat})") } else { format!("rapport({seat})") };
                        let blocked = b.blocked.contains(&job.id.as_str()) || (n == 1 && b.blocked_once.contains(&job.id.as_str()));
                        let structured = if blocked {
                            json!({"status": "blocked", "report": text, "blockers": [{"kind": "tests", "detail": format!("tests rouges chez {seat}"), "needs": "corriger"}]})
                        } else {
                            json!({"status": "done", "report": text, "blockers": []})
                        };
                        let denials = if b.deny.contains(&job.id.as_str()) { vec!["Bash(git status)".to_string()] } else { vec![] };
                        NodeResult { structured: Some(structured), output: text, denials, ..Default::default() }
                    }
                    "fix" if b.fix_assign.contains(&seat.as_str()) => {
                        let schema = job.spec.json_schema.as_deref().unwrap();
                        let with_scope = schema.contains("\"scope\"");
                        let parent = parent_scope(&job.spec.prompt);
                        let dir = parent.first().map(|s| s.trim_end_matches("/**").to_string()).filter(|s| s != "**").unwrap_or_else(|| "src".into());
                        let items: Vec<Value> = enum_ids(schema)
                            .iter()
                            .map(|k| {
                                let mut a = json!({"to": k, "task": format!("correctif pour {k}")});
                                if with_scope {
                                    a["scope"] = json!([format!("{dir}/{k}/**")]);
                                }
                                a
                            })
                            .collect();
                        NodeResult { structured: Some(json!({"plan": "plan de correction", "assignments": items})), output: "{}".into(), ..Default::default() }
                    }
                    _ => NodeResult { structured: Some(json!({"plan": "personne ne peut corriger ici", "escalate": "aucun poste compétent dans cette branche"})), output: "{}".into(), ..Default::default() },
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
        /// Réponses successives aux escalades vers l'utilisateur (défaut : arrêter).
        escalations: Vec<EscalationReply>,
    }

    async fn run_cfg(c: &Company, p: Arc<Probe>, b: Behavior, cfg: Cfg) -> (MissionOutcome, Vec<RunEvent>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (ctx, crx) = watch::channel(cfg.cancel_at_start);
        let events = Arc::new(Mutex::new(Vec::<RunEvent>::new()));
        let ev2 = events.clone();
        let answer = cfg.answer;
        let mut script = cfg.escalations.clone().into_iter();
        let collector = tokio::spawn(async move {
            while let Some(e) = rx.recv().await {
                if let (RunEvent::Checkpoint { reply, .. }, Some(a)) = (&e, answer) {
                    let _ = reply.send(a);
                }
                if let RunEvent::Escalation { reply, .. } = &e {
                    let _ = reply.send(script.next().unwrap_or(EscalationReply::Stop));
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
        let opts = CompanyOptions { base_dir: std::env::temp_dir(), mission_dir: cfg.mission_dir.clone(), lang: Lang::Fr, resume: cfg.resume };
        let out = run_mission(c, &agents(), "Brief de test", &opts, fake(p, b), tx, crx).await.unwrap();
        collector.await.unwrap();
        drop(ctx);
        let evs = std::mem::take(&mut *events.lock().unwrap());
        (out, evs)
    }

    async fn run(c: &Company, p: Arc<Probe>, b: Behavior, cancel: bool) -> (MissionOutcome, Vec<RunEvent>) {
        run_cfg(c, p, b, Cfg { cancel_at_start: cancel, answer: Some(true), ..Default::default() }).await
    }

    /// Organigramme sans poste de réalisation : conception seule.
    fn company() -> Company {
        let mut c = Company::parse(SRC).unwrap();
        for s in &mut c.seats {
            s.builds = false;
            s.allow_bash.clear();
        }
        c
    }

    /// Organigramme avec dev1/dev2 en réalisation, sans point de contrôle.
    fn company_build() -> Company {
        let mut c = Company::parse(SRC).unwrap();
        c.checkpoint = false;
        c
    }

    fn tmpdir() -> PathBuf {
        std::env::temp_dir().join(format!("essaim-mission-{}", uuid::Uuid::new_v4()))
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

    /// R1 : un livrable qui ne peut pas être écrit n'est plus avalé : le run se ferme en échec et l'erreur remonte.
    #[tokio::test]
    async fn echec_d_ecriture_du_livrable_ferme_le_run_en_echec() {
        let base = tmpdir();
        std::fs::create_dir_all(&base).unwrap();
        let bloque = base.join("bloque");
        std::fs::write(&bloque, "x").unwrap(); // un fichier à la place d'un dossier : la création du dossier de mission échoue
        let opts = CompanyOptions { base_dir: base.clone(), mission_dir: Some(bloque.join("mission")), lang: Lang::Fr, resume: false };
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

    // ----------------------------------------------------------- conception

    #[tokio::test]
    async fn cascade_complete_remontee_des_rapports() {
        let p = Arc::new(Probe::default());
        let (out, evs) = run(&company(), p.clone(), Behavior::default(), false).await;
        assert!(out.ok);
        assert_eq!(out.report, "rapport(ceo)");
        // 4 postes internes × 2 appels + 3 feuilles = 11 (= estimation de conception).
        assert_eq!(p.started.lock().unwrap().len(), 11);
        assert_eq!(out.calls, 11);
        assert_eq!(out.tokens, 0);
        assert!(out.realisation.is_none() && out.approved.is_none(), "pas de poste de réalisation");
        assert!(!evs.iter().any(|e| matches!(e, RunEvent::Checkpoint { .. })));
        assert!(matches!(evs.last(), Some(RunEvent::RunFinished { ok: true, .. })));
    }

    #[tokio::test]
    async fn ordre_haut_en_bas_puis_bas_en_haut() {
        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior::default(), false).await;
        let started = p.started.lock().unwrap().clone();
        let finished = p.finished.lock().unwrap().clone();
        let s = |id: &str| started.iter().position(|x| x == id).unwrap_or_else(|| panic!("{id} jamais lancé : {started:?}"));
        let f = |id: &str| finished.iter().position(|x| x == id).unwrap();
        assert!(f("ceo/conception/delegate") < s("cto/conception/delegate"));
        assert!(f("cto/conception/delegate") < s("mgr/conception/delegate"));
        assert!(f("mgr/conception/delegate") < s("dev1/conception/work"));
        assert!(f("dev1/conception/work") < s("mgr/conception/report"));
        assert!(f("dev2/conception/work") < s("mgr/conception/report"));
        assert!(f("mgr/conception/report") < s("cto/conception/report"));
        assert!(f("cto/conception/report") < s("ceo/conception/report"));
        assert!(f("sec/conception/work") < s("ciso/conception/report"));
        assert!(f("ciso/conception/report") < s("ceo/conception/report"));
    }

    #[tokio::test]
    async fn le_rapport_contient_les_resultats_des_subordonnes() {
        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior::default(), false).await;
        let prompts = p.prompts.lock().unwrap();
        let ceo_report = &prompts.iter().find(|(id, _, _)| id == "ceo/conception/report").unwrap().1;
        assert!(ceo_report.contains("rapport(cto)") && ceo_report.contains("rapport(ciso)"));
        assert!(ceo_report.contains("task for cto"));
        let mgr_report = &prompts.iter().find(|(id, _, _)| id == "mgr/conception/report").unwrap().1;
        assert!(mgr_report.contains("travail(dev1)") && mgr_report.contains("travail(dev2)"));
        let sys = &prompts.iter().find(|(id, _, _)| id == "mgr/conception/delegate").unwrap().2;
        assert!(sys.contains("Engineering Manager") && sys.contains("N-2") && sys.contains("CEO → CTO → Engineering Manager (you)"));
        assert!(sys.contains("dev1 · Backend Engineer") && sys.contains("Brief de test"));
    }

    #[tokio::test]
    async fn la_conception_interdit_de_traiter_l_absence_de_resultat_comme_un_echec() {
        let p = Arc::new(Probe::default());
        run(&company_build(), p.clone(), Behavior::default(), false).await;
        let prompts = p.prompts.lock().unwrap();
        let (_, prompt, sys) = prompts.iter().find(|(id, _, _)| id == "dev1/conception/work").unwrap();
        assert!(sys.contains("DESIGN PHASE") && sys.contains("build NOTHING") && sys.contains("Never report as a failure"));
        assert!(prompt.contains("You do not have to modify the project"));
        let (_, dprompt, _) = prompts.iter().find(|(id, _, _)| id == "ceo/conception/delegate").unwrap();
        assert!(dprompt.contains("ANALYSIS or SPECIFICATION"));
        let (_, rprompt, rsys) = prompts.iter().find(|(id, _, _)| id == "dev1/realisation/work").unwrap();
        assert!(rsys.contains("REALISATION PHASE") && !rsys.contains("DESIGN PHASE"));
        assert!(rprompt.contains("Carry it out"));
        let (_, rdprompt, _) = prompts.iter().find(|(id, _, _)| id == "ceo/realisation/delegate").unwrap();
        assert!(!rdprompt.contains("ANALYSIS or SPECIFICATION"));
    }

    #[tokio::test]
    async fn parallelisme_borne_par_max_parallel() {
        let mut c = company();
        c.max_parallel = 1;
        let p = Arc::new(Probe::default());
        run(&c, p.clone(), Behavior::default(), false).await;
        assert_eq!(p.peak.load(Ordering::SeqCst), 1);

        let mut c = company();
        c.max_parallel = 4;
        let p = Arc::new(Probe::default());
        run(&c, p.clone(), Behavior::default(), false).await;
        let peak = p.peak.load(Ordering::SeqCst);
        assert!(peak > 1 && peak <= 4, "pic = {peak}");
    }

    #[tokio::test]
    async fn conception_en_lecture_seule_sans_outils_pour_les_managers() {
        let p = Arc::new(Probe::default());
        run(&company(), p.clone(), Behavior::default(), false).await;
        let specs = p.specs.lock().unwrap();
        let spec = |id: &str| specs.iter().find(|(i, _)| i == id).unwrap().1.clone();
        assert_eq!(spec("dev1/conception/work").tools.as_deref(), Some("Read,Grep,Glob"));
        assert_eq!(spec("mgr/conception/delegate").tools.as_deref(), Some(""));
        assert_eq!(spec("mgr/conception/report").tools.as_deref(), Some(""));
        assert!(spec("dev1/conception/work").allowed_tools.is_empty() && spec("dev1/conception/work").permission_mode.is_none());
    }

    #[tokio::test]
    async fn echec_d_un_worker_escalade_au_manager() {
        let p = Arc::new(Probe::default());
        let b = Behavior { fail: vec!["dev1/conception/work"], ..Default::default() };
        let (out, _) = run(&company(), p.clone(), b, false).await;
        assert!(!out.ok);
        assert_eq!(out.report, "rapport(ceo)", "le boss produit quand même son rapport");
        let prompts = p.prompts.lock().unwrap();
        let mgr = &prompts.iter().find(|(id, _, _)| id == "mgr/conception/report").unwrap().1;
        assert!(mgr.contains("FAILED") && mgr.contains("boum"), "{mgr}");
    }

    #[tokio::test]
    async fn delegation_partielle_ne_mobilise_que_les_choisis() {
        let p = Arc::new(Probe::default());
        let b = Behavior { only: vec![("ceo", vec!["cto"])], ..Default::default() };
        let (out, _) = run(&company(), p.clone(), b, false).await;
        assert!(out.ok);
        let started = p.started.lock().unwrap().clone();
        assert!(!started.iter().any(|id| id.starts_with("ciso/") || id.starts_with("sec/")), "{started:?}");
        assert_eq!(started.len(), 2 + 2 + 2 + 1 + 1, "ceo, cto, mgr (2 appels chacun) + dev1, dev2 (1 chacun)");
    }

    #[tokio::test]
    async fn delegation_invalide_nouvel_essai_puis_echec_escalade() {
        let p = Arc::new(Probe::default());
        let b = Behavior { bad_delegation: vec!["mgr"], ..Default::default() };
        let (out, _) = run(&company(), p.clone(), b, false).await;
        assert!(!out.ok);
        let started = p.started.lock().unwrap().clone();
        assert_eq!(started.iter().filter(|id| *id == "mgr/conception/delegate").count(), 2, "un nouvel essai");
        assert!(!started.iter().any(|id| id.starts_with("dev")), "personne n'a reçu de tâche");
        let prompts = p.prompts.lock().unwrap();
        let cto = &prompts.iter().find(|(id, _, _)| id == "cto/conception/report").unwrap().1;
        assert!(cto.contains("delegation impossible"), "{cto}");
    }

    #[tokio::test]
    async fn plafond_d_appels() {
        let mut c = company();
        c.max_calls = 4;
        let p = Arc::new(Probe::default());
        let (out, _) = run(&c, p.clone(), Behavior::default(), false).await;
        assert!(!out.ok);
        assert_eq!(p.started.lock().unwrap().len(), 4, "jamais plus que max_calls");
        assert_eq!(out.calls, 4);
    }

    #[tokio::test]
    async fn annulation_avant_depart() {
        let p = Arc::new(Probe::default());
        let (out, evs) = run(&company(), p.clone(), Behavior::default(), true).await;
        assert!(!out.ok);
        assert!(p.started.lock().unwrap().is_empty());
        assert!(matches!(evs.last(), Some(RunEvent::RunFinished { ok: false, .. })));
    }

    #[tokio::test]
    async fn evenements_etiquetes_et_indentes() {
        let p = Arc::new(Probe::default());
        let (_, evs) = run(&company(), p, Behavior::default(), false).await;
        let labels: Vec<(String, usize)> = evs
            .iter()
            .filter_map(|e| match e {
                RunEvent::Label { label, depth, .. } => Some((label.clone(), *depth)),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&("CEO · delegates".to_string(), 0)));
        assert!(labels.contains(&("Engineering Manager · reports".to_string(), 2)));
        assert!(labels.contains(&("Backend Engineer · analyses".to_string(), 3)));
    }

    // ----------------------------------------------------------- réalisation

    #[tokio::test]
    async fn realisation_limitee_a_la_branche_des_builders() {
        let p = Arc::new(Probe::default());
        let (out, evs) = run(&company_build(), p.clone(), Behavior::default(), false).await;
        assert!(out.ok, "{:?}", out.report);
        let started = p.started.lock().unwrap().clone();
        let real: Vec<&String> = started.iter().filter(|id| id.contains("/realisation/")).collect();
        // ceo, cto, mgr (2 appels) + dev1, dev2 (1) = 8 ; ciso et sec ne participent pas.
        assert_eq!(real.len(), 8, "{real:?}");
        assert!(!real.iter().any(|id| id.starts_with("ciso/") || id.starts_with("sec/")));
        assert!(real.iter().any(|id| *id == "dev1/realisation/work") && real.iter().any(|id| *id == "dev2/realisation/work"));
        assert_eq!(out.calls, 11 + 8);
        assert!(out.realisation.is_some());
        assert!(!evs.iter().any(|e| matches!(e, RunEvent::Checkpoint { .. })), "checkpoint = false");
    }

    #[tokio::test]
    async fn builders_verrouilles_sur_leur_perimetre() {
        let p = Arc::new(Probe::default());
        run(&company_build(), p.clone(), Behavior::default(), false).await;
        let specs = p.specs.lock().unwrap();
        let spec = |id: &str| specs.iter().find(|(i, _)| i == id).unwrap().1.clone();
        let d1 = spec("dev1/realisation/work");
        assert_eq!(d1.permission_mode.as_deref(), Some("dontAsk"), "tout ce qui n'est pas autorisé est refusé");
        assert!(d1.allowed_tools.contains(&"Edit(src/cto/mgr/dev1/**)".to_string()), "{:?}", d1.allowed_tools);
        assert!(d1.allowed_tools.contains(&"Write(src/cto/mgr/dev1/**)".to_string()));
        assert!(!d1.allowed_tools.iter().any(|r| r == "Edit" || r == "Write"), "jamais d'écriture sans restriction");
        assert_eq!(d1.tools.as_deref(), Some("Read,Grep,Glob,Edit,Write,Bash"));
        assert!(d1.allowed_tools.contains(&"Bash(cargo test:*)".to_string()));
        let d2 = spec("dev2/realisation/work");
        assert_eq!(d2.tools.as_deref(), Some("Read,Grep,Glob,Edit,Write"), "pas de shell sans allow_bash");
        assert!(!d2.allowed_tools.iter().any(|r| r.starts_with("Bash")));
        assert!(d2.allowed_tools.contains(&"Edit(src/cto/mgr/dev2/**)".to_string()));
        // Les managers ne touchent à rien.
        assert_eq!(spec("mgr/realisation/delegate").tools.as_deref(), Some(""));
        assert!(spec("mgr/realisation/report").allowed_tools.is_empty());
        // La fiche de poste rappelle le périmètre et les commandes.
        let sys = p.prompts.lock().unwrap().iter().find(|(id, _, _)| id == "dev1/realisation/work").unwrap().2.clone();
        assert!(sys.contains("File scope assigned to you: src/cto/mgr/dev1/**"));
        assert!(sys.contains("Allowed shell commands: cargo test"));
    }

    #[tokio::test]
    async fn perimetres_disjoints_en_parallele_chevauchement_en_sequence() {
        let pos = |t: &Vec<String>, ev: &str| t.iter().position(|x| x == ev).unwrap_or_else(|| panic!("{ev} absent de {t:?}"));

        // Disjoints : dev2 démarre avant que dev1 ait fini.
        let p = Arc::new(Probe::default());
        run(&company_build(), p.clone(), Behavior::default(), false).await;
        let t = p.timeline.lock().unwrap().clone();
        assert!(pos(&t, "S:dev2/realisation/work") < pos(&t, "E:dev1/realisation/work"), "parallèle : {t:?}");

        // Chevauchants : l'un finit avant que l'autre ne commence.
        let p = Arc::new(Probe::default());
        let b = Behavior { overlap: true, ..Default::default() };
        run(&company_build(), p.clone(), b, false).await;
        let t = p.timeline.lock().unwrap().clone();
        let seq = pos(&t, "E:dev1/realisation/work") < pos(&t, "S:dev2/realisation/work") || pos(&t, "E:dev2/realisation/work") < pos(&t, "S:dev1/realisation/work");
        assert!(seq, "séquentiel : {t:?}");
    }

    #[tokio::test]
    async fn perimetre_hors_de_celui_du_delegant_refuse() {
        let p = Arc::new(Probe::default());
        let b = Behavior { escape: vec!["mgr"], ..Default::default() };
        let (out, _) = run(&company_build(), p.clone(), b, false).await;
        assert!(!out.ok);
        let started = p.started.lock().unwrap().clone();
        assert_eq!(started.iter().filter(|id| *id == "mgr/realisation/delegate").count(), 2, "un nouvel essai");
        assert!(!started.iter().any(|id| id.starts_with("dev") && id.contains("realisation")), "personne n'écrit hors périmètre");
        let prompts = p.prompts.lock().unwrap();
        let cto = &prompts.iter().find(|(id, _, _)| id == "cto/realisation/report").unwrap().1;
        assert!(cto.contains("delegation impossible"), "{cto}");
    }

    #[tokio::test]
    async fn point_de_controle_accepte_ecrit_les_livrables() {
        let dir = tmpdir();
        let mut c = company_build();
        c.checkpoint = true;
        let p = Arc::new(Probe::default());
        let cfg = Cfg { answer: Some(true), mission_dir: Some(dir.clone()), ..Default::default() };
        let (out, evs) = run_cfg(&c, p.clone(), Behavior::default(), cfg).await;
        assert_eq!(out.approved, Some(true));
        let plan = evs.iter().find_map(|e| match e { RunEvent::Checkpoint { plan, .. } => Some(plan.clone()), _ => None }).expect("point de contrôle");
        assert_eq!(plan, "rapport(ceo)");
        assert_eq!(std::fs::read_to_string(dir.join("conception.md")).unwrap(), "rapport(ceo)");
        assert_eq!(std::fs::read_to_string(dir.join("rapport.md")).unwrap(), "rapport(ceo)");
        // Le plan validé est transmis au boss pour la réalisation.
        let prompts = p.prompts.lock().unwrap();
        let root = &prompts.iter().find(|(id, _, _)| id == "ceo/realisation/delegate").unwrap().1;
        assert!(root.contains("consolidated plan, approved") && root.contains("rapport(ceo)"));
    }

    #[tokio::test]
    async fn point_de_controle_refuse_arrete_sur_le_plan() {
        let dir = tmpdir();
        let mut c = company_build();
        c.checkpoint = true;
        let p = Arc::new(Probe::default());
        let cfg = Cfg { answer: Some(false), mission_dir: Some(dir.clone()), ..Default::default() };
        let (out, evs) = run_cfg(&c, p.clone(), Behavior::default(), cfg).await;
        assert_eq!(out.approved, Some(false));
        assert!(out.ok, "refuser n'est pas un échec");
        assert!(out.realisation.is_none());
        assert!(!p.started.lock().unwrap().iter().any(|id| id.contains("/realisation/")));
        assert!(dir.join("conception.md").is_file() && !dir.join("rapport.md").exists());
        assert!(matches!(evs.last(), Some(RunEvent::RunFinished { ok: true, .. })));
    }

    #[tokio::test]
    async fn reprise_saute_la_conception_terminee() {
        let dir = tmpdir();
        let mut c = company_build();
        c.checkpoint = true;
        // 1er run : conception menée à terme, réalisation refusée (= run interrompu avant la réalisation).
        let cfg = Cfg { answer: Some(false), mission_dir: Some(dir.clone()), ..Default::default() };
        run_cfg(&c, Arc::new(Probe::default()), Behavior::default(), cfg).await;
        assert!(crate::runtime::missions::resumable_plan(&dir).is_some());
        // Reprise : aucun nœud de conception, le plan validé alimente la réalisation.
        let p = Arc::new(Probe::default());
        let cfg = Cfg { answer: Some(true), mission_dir: Some(dir.clone()), resume: true, ..Default::default() };
        let (out, _) = run_cfg(&c, p.clone(), Behavior::default(), cfg).await;
        assert!(out.ok && out.approved == Some(true) && out.realisation.is_some());
        let started = p.started.lock().unwrap();
        assert!(!started.iter().any(|id| id.contains("/conception/")), "{started:?}");
        assert!(started.iter().any(|id| id.contains("/realisation/")));
        drop(started);
        assert!(dir.join("rapport.md").is_file());
        // Sans --resume, la conception est rejouée.
        let p = Arc::new(Probe::default());
        let cfg = Cfg { answer: Some(false), mission_dir: Some(dir.clone()), ..Default::default() };
        run_cfg(&c, p.clone(), Behavior::default(), cfg).await;
        assert!(p.started.lock().unwrap().iter().any(|id| id.contains("/conception/")));
    }

    #[tokio::test]
    async fn plan_retouche_pendant_le_point_de_controle_est_celui_de_la_realisation() {
        let dir = tmpdir();
        let mut c = company_build();
        c.checkpoint = true;
        let p = Arc::new(Probe::default());
        let cfg = Cfg { answer: None, mission_dir: Some(dir.clone()), ..Default::default() };
        // Personne ne répond automatiquement : on retouche le plan puis on valide, comme la TUI.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (_ctx, crx) = watch::channel(false);
        let opts = CompanyOptions { base_dir: std::env::temp_dir(), mission_dir: cfg.mission_dir.clone(), lang: Lang::Fr, resume: false };
        let d2 = dir.clone();
        let collector = tokio::spawn(async move {
            while let Some(e) = rx.recv().await {
                if let RunEvent::Checkpoint { reply, .. } = e {
                    std::fs::write(d2.join("conception.md"), "PLAN RETOUCHE").unwrap();
                    let _ = reply.send(true);
                }
            }
        });
        run_mission(&c, &agents(), "Brief", &opts, fake(p.clone(), Behavior::default()), tx, crx).await.unwrap();
        collector.await.unwrap();
        let prompts = p.prompts.lock().unwrap();
        let root = &prompts.iter().find(|(id, _, _)| id == "ceo/realisation/delegate").unwrap().1;
        assert!(root.contains("PLAN RETOUCHE") && !root.contains("rapport(ceo)"), "{root}");
    }

    #[tokio::test]
    async fn conception_annulee_non_reprenable() {
        let dir = tmpdir();
        let c = company_build();
        let cfg = Cfg { cancel_at_start: true, answer: Some(true), mission_dir: Some(dir.clone()), ..Default::default() };
        run_cfg(&c, Arc::new(Probe::default()), Behavior::default(), cfg).await;
        assert!(crate::runtime::missions::resumable_plan(&dir).is_none());
    }

    #[tokio::test]
    async fn annulation_pendant_le_point_de_controle() {
        let mut c = company_build();
        c.checkpoint = true;
        let p = Arc::new(Probe::default());
        // Personne ne répond ; l'annulation arrive pendant l'attente.
        let cfg = Cfg { answer: None, cancel_after_ms: Some(400), ..Default::default() };
        let (out, _) = tokio::time::timeout(Duration::from_secs(10), run_cfg(&c, p.clone(), Behavior::default(), cfg)).await.expect("ne doit pas rester bloqué");
        assert_eq!(out.approved, Some(false));
        assert!(!p.started.lock().unwrap().iter().any(|id| id.contains("/realisation/")));
    }

    #[tokio::test]
    async fn plafond_d_appels_partage_entre_les_deux_phases() {
        let mut c = company_build();
        c.max_calls = 14; // 11 en conception, il n'en reste que 3 pour la réalisation
        let p = Arc::new(Probe::default());
        let (out, _) = run(&c, p.clone(), Behavior::default(), false).await;
        assert_eq!(p.started.lock().unwrap().len(), 14);
        assert!(!out.ok);
        assert_eq!(out.calls, 14);
    }

    // ----------------------------------------------------------- blocages : correction et escalade

    #[tokio::test]
    async fn blocage_corrige_par_le_manager_la_phase_reussit() {
        let p = Arc::new(Probe::default());
        let b = Behavior { blocked_once: vec!["dev1/conception/work"], fix_assign: vec!["mgr"], ..Default::default() };
        let (out, evs) = run(&company(), p.clone(), b, false).await;
        assert!(out.ok, "le correctif a réussi : {:?}", out.conception.blockers);
        let started = p.started.lock().unwrap().clone();
        assert_eq!(started.iter().filter(|id| *id == "mgr/conception/fix").count(), 1);
        assert_eq!(started.iter().filter(|id| *id == "dev1/conception/work").count(), 2, "relancé une fois");
        assert!(evs.iter().any(|e| matches!(e, RunEvent::NodeFinished { id, status: NodeStatus::Blocked(_), .. } if id == "dev1/conception/work")));
        let prompts = p.prompts.lock().unwrap();
        let fix = &prompts.iter().find(|(id, _, _)| id == "mgr/conception/fix").unwrap().1;
        assert!(fix.contains("could NOT complete") && fix.contains("tests rouges chez dev1") && fix.contains("correction plan"), "{fix}");
        let rep = &prompts.iter().find(|(id, _, _)| id == "mgr/conception/report").unwrap().1;
        assert!(rep.contains("correctif pour dev1") || rep.contains("travail(dev1)"), "{rep}");
    }

    #[tokio::test]
    async fn blocage_non_reglable_remonte_au_superieur_qui_corrige() {
        let p = Arc::new(Probe::default());
        // mgr ne peut rien faire (il remonte) ; cto prend la main et réassigne.
        let b = Behavior { blocked: vec!["dev1/conception/work"], fix_assign: vec!["cto"], ..Default::default() };
        let (out, _) = run(&company(), p.clone(), b, false).await;
        assert!(!out.ok, "dev1 reste bloqué malgré les correctifs");
        let started = p.started.lock().unwrap().clone();
        assert!(started.iter().any(|id| id == "mgr/conception/fix"), "le manager essaie d'abord");
        assert!(started.iter().any(|id| id == "cto/conception/fix"), "puis le supérieur reçoit le blocage");
        let prompts = p.prompts.lock().unwrap();
        let cto = &prompts.iter().find(|(id, _, _)| id == "cto/conception/fix").unwrap().1;
        assert!(cto.contains("Engineering Manager") && cto.contains("tests rouges chez dev1"), "{cto}");
    }

    #[tokio::test]
    async fn ceo_bloque_previent_l_utilisateur_et_s_arrete_sur_stop() {
        let p = Arc::new(Probe::default());
        let b = Behavior { blocked: vec!["dev1/conception/work"], ..Default::default() };
        let (out, evs) = run(&company(), p.clone(), b, false).await;
        assert!(!out.ok);
        let summary = evs.iter().find_map(|e| match e { RunEvent::Escalation { summary, .. } => Some(summary.clone()), _ => None }).expect("escalade vers l'utilisateur");
        assert!(summary.contains("tests rouges chez dev1"), "{summary}");
        assert!(!out.conception.blockers.is_empty() && out.conception.blockers.iter().any(|b| b.kind == "tests"));
        assert!(matches!(evs.last(), Some(RunEvent::RunFinished { ok: false, .. })));
    }

    #[tokio::test]
    async fn relance_apres_discussion_injecte_la_decision_de_l_utilisateur() {
        let dir = tmpdir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolution.md"), "DECISION: autoriser git").unwrap();
        let p = Arc::new(Probe::default());
        let b = Behavior { blocked: vec!["dev1/conception/work"], ..Default::default() };
        let cfg = Cfg { answer: Some(true), mission_dir: Some(dir), escalations: vec![EscalationReply::Retry], ..Default::default() };
        let (out, evs) = run_cfg(&company(), p.clone(), b, cfg).await;
        assert!(!out.ok);
        assert_eq!(evs.iter().filter(|e| matches!(e, RunEvent::Escalation { .. })).count(), 2, "relance puis nouveau blocage");
        let prompts = p.prompts.lock().unwrap();
        let fixes: Vec<&String> = prompts.iter().filter(|(id, _, _)| id == "ceo/conception/fix").map(|(_, pr, _)| pr).collect();
        assert!(fixes.len() >= 2 && !fixes[0].contains("DECISION") && fixes.iter().skip(1).all(|f| f.contains("DECISION: autoriser git")), "{fixes:?}");
    }

    #[tokio::test]
    async fn refus_de_permission_est_un_blocage_meme_si_l_agent_dit_done() {
        let p = Arc::new(Probe::default());
        let b = Behavior { deny: vec!["dev1/conception/work"], ..Default::default() };
        let (out, evs) = run(&company(), p.clone(), b, false).await;
        assert!(!out.ok);
        let blocked = evs.iter().find_map(|e| match e { RunEvent::NodeFinished { id, status: NodeStatus::Blocked(m), .. } if id == "dev1/conception/work" => Some(m.clone()), _ => None });
        assert!(blocked.is_some_and(|m| m.contains("[permission]") && m.contains("git status")));
    }

    #[tokio::test]
    async fn lot_bloque_les_suivants_ne_demarrent_pas() {
        let p = Arc::new(Probe::default());
        let b = Behavior { overlap: true, blocked: vec!["dev1/realisation/work", "dev2/realisation/work"], ..Default::default() };
        let (out, _) = run(&company_build(), p.clone(), b, false).await;
        assert!(!out.ok);
        let started = p.started.lock().unwrap().clone();
        let n = started.iter().filter(|id| *id == "dev1/realisation/work" || *id == "dev2/realisation/work").count();
        assert_eq!(n, 1, "le second lot n'est pas lancé sur une base bloquée : {started:?}");
    }

    #[tokio::test]
    async fn conception_bloquee_ne_lance_pas_la_realisation() {
        let p = Arc::new(Probe::default());
        let b = Behavior { blocked: vec!["dev1/conception/work"], ..Default::default() };
        let (out, _) = run(&company_build(), p.clone(), b, false).await;
        assert!(!out.ok && out.realisation.is_none() && out.approved.is_none());
        assert!(!p.started.lock().unwrap().iter().any(|id| id.contains("/realisation/")));
    }

    #[tokio::test]
    async fn tours_de_correction_bornes_par_max_fix_rounds() {
        let mut c = company();
        c.max_fix_rounds = 1;
        let p = Arc::new(Probe::default());
        let b = Behavior { blocked: vec!["dev1/conception/work"], fix_assign: vec!["mgr"], ..Default::default() };
        let (out, _) = run(&c, p.clone(), b, false).await;
        assert!(!out.ok);
        let started = p.started.lock().unwrap().clone();
        assert_eq!(started.iter().filter(|id| *id == "mgr/conception/fix").count(), 1);
        assert_eq!(started.iter().filter(|id| *id == "dev1/conception/work").count(), 2);
    }

    #[test]
    fn lecture_du_plan_de_correction() {
        let c = Company::parse(SRC).unwrap();
        let kids = c.children(c.index_of("ceo").unwrap());
        let ok = json!({"plan": "p", "assignments": [{"to": "cto", "task": "corriger"}]});
        assert!(matches!(parse_fix(&ok, &c, &kids, None), FixPlan::Assign { .. }));
        let esc = json!({"plan": "p", "escalate": "il manque un droit"});
        assert_eq!(parse_fix(&esc, &c, &kids, None), FixPlan::Escalate("il manque un droit".into()));
        assert!(matches!(parse_fix(&json!({"plan": "p"}), &c, &kids, None), FixPlan::Escalate(_)), "rien à assigner : on remonte");
    }

    #[test]
    fn sortie_sans_statut_ou_refus_jamais_un_succes() {
        let r = |structured: Option<Value>, denials: Vec<String>| NodeResult { structured, denials, output: "txt".into(), ..Default::default() };
        assert!(parse_outcome(&r(None, vec![])).is_none(), "sortie structurée absente");
        assert!(parse_outcome(&r(Some(json!({"status": "oups", "report": "x"})), vec![])).is_none());
        let o = parse_outcome(&r(Some(json!({"status": "done", "report": "ok"})), vec!["Bash(git push)".into()])).unwrap();
        assert_eq!(o.status, Status::Blocked);
        assert_eq!(o.blockers[0].kind, "permission");
        let o = parse_outcome(&r(Some(json!({"status": "blocked", "report": "KO"})), vec![])).unwrap();
        assert_eq!(o.blockers.len(), 1, "un blocage est toujours expliqué");
        assert_eq!(parse_outcome(&NodeResult { is_error: true, output: "boum".into(), ..Default::default() }).unwrap().status, Status::Failed);
    }

    // ----------------------------------------------------------- briques pures

    #[test]
    fn schema_restreint_aux_subordonnes_directs() {
        let c = Company::parse(SRC).unwrap();
        let kids = c.children(c.index_of("ceo").unwrap());
        let ids: Vec<String> = kids.iter().map(|&k| c.seats[k].id.clone()).collect();
        let v: Value = serde_json::from_str(&delegation_schema(&ids, false)).unwrap();
        assert_eq!(v["properties"]["assignments"]["items"]["properties"]["to"]["enum"], json!(["cto", "ciso"]));
        assert!(v["properties"]["assignments"]["items"]["properties"].get("scope").is_none());
        let v: Value = serde_json::from_str(&delegation_schema(&ids, true)).unwrap();
        assert!(v["properties"]["assignments"]["items"]["required"].as_array().unwrap().contains(&json!("scope")));
    }

    #[test]
    fn lecture_des_affectations() {
        let c = Company::parse(SRC).unwrap();
        let kids = c.children(c.index_of("ceo").unwrap());
        let v = json!({"assignments": [
            {"to": "cto", "task": "A"}, {"to": "cto", "task": "B"}, {"to": "dev1", "task": "hors hiérarchie"},
            {"to": "ciso", "task": "  "}, {"to": 3, "task": "x"}
        ]});
        let a = parse_assignments(&v, &c, &kids, None).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].task, "A\n\nB", "doublons fusionnés, inconnus/vides ignorés");
        assert!(parse_assignments(&json!({"assignments": []}), &c, &kids, None).is_err());
        assert!(parse_assignments(&json!({}), &c, &kids, None).is_err());
    }

    #[test]
    fn lecture_des_perimetres() {
        let c = Company::parse(SRC).unwrap();
        let kids = c.children(c.index_of("ceo").unwrap());
        let parent = vec!["src/**".to_string()];
        let v = json!({"assignments": [
            {"to": "cto", "task": "A", "scope": ["./src/api/**", "docs/x.md", "src/ui/**"]},
            {"to": "ciso", "task": "B", "scope": ["docs/**"]},
            {"to": "ciso", "task": "C"}
        ]});
        let a = parse_assignments(&v, &c, &kids, Some(&parent)).unwrap();
        assert_eq!(a.len(), 1, "ciso n'a aucun motif valide : ignoré");
        assert_eq!(a[0].scope, vec!["src/api/**", "src/ui/**"], "docs/x.md hors du périmètre du délégant est écarté");
        assert!(parse_assignments(&json!({"assignments": [{"to": "cto", "task": "A", "scope": ["docs/**"]}]}), &c, &kids, Some(&parent)).is_err());
        let total = vec!["**".to_string()];
        assert!(parse_assignments(&json!({"assignments": [{"to": "cto", "task": "A", "scope": ["../secret/**"]}]}), &c, &kids, Some(&total)).is_err(), "remontée hors du projet refusée, même sous un parent total");
    }

    #[test]
    fn poste_de_realisation_sans_tout_le_projet_ni_chemin_reserve() {
        let c = Company::parse(SRC).unwrap();
        let kids_mgr = c.children(c.index_of("mgr").unwrap());
        let total = vec!["**".to_string()];
        // dev1 est un poste ⚒ : « tout le projet » (écriture sans restriction) est refusé.
        let v = json!({"assignments": [{"to": "dev1", "task": "A", "scope": ["**"]}]});
        assert!(parse_assignments(&v, &c, &kids_mgr, Some(&total)).is_err());
        // Chemins réservés (.claude, .git) écartés ; le reste du périmètre est conservé.
        let v = json!({"assignments": [{"to": "dev1", "task": "A", "scope": [".claude/**", ".git/hooks/**", "src/dev1/**"]}]});
        let a = parse_assignments(&v, &c, &kids_mgr, Some(&total)).unwrap();
        assert_eq!(a[0].scope, vec!["src/dev1/**"]);
        // Un poste qui n'écrit pas (manager) garde son périmètre tel quel.
        let kids_cto = c.children(c.index_of("cto").unwrap());
        let v = json!({"assignments": [{"to": "mgr", "task": "A", "scope": ["**"]}]});
        assert_eq!(parse_assignments(&v, &c, &kids_cto, Some(&total)).unwrap()[0].scope, vec!["**"]);
    }

    #[test]
    fn refus_de_realisation_signale() {
        assert!(note_refus(Some(false)).is_some(), "refus : le message doit apparaître");
        assert!(note_refus(Some(true)).is_none() && note_refus(None).is_none());
    }

    #[test]
    fn rapport_tronque_les_gros_resultats() {
        let big = "x".repeat(MAX_CHILD_CHARS + 500);
        let p = report_prompt("t", &[("CTO".into(), "cto".into(), "sous-tâche".into(), true, big)]);
        assert!(p.contains("[… truncated]"));
        assert!(p.len() < MAX_CHILD_CHARS + 1500);
    }
}
