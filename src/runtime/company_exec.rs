//! Exécution d'une mission par une entreprise d'agents : délégation en cascade (boss → … → workers),
//! puis remontée de rapports synthétisés à chaque niveau. Chaque étape est un appel `claude -p`
//! indépendant ; essaim orchestre les niveaux (les sous-agents natifs ont une profondeur limitée).

use crate::tf;
use crate::tui::settings::Lang;
use crate::model::agent::AgentDef;
use crate::model::company::Company;
use crate::runtime::claude_proc::{ClaudeEvent, RunSpec};
use crate::runtime::executor::{EscalationReply, NodeJob, NodeResult, NodeStatus, RunEvent, Runner, cancelled};
use crate::runtime::{missions, scope};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinSet;

/// Un résultat d'enfant tronqué au-delà de cette taille dans le prompt de rapport du parent.
const MAX_CHILD_CHARS: usize = 6000;

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

struct Shared {
    company: Company,
    agents: Vec<AgentDef>,
    brief: String,
    opts: CompanyOptions,
    runner: Runner,
    tx: mpsc::UnboundedSender<RunEvent>,
    cancel: watch::Receiver<bool>,
    sem: Semaphore,
    /// Compteur d'appels partagé par toute la mission (plafond `max_calls`).
    calls: Arc<AtomicU32>,
    cost: Mutex<u64>,
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

/// Schéma de sortie des appels de travail et de rapport.
pub fn outcome_schema() -> String {
    json!({
        "type": "object",
        "properties": {
            "status": {"type": "string", "enum": ["done", "blocked", "failed"]},
            "report": {"type": "string"},
            "blockers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string", "enum": ["tests", "permission", "prerequisite", "bug", "scope", "other"]},
                        "detail": {"type": "string"},
                        "needs": {"type": "string"}
                    },
                    "required": ["kind", "detail"]
                }
            }
        },
        "required": ["status", "report"]
    })
    .to_string()
}

/// Lit le résultat d'un appel de travail/rapport. `None` : sortie structurée absente ou illisible.
/// Un refus de permission est toujours un blocage, même si l'agent se déclare « done ».
fn parse_outcome(r: &NodeResult) -> Option<Outcome> {
    if r.is_error {
        return Some(Outcome::failed(r.output.clone()));
    }
    let v = r.structured.as_ref()?;
    let mut status = match v["status"].as_str()? {
        "done" => Status::Done,
        "blocked" => Status::Blocked,
        "failed" => Status::Failed,
        _ => return None,
    };
    let text = |x: &Value| x.as_str().unwrap_or_default().trim().to_string();
    let mut blockers: Vec<Blocker> = v["blockers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| Blocker { kind: text(&b["kind"]).chars().take(20).collect(), detail: text(&b["detail"]), needs: text(&b["needs"]) })
        .filter(|b| !b.detail.is_empty())
        .collect();
    let mut seen: Vec<&String> = vec![];
    let mut denied: Vec<Blocker> = vec![];
    for d in &r.denials {
        if !seen.contains(&d) && denied.len() < 5 {
            seen.push(d);
            denied.push(Blocker { kind: "permission".into(), detail: format!("refused by permissions: {d}"), needs: "authorize it (allow_bash / scope) or give it to a seat that has it".into() });
        }
    }
    if !denied.is_empty() {
        blockers.extend(denied);
        if status == Status::Done {
            status = Status::Blocked;
        }
    }
    let report = text(&v["report"]);
    if status != Status::Done && blockers.is_empty() {
        let detail = report.lines().next().unwrap_or("not completed").to_string();
        blockers.push(Blocker { kind: "other".into(), detail, needs: String::new() });
    }
    Some(Outcome { status, report, blockers })
}

/// Statut affiché d'un nœud, d'après son résultat.
fn node_status(kind: Kind, r: &NodeResult) -> NodeStatus {
    if r.is_error {
        return NodeStatus::Failed(r.output.clone());
    }
    if !matches!(kind, Kind::Work | Kind::Report) {
        return NodeStatus::Succeeded;
    }
    match parse_outcome(r) {
        Some(o) if o.status == Status::Done => NodeStatus::Succeeded,
        Some(o) if o.status == Status::Blocked => NodeStatus::Blocked(o.blockers.iter().map(Blocker::line).collect::<Vec<_>>().join("\n")),
        Some(o) => NodeStatus::Failed(o.report),
        None => NodeStatus::Failed("structured result missing".into()),
    }
}

// ---------------------------------------------------------------- prompts (fonctions pures)

/// Seat card appended to the system prompt of every call. Written in English (cheaper in tokens);
/// the user's language tells the model what to write in.
pub fn role_card(lang: Lang, c: &Company, i: usize, brief: &str, phase: Phase, kids: &[usize], scope: &[String]) -> String {
    let s = &c.seats[i];
    let mut chain: Vec<String> = c.chain_of_command(i).iter().rev().map(|&p| c.seats[p].title.clone()).collect();
    chain.push(format!("{} (you)", s.title));
    let mut out = format!(
        "You hold the seat \"{}\" ({}) in the company \"{}\", under the identity of the agent \"{}\".\nChain of command: {}.\nCurrent phase: {}.\n",
        s.title,
        c.level(i),
        c.name,
        s.agent,
        chain.join(" → "),
        phase.name()
    );
    if !kids.is_empty() {
        out.push_str("Your direct subordinates (you delegate only to them):\n");
        for &k in kids {
            let ks = &c.seats[k];
            let expertise = ks.expertise.clone().unwrap_or_default();
            let tools = match (ks.builds, ks.allow_bash.is_empty()) {
                (false, _) => String::new(),
                (true, true) => " · builds".to_string(),
                (true, false) => format!(" · builds, shell: {}", ks.allow_bash.join(", ")),
            };
            out.push_str(&format!("- {} · {}{}{tools}\n", ks.id, ks.title, if expertise.is_empty() { String::new() } else { format!(" · {expertise}") }));
        }
    }
    if phase == Phase::Realisation && !scope.is_empty() {
        out.push_str(&format!("File scope assigned to you: {}.\n", scope.join(", ")));
        if s.builds {
            out.push_str("You may only create or modify files within this scope (any other write will be refused).\n");
            if !s.allow_bash.is_empty() {
                out.push_str(&format!("Allowed shell commands: {}.\n", s.allow_bash.join(", ")));
            }
        }
    }
    out.push_str(&format!("\nCompany mission (brief framed with the leader):\n{brief}\n"));
    match phase {
        Phase::Conception => out.push_str(
            "\nDESIGN PHASE: we think and prepare, we build NOTHING. Produce only analysis, specifications, decisions and recommendations for the mission (nothing is created or modified in the project yet; this is normal). Never report as a failure or a gap that the mission's result does not exist yet: it is set up later by the realisation team, after the plan is approved.\n",
        ),
        Phase::Realisation => out.push_str("\nREALISATION PHASE: the plan is approved, we now build in the project.\n"),
    }
    out.push_str(&format!(
        "\nRules: be concise and actionable; stay within your seat's remit; do not repeat the brief; clearly flag what is missing or what really failed. Never claim a success you could not verify: when you cannot complete your task, say so (status blocked) with the exact cause and what is needed, instead of working around it. The user's language is {}: write everything you produce in it.",
        lang.name()
    ));
    out
}

pub fn delegate_prompt(task: &str, phase: Phase, scope: Option<&[String]>) -> String {
    let mut out = format!(
        "Task received from your supervisor:\n{task}\n\nSplit it among your direct subordinates: involve only those who are useful, and give each a self-contained, precise task that does not overlap with the others."
    );
    if phase == Phase::Conception {
        out.push_str(" In design, give ANALYSIS or SPECIFICATION tasks (what will need doing and how), never execution tasks.");
    }
    if let Some(sc) = scope {
        out.push_str(&format!(
            "\n\nYour file scope: `{}`. For each subordinate, give a `scope`: a list of project-relative file patterns (e.g. `src/api/**`, `docs/guide.md`), ALL included in yours. Two subordinates whose scopes overlap run one after the other: prefer disjoint scopes.",
            sc.join("`, `")
        ));
    }
    out.push_str(" Answer only through the structured output.");
    out
}

pub fn work_prompt(task: &str, phase: Phase) -> String {
    let end = " Answer only through the structured output: status \"done\" ONLY if the task is really complete (and verified as far as your tools allow); otherwise \"blocked\" with `blockers` (kind, exact cause, what is needed). Never report \"done\" with a known problem.";
    match phase {
        Phase::Conception => format!("Task:\n{task}\n\nProduce your deliverable as text (analysis, specification, recommendations) in `report`. You do not have to modify the project; use \"blocked\" only if you cannot produce your analysis (missing access or information).{end}"),
        Phase::Realisation => format!("Task:\n{task}\n\nCarry it out in the project, only within your scope, verify it with the commands you are allowed (tests, build), then summarize precisely what you did in `report`.\n\nIf a tool or command is refused, a test fails or a prerequisite is missing: first fix what is within your scope; if you cannot, STOP and report it as blocked. Do not silently work around it and do not continue as if it worked.{end}"),
    }
}

/// `results`: (title, id, assigned task, success, text).
pub fn report_prompt(task: &str, results: &[(String, String, String, bool, String)]) -> String {
    let mut out = format!("Original task your supervisor gave you:\n{task}\n\nResults from your subordinates:\n");
    for (title, id, sub_task, ok, text) in results {
        let mut t = text.clone();
        if t.chars().count() > MAX_CHILD_CHARS {
            t = t.chars().take(MAX_CHILD_CHARS).collect::<String>() + "\n[… truncated]";
        }
        out.push_str(&format!(
            "\n### {title} ({id}){}\nAssigned task: {sub_task}\n{}\n",
            if *ok { "" } else { " — FAILED" },
            if *ok { t } else { format!("[FAILED: {t}]") }
        ));
    }
    out.push_str("\nWrite your summary report for your supervisor: conclusions, decisions, points of attention, missing elements. Explicitly mention any FAILURE and its impact. Answer through the structured output: status \"done\" only if every part of the task is really complete and verified (a subordinate's problem that was fixed in a later result counts as resolved); otherwise \"blocked\" with the remaining `blockers`.");
    out
}

/// Schéma JSON de la délégation : `to` restreint aux ids des subordonnés directs.
pub fn delegation_schema(kid_ids: &[String], with_scope: bool) -> String {
    let mut props = json!({
        "to": {"type": "string", "enum": kid_ids},
        "task": {"type": "string"}
    });
    let mut required = vec!["to", "task"];
    if with_scope {
        props["scope"] = json!({"type": "array", "minItems": 1, "items": {"type": "string"}});
        required.push("scope");
    }
    json!({
        "type": "object",
        "properties": {
            "assignments": {
                "type": "array",
                "minItems": 1,
                "items": {"type": "object", "properties": props, "required": required}
            }
        },
        "required": ["assignments"]
    })
    .to_string()
}

/// Schéma du plan de correction : `plan`, puis des affectations correctives ou une remontée (`escalate`).
pub fn fix_schema(kid_ids: &[String], with_scope: bool) -> String {
    let mut v: Value = serde_json::from_str(&delegation_schema(kid_ids, with_scope)).expect("schéma valide");
    v["properties"]["assignments"].as_object_mut().map(|a| a.remove("minItems"));
    v["properties"]["plan"] = json!({"type": "string"});
    v["properties"]["escalate"] = json!({"type": "string"});
    v["required"] = json!(["plan"]);
    v.to_string()
}

/// `failed`: (title, id, assigned task, ok = false, texte avec blocages).
pub fn fix_prompt(task: &str, failed: &[(String, String, String, bool, String)], scope: Option<&[String]>, decision: Option<&str>) -> String {
    let mut out = format!("Original task your supervisor gave you:\n{task}\n\nThese subordinates could NOT complete their work:\n");
    for (title, id, sub_task, _, text) in failed {
        let mut t = text.clone();
        if t.chars().count() > MAX_CHILD_CHARS {
            t = t.chars().take(MAX_CHILD_CHARS).collect::<String>() + "\n[… truncated]";
        }
        out.push_str(&format!("\n### {title} ({id})\nAssigned task: {sub_task}\n[{t}]\n"));
    }
    out.push_str("\nYou are responsible for getting this fixed, properly. Write a short correction plan in `plan` (root cause, then the steps). Then give corrective, self-contained tasks in `assignments` to the subordinate(s) best placed to fix it: possibly a different one than who failed (e.g. the one that has the missing tool or permission, or the specialist team). If nobody in your branch can fix it (a permission or tool nobody has, a prerequisite that does not exist, a decision that is not yours), assign nothing and explain precisely in `escalate` what is needed: your supervisor will take over. Do not play the problem down.");
    if let Some(sc) = scope {
        out.push_str(&format!("\n\nYour file scope: `{}`. Each assignment needs a `scope` (project-relative patterns, ALL included in yours).", sc.join("`, `")));
    }
    if let Some(d) = decision.filter(|d| !d.trim().is_empty()) {
        out.push_str(&format!("\n\nDecision from the user who requested the mission (follow it):\n{}", d.trim()));
    }
    out.push_str("\n\nAnswer only through the structured output.");
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum FixPlan {
    Assign { plan: String, assignments: Vec<Assignment> },
    /// Personne ne peut corriger dans cette branche : on remonte, avec la raison.
    Escalate(String),
}

/// Lit le plan de correction : des affectations valides priment ; sinon la remontée.
pub fn parse_fix(v: &Value, c: &Company, kids: &[usize], parent_scope: Option<&[String]>) -> FixPlan {
    let reason = v["escalate"].as_str().map(str::trim).filter(|r| !r.is_empty());
    match parse_assignments(v, c, kids, parent_scope) {
        Ok(assignments) => FixPlan::Assign { plan: v["plan"].as_str().unwrap_or_default().to_string(), assignments },
        Err(e) => FixPlan::Escalate(reason.map(String::from).unwrap_or_else(|| e.to_string())),
    }
}

/// Lit la sortie structurée : ignore les destinataires inconnus, fusionne les doublons.
/// `parent_scope` : en phase de réalisation, périmètre du délégant ; chaque `scope` enfant doit y être inclus
/// (les motifs hors périmètre sont écartés ; une affectation sans motif valide est ignorée).
pub fn parse_assignments(v: &Value, c: &Company, kids: &[usize], parent_scope: Option<&[String]>) -> Result<Vec<Assignment>> {
    let arr = v["assignments"].as_array().ok_or_else(|| anyhow::anyhow!("pas de liste « assignments »"))?;
    let mut out: Vec<Assignment> = vec![];
    for a in arr {
        let (Some(to), Some(task)) = (a["to"].as_str(), a["task"].as_str()) else { continue };
        let Some(idx) = kids.iter().copied().find(|&k| c.seats[k].id == to) else { continue };
        if task.trim().is_empty() {
            continue;
        }
        let mut sc: Vec<String> = vec![];
        if let Some(parent) = parent_scope {
            let wanted: Vec<String> = a["scope"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(scope::normalize).collect();
            // Poste ⚒ : jamais « tout le projet » (`**` = écriture sans restriction) ni chemin réservé (.claude, .git…).
            let builder = c.seats[idx].builds;
            sc = wanted
                .into_iter()
                .filter(|w| scope::all_covered(parent, std::slice::from_ref(w)))
                .filter(|w| !(builder && (scope::is_everything(w) || scope::is_sensitive(w))))
                .collect();
            sc.dedup();
            if sc.is_empty() {
                continue; // aucun motif dans le périmètre du délégant
            }
        }
        match out.iter_mut().find(|x| x.to == idx) {
            Some(x) => {
                x.task.push_str("\n\n");
                x.task.push_str(task.trim());
                for m in sc {
                    if !x.scope.contains(&m) {
                        x.scope.push(m);
                    }
                }
            }
            None => out.push(Assignment { to: idx, task: task.trim().to_string(), scope: sc }),
        }
    }
    if out.is_empty() {
        bail!("no valid delegation to a direct subordinate{}", if parent_scope.is_some() { " (with a scope included in yours)" } else { "" });
    }
    Ok(out)
}

// ---------------------------------------------------------------- moteur

impl Shared {
    /// Subordonnés mobilisables dans la phase.
    fn kids(&self, i: usize, phase: Phase) -> Vec<usize> {
        let kids = self.company.children(i);
        match phase {
            Phase::Conception => kids,
            Phase::Realisation => {
                let branch = self.company.build_branch();
                kids.into_iter().filter(|&k| branch[k]).collect()
            }
        }
    }

    fn cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    fn spec(&self, i: usize, phase: Phase, kind: Kind, prompt: String, schema: Option<String>, kids: &[usize], sc: &[String]) -> RunSpec {
        let seat = &self.company.seats[i];
        let mut agent = self.agents.iter().find(|a| a.name == seat.agent).expect("agent validé").clone();
        if seat.model.is_some() {
            agent.model = seat.model.clone();
        }
        let mut agents = BTreeMap::new();
        agents.insert(agent.name.clone(), agent.to_cli_json());
        // Délégation et rapport : raisonnement pur. Conception : lecture seule.
        // Réalisation (poste « builds ») : écriture limitée à son périmètre, tout le reste refusé (`dontAsk`).
        let mut tools = String::new();
        let mut allowed: Vec<String> = vec![];
        let mut permission_mode = None;
        match (kind, phase) {
            (Kind::Work, Phase::Conception) => tools = "Read,Grep,Glob".into(),
            (Kind::Work, Phase::Realisation) => {
                tools = "Read,Grep,Glob,Edit,Write".into();
                allowed = vec!["Read".into(), "Grep".into(), "Glob".into()];
                allowed.extend(scope::write_rules(sc));
                if !seat.allow_bash.is_empty() {
                    tools.push_str(",Bash");
                    allowed.extend(seat.allow_bash.iter().map(|c| format!("Bash({}:*)", c.trim())));
                }
                permission_mode = Some("dontAsk".to_string());
            }
            _ => {}
        }
        RunSpec {
            prompt,
            cwd: self.opts.base_dir.clone(),
            agents,
            agent: Some(agent.name.clone()),
            tools: Some(tools),
            allowed_tools: allowed,
            permission_mode,
            append_system_prompt: Some(role_card(self.opts.lang, &self.company, i, &self.brief, phase, kids, sc)),
            json_schema: schema,
            ..Default::default()
        }
    }

    /// Un appel `claude` : plafond d'appels, parallélisme borné, événements, coût.
    async fn call(self: &Arc<Self>, i: usize, phase: Phase, kind: Kind, prompt: String, schema: Option<String>, kids: &[usize], sc: &[String]) -> Result<NodeResult> {
        if self.cancelled() {
            bail!("cancelled");
        }
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.company.max_calls {
            bail!("call cap reached (max_calls = {})", self.company.max_calls);
        }
        let seat = &self.company.seats[i];
        let id = format!("{}/{}/{}", seat.id, phase.key(), kind.key());
        let verb = match (kind, phase) {
            (Kind::Delegate, _) => "delegates",
            (Kind::Work, Phase::Conception) => "analyses",
            (Kind::Work, Phase::Realisation) => "builds",
            (Kind::Report, _) => "reports",
            (Kind::Fix, _) => "fixes",
        };
        let _ = self.tx.send(RunEvent::Label {
            id: id.clone(),
            label: format!("{} · {verb}{}", seat.title, if phase == Phase::Realisation { " ⚒" } else { "" }),
            agent: seat.agent.clone(),
            depth: self.company.depth(i),
        });
        let spec = self.spec(i, phase, kind, prompt, schema, kids, sc);
        let _permit = self.sem.acquire().await?;
        if self.cancelled() {
            bail!("cancelled");
        }
        let _ = self.tx.send(RunEvent::NodeStarted { id: id.clone() });

        let (ctx, mut crx) = mpsc::unbounded_channel::<ClaudeEvent>();
        let (etx, eid) = (self.tx.clone(), id.clone());
        let fwd = tokio::spawn(async move {
            while let Some(event) = crx.recv().await {
                let _ = etx.send(RunEvent::Node { id: eid.clone(), event });
            }
        });
        let job = NodeJob { id: id.clone(), spec };
        let res = (self.runner)(job, ctx, self.cancel.clone()).await;
        let _ = fwd.await;

        let (status, used) = match &res {
            Ok(r) => (node_status(kind, r), r.tokens),
            Err(e) if self.cancelled() => {
                let _ = e;
                (NodeStatus::Cancelled, 0)
            }
            Err(e) => (NodeStatus::Failed(e.to_string()), 0),
        };
        *self.cost.lock().unwrap() += used;
        let _ = self.tx.send(RunEvent::NodeFinished { id, status, tokens: used });
        res
    }

    /// Délégation avec un nouvel essai si la sortie structurée est absente ou inutilisable.
    async fn delegate(self: &Arc<Self>, i: usize, phase: Phase, task: &str, kids: &[usize], sc: &[String]) -> Result<Vec<Assignment>> {
        let ids: Vec<String> = kids.iter().map(|&k| self.company.seats[k].id.clone()).collect();
        let real = phase == Phase::Realisation;
        let parent_scope = real.then_some(sc);
        let mut last = anyhow::anyhow!("delegation impossible");
        for _ in 0..2 {
            let r = self.call(i, phase, Kind::Delegate, delegate_prompt(task, phase, parent_scope), Some(delegation_schema(&ids, real)), kids, sc).await;
            match r {
                Ok(res) if !res.is_error => match res.structured.as_ref().map(|v| parse_assignments(v, &self.company, kids, parent_scope)) {
                    Some(Ok(a)) => return Ok(a),
                    Some(Err(e)) => last = e,
                    None => last = anyhow::anyhow!("structured output missing"),
                },
                Ok(res) => return Err(anyhow::anyhow!(res.output)),
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// Appel de travail ou de rapport : sortie structurée obligatoire, un nouvel essai si elle manque.
    async fn act(self: &Arc<Self>, i: usize, phase: Phase, kind: Kind, prompt: String, kids: &[usize], sc: &[String]) -> Outcome {
        for _ in 0..2 {
            match self.call(i, phase, kind, prompt.clone(), Some(outcome_schema()), kids, sc).await {
                Ok(r) => {
                    if let Some(o) = parse_outcome(&r) {
                        return o;
                    }
                }
                Err(e) => return Outcome::failed(e.to_string()),
            }
        }
        Outcome::failed("structured result missing")
    }

    /// Plan de correction du poste `i` pour les subordonnés en échec ; remonte si personne ne peut corriger.
    #[allow(clippy::too_many_arguments)]
    async fn fix(self: &Arc<Self>, i: usize, phase: Phase, task: &str, failed: &[(String, String, String, bool, String)], kids: &[usize], sc: &[String], decision: Option<&str>) -> FixPlan {
        let ids: Vec<String> = kids.iter().map(|&k| self.company.seats[k].id.clone()).collect();
        let real = phase == Phase::Realisation;
        let parent_scope = real.then_some(sc);
        for _ in 0..2 {
            match self.call(i, phase, Kind::Fix, fix_prompt(task, failed, parent_scope, decision), Some(fix_schema(&ids, real)), kids, sc).await {
                Ok(res) if !res.is_error => {
                    if let Some(v) = &res.structured {
                        return parse_fix(v, &self.company, kids, parent_scope);
                    }
                }
                Ok(res) => return FixPlan::Escalate(res.output),
                Err(e) => return FixPlan::Escalate(e.to_string()),
            }
        }
        FixPlan::Escalate("structured output missing".into())
    }

    /// Le CEO n'a pas pu régler le blocage : pause, l'utilisateur décide (relancer, arrêter, ou en discuter d'abord).
    async fn ask_user(self: &Arc<Self>, summary: String) -> EscalationReply {
        if self.cancelled() || self.calls.load(Ordering::SeqCst) >= self.company.max_calls {
            return EscalationReply::Stop;
        }
        let (rtx, mut rrx) = mpsc::unbounded_channel::<EscalationReply>();
        let _ = self.tx.send(RunEvent::Escalation { summary, reply: rtx });
        let mut cancel = self.cancel.clone();
        tokio::select! {
            r = rrx.recv() => r.unwrap_or(EscalationReply::Stop),
            _ = cancelled(&mut cancel) => EscalationReply::Stop,
        }
    }

    /// Décision écrite par l'utilisateur pendant la discussion avec le CEO.
    fn resolution(&self) -> Option<String> {
        let d = self.opts.mission_dir.as_ref()?;
        std::fs::read_to_string(d.join(missions::RESOLUTION_FILE)).ok().filter(|t| !t.trim().is_empty())
    }
}

/// Exécute des affectations : en conception tout en parallèle ; en réalisation par lots sans chevauchement de
/// fichiers, l'un après l'autre. Un lot qui échoue arrête les suivants (on ne construit pas sur une base cassée).
/// Renvoie les affectations menées (avec leur résultat) et celles jamais démarrées.
async fn run_batch(sh: &Arc<Shared>, phase: Phase, todo: Vec<Assignment>) -> (Vec<(Assignment, SeatOut)>, Vec<Assignment>) {
    let lots: Vec<Vec<usize>> = match phase {
        Phase::Conception => vec![(0..todo.len()).collect()],
        Phase::Realisation => scope::batches(&todo.iter().map(|a| a.scope.clone()).collect::<Vec<_>>()),
    };
    let mut outs: Vec<Option<SeatOut>> = (0..todo.len()).map(|_| None).collect();
    let mut started = vec![false; todo.len()];
    for lot in lots {
        if sh.cancelled() {
            break;
        }
        let mut set: JoinSet<(usize, SeatOut)> = JoinSet::new();
        for &n in &lot {
            started[n] = true;
            let a = &todo[n];
            let (sh2, to, t, s2) = (sh.clone(), a.to, a.task.clone(), a.scope.clone());
            set.spawn(async move { (n, run_seat(sh2, to, phase, t, s2).await) });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((n, out)) = joined {
                outs[n] = Some(out);
            }
        }
        if lot.iter().any(|&n| outs[n].as_ref().is_none_or(|o| !o.ok())) {
            break;
        }
    }
    let (mut ran, mut pending) = (vec![], vec![]);
    for ((a, out), was_started) in todo.into_iter().zip(outs).zip(started) {
        match (out, was_started) {
            (Some(o), _) => ran.push((a, o)),
            (None, true) => ran.push((a, SeatOut::failed("the node was interrupted"))),
            (None, false) => pending.push(a),
        }
    }
    (ran, pending)
}

/// Rangée du rapport d'un supérieur : (titre, id, tâche, ok, texte avec blocages).
fn result_row(sh: &Shared, a: &Assignment, out: &SeatOut) -> (String, String, String, bool, String) {
    let s = &sh.company.seats[a.to];
    (s.title.clone(), s.id.clone(), a.task.clone(), out.ok(), out.for_supervisor())
}

/// Texte de l'escalade vers l'utilisateur : ce qui bloque, par poste, et pourquoi le CEO ne peut pas régler.
pub fn escalation_summary(problems: &[(String, String)], reason: Option<&str>) -> String {
    let mut out = tf!("Le CEO n'a pas pu lever ces blocages :", "The CEO could not clear these blockers:").to_string();
    for (title, text) in problems {
        out.push_str(&format!("\n\n### {title}\n{text}"));
    }
    if let Some(r) = reason.filter(|r| !r.trim().is_empty()) {
        out.push_str(&format!("\n\n{}\n{}", tf!("Raison de la remontée :", "Why it was escalated:"), r.trim()));
    }
    out
}

fn run_seat(sh: Arc<Shared>, i: usize, phase: Phase, task: String, sc: Vec<String>) -> Pin<Box<dyn Future<Output = SeatOut> + Send>> {
    Box::pin(async move {
        let kids = sh.kids(i, phase);

        // Feuille : travail direct.
        if kids.is_empty() {
            return sh.act(i, phase, Kind::Work, work_prompt(&task, phase), &kids, &sc).await.into();
        }

        // Poste interne : délégation → travail des subordonnés → (correction des échecs) → rapport de synthèse.
        let assignments = match sh.delegate(i, phase, &task, &kids, &sc).await {
            Ok(a) => a,
            Err(e) => return SeatOut::failed(format!("delegation impossible: {e}")),
        };
        let is_root = sh.company.root() == Some(i);
        let mut history: Vec<(String, String, String, bool, String)> = vec![];
        let mut todo = assignments;
        let mut rounds_left = sh.company.max_fix_rounds;
        let mut decision: Option<String> = None;
        let mut reason: Option<String> = None;
        // Problèmes non résolus à la fin : (titre, texte avec blocages, blocages).
        let mut unresolved: Vec<(String, String, Vec<Blocker>)> = vec![];
        loop {
            let (ran, pending) = run_batch(&sh, phase, todo).await;
            let mut failed_rows = vec![];
            unresolved.clear();
            for (a, out) in &ran {
                let row = result_row(&sh, a, out);
                if !out.ok() {
                    let title = row.0.clone();
                    unresolved.push((title.clone(), row.4.clone(), out.blockers.iter().map(|b| Blocker { detail: format!("{title}: {}", b.detail), ..b.clone() }).collect()));
                    failed_rows.push(row.clone());
                }
                history.push(row);
            }
            for a in &pending {
                let title = sh.company.seats[a.to].title.clone();
                let msg = "not started: a previous batch is blocked".to_string();
                unresolved.push((title.clone(), msg.clone(), vec![Blocker { kind: "prerequisite".into(), detail: format!("{title}: {msg}"), needs: String::new() }]));
            }
            // Résolution : correctifs dans la branche, puis remontée ; le CEO en dernier recours s'adresse à l'utilisateur.
            let next = loop {
                if unresolved.is_empty() || sh.cancelled() {
                    break None;
                }
                if rounds_left > 0 {
                    rounds_left -= 1;
                    match sh.fix(i, phase, &task, &failed_rows, &kids, &sc, decision.as_deref()).await {
                        FixPlan::Assign { assignments, .. } => {
                            let mut next = assignments;
                            next.extend(pending.iter().cloned());
                            break Some(next);
                        }
                        FixPlan::Escalate(r) => {
                            reason = Some(r);
                            rounds_left = 0;
                        }
                    }
                } else if is_root {
                    let shown: Vec<(String, String)> = unresolved.iter().map(|(t, x, _)| (t.clone(), x.clone())).collect();
                    match sh.ask_user(escalation_summary(&shown, reason.as_deref())).await {
                        EscalationReply::Retry => {
                            rounds_left = sh.company.max_fix_rounds.max(1);
                            decision = sh.resolution();
                        }
                        EscalationReply::Stop => break None,
                    }
                } else {
                    break None;
                }
            };
            match next {
                Some(n) => todo = n,
                None => break,
            }
        }

        let rep = sh.act(i, phase, Kind::Report, report_prompt(&task, &history), &kids, &sc).await;
        let mut blockers: Vec<Blocker> = unresolved.iter().flat_map(|(_, _, b)| b.clone()).collect();
        if unresolved.is_empty() && rep.status == Status::Done {
            return SeatOut { status: Status::Done, text: rep.report, blockers: vec![] };
        }
        blockers.extend(rep.blockers);
        let status = if unresolved.is_empty() && rep.status == Status::Failed { Status::Failed } else { Status::Blocked };
        SeatOut { status, text: rep.report, blockers }
    })
}

/// Lance une phase complète depuis le boss. `calls` est le compteur d'appels de toute la mission.
#[allow(clippy::too_many_arguments)]
pub async fn run_phase(
    company: &Company,
    agents: &[AgentDef],
    brief: &str,
    phase: Phase,
    root_task: &str,
    opts: &CompanyOptions,
    runner: Runner,
    tx: mpsc::UnboundedSender<RunEvent>,
    cancel: watch::Receiver<bool>,
    calls: Arc<AtomicU32>,
) -> Result<PhaseOutcome> {
    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    company.validate(&names)?;
    let Some(root) = company.root() else { bail!("org chart without a single boss") };
    let before = calls.load(Ordering::SeqCst);
    let sh = Arc::new(Shared {
        company: company.clone(),
        agents: agents.to_vec(),
        brief: brief.to_string(),
        opts: opts.clone(),
        runner,
        tx,
        cancel,
        sem: Semaphore::new(company.max_parallel.max(1)),
        calls,
        cost: Mutex::new(0),
    });
    let root_scope = if phase == Phase::Realisation { vec!["**".to_string()] } else { vec![] };
    let out = run_seat(sh.clone(), root, phase, root_task.to_string(), root_scope).await;
    let cost = *sh.cost.lock().unwrap();
    let used = sh.calls.load(Ordering::SeqCst).min(company.max_calls).saturating_sub(before);
    let ok = out.ok();
    Ok(PhaseOutcome { ok, report: out.text, blockers: if ok { vec![] } else { out.blockers }, tokens: cost, calls: used })
}

const CONCEPTION_TASK: &str = "Lead the design phase of the mission described in the brief: have each domain framed by the right lead.";

fn realisation_task(plan: &str) -> String {
    format!("Here is the consolidated plan, approved at the end of design:\n{plan}\n\nLead its setup in the project: split the work across your realisation branch.")
}

/// Note affichée quand la réalisation est refusée au point de contrôle : le run est « terminé » (code 0)
/// sans aucune modification du projet ; sans cette note, l'écran ne le distingue pas d'une réalisation réussie.
pub fn note_refus(approved: Option<bool>) -> Option<String> {
    (approved == Some(false)).then(|| {
        tf!(
            "réalisation non lancée : plan refusé au point de contrôle, aucun fichier du projet modifié",
            "build not started: plan refused at the checkpoint, no project file changed"
        )
    })
}

/// Mission complète : conception → (point de contrôle) → réalisation. `RunFinished` clôt le run.
pub async fn run_mission(
    company: &Company,
    agents: &[AgentDef],
    brief: &str,
    opts: &CompanyOptions,
    runner: Runner,
    tx: mpsc::UnboundedSender<RunEvent>,
    mut cancel: watch::Receiver<bool>,
) -> Result<MissionOutcome> {
    let calls = Arc::new(AtomicU32::new(0));
    if let Some(d) = &opts.mission_dir {
        missions::mark_done(d, false); // un run neuf invalide l'ancien succès
    }
    let save = |name: &str, text: &str| {
        if let Some(d) = &opts.mission_dir {
            let _ = missions::write_output(d, name, text);
        }
    };

    let resumed_plan = if opts.resume { opts.mission_dir.as_deref().and_then(missions::resumable_plan) } else { None };
    let conception = if let Some(plan) = resumed_plan {
        PhaseOutcome { ok: true, report: plan, blockers: vec![], tokens: 0, calls: 0 }
    } else {
        if let Some(d) = &opts.mission_dir {
            missions::clear_conception_mark(d); // un run neuf invalide l'ancien plan
        }
        let c = match run_phase(company, agents, brief, Phase::Conception, CONCEPTION_TASK, opts, runner.clone(), tx.clone(), cancel.clone(), calls.clone()).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(RunEvent::RunFinished { ok: false, tokens: 0 });
                return Err(e);
            }
        };
        save("conception.md", &c.report);
        if c.ok && !*cancel.borrow() {
            if let Some(d) = &opts.mission_dir {
                let _ = missions::mark_conception_done(d);
            }
        }
        c
    };

    let mut outcome = MissionOutcome {
        ok: conception.ok,
        report: conception.report.clone(),
        tokens: conception.tokens,
        calls: conception.calls,
        conception: conception.clone(),
        realisation: None,
        approved: None,
    };

    let has_builders = company.seats.iter().any(|s| s.builds);
    let cancelled_now = *cancel.borrow();
    // Conception non réussie : on ne construit jamais sur une base bloquée (l'utilisateur a déjà pu intervenir).
    if has_builders && !cancelled_now && conception.ok {
        // Point de contrôle : le plan est validé avant de toucher au projet.
        let approved = if company.checkpoint {
            let (rtx, mut rrx) = mpsc::unbounded_channel::<bool>();
            let _ = tx.send(RunEvent::Checkpoint { plan: conception.report.clone(), reply: rtx });
            tokio::select! {
                r = rrx.recv() => r.unwrap_or(false),
                _ = cancelled(&mut cancel) => false,
            }
        } else {
            true
        };
        outcome.approved = Some(approved);
        if approved {
            // Le plan peut avoir été retouché pendant le point de contrôle (conversation avec le boss) : le fichier fait foi.
            let plan = opts
                .mission_dir
                .as_ref()
                .and_then(|d| std::fs::read_to_string(d.join("conception.md")).ok())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| conception.report.clone());
            let task = realisation_task(&plan);
            match run_phase(company, agents, brief, Phase::Realisation, &task, opts, runner, tx.clone(), cancel.clone(), calls).await {
                Ok(r) => {
                    save("rapport.md", &r.report);
                    outcome.ok = conception.ok && r.ok;
                    outcome.report = r.report.clone();
                    outcome.tokens += r.tokens;
                    outcome.calls += r.calls;
                    outcome.realisation = Some(r);
                }
                Err(e) => {
                    outcome.ok = false;
                    let _ = tx.send(RunEvent::RunFinished { ok: false, tokens: outcome.tokens });
                    return Err(e);
                }
            }
        } else {
            // Réalisation refusée : la mission s'arrête sur le plan, sans que ce soit un échec.
            outcome.ok = conception.ok;
        }
    }
    // Réussie = menée à terme : conception ok, et réalisation faite si l'entreprise en a une (un plan refusé n'est pas un succès).
    if let Some(d) = &opts.mission_dir {
        missions::mark_done(d, outcome.ok && (!has_builders || outcome.realisation.is_some()));
    }
    let _ = tx.send(RunEvent::RunFinished { ok: outcome.ok, tokens: outcome.tokens });
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
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
