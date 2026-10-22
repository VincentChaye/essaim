//! État partagé d'une phase (`Shared`) et appels vers `claude` : un poste, un appel, un plafond, un coût.

use super::prompt::{answer_prompt, delegate_prompt, fix_prompt, permission_prompt, review_prompt, role_card};
use super::schema::{Decision, FixPlan, ReviewPlan, answer_schema, parse_decision, permission_schema, delegation_schema, fix_schema, node_status, outcome_schema, parse_answer, parse_assignments, parse_fix, parse_outcome, parse_review, review_schema};
use super::{Assignment, CompanyOptions, Kind, MAX_QUESTIONS, Outcome, Phase, Section};
use crate::model::agent::AgentDef;
use crate::model::company::Company;
use crate::runtime::claude_proc::{ClaudeEvent, RunSpec};
use crate::runtime::executor::{EscalationReply, NodeJob, NodeResult, NodeStatus, RunEvent, Runner, cancelled};
use crate::runtime::bashrisk::is_risky;
use crate::runtime::{missions, scope};
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::{Semaphore, mpsc, watch};

/// Verrou du compteur de coût. Un verrou empoisonné (panique d'un autre fil) garde une valeur entière valide :
/// on la reprend au lieu de paniquer.
pub(super) fn cost_guard(cost: &Mutex<u64>) -> MutexGuard<'_, u64> {
    cost.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Envoi d'un événement vers l'interface. Un récepteur fermé signifie que l'interface a quitté : l'événement
/// n'a alors plus de destinataire, l'ignorer est voulu.
pub(super) fn emit(tx: &mpsc::UnboundedSender<RunEvent>, ev: RunEvent) {
    let _ = tx.send(ev);
}

/// Nom du sous-agent léger d'un poste dont `helpers` est défini.
pub(super) const HELPER: &str = "essaim-assistant";

/// Reprise d'une session `claude` ; `allow` : règles accordées pour CE seul appel (commandes à risque approuvées une fois).
#[derive(Debug, Clone)]
pub(super) struct Resume {
    pub(super) session: String,
    pub(super) allow: Vec<String>,
}

/// Commandes shell refusées par les permissions, lues dans les libellés « Bash(cmd) ». Un libellé tronqué est écarté
/// (on n'autorise jamais une commande qu'on ne connaît pas en entier).
fn denied_bash(denials: &[String]) -> Vec<String> {
    denials.iter().filter_map(|d| d.strip_prefix("Bash(")?.strip_suffix(')')).filter(|c| c.chars().count() < 80).map(String::from).collect()
}

pub(super) struct Shared {
    pub(super) company: Company,
    pub(super) agents: Vec<AgentDef>,
    pub(super) brief: String,
    pub(super) opts: CompanyOptions,
    pub(super) runner: Runner,
    pub(super) tx: mpsc::UnboundedSender<RunEvent>,
    pub(super) cancel: watch::Receiver<bool>,
    pub(super) sem: Semaphore,
    /// Compteur d'appels partagé par toute la mission (plafond `max_calls`).
    pub(super) calls: Arc<AtomicU32>,
    pub(super) cost: Mutex<u64>,
    /// Modèle choisi par les délégants : pour une feuille son propre modèle, sinon le défaut des feuilles de sa branche.
    pub(super) hints: Mutex<BTreeMap<usize, String>>,
    /// Règles Bash accordées durablement (commandes sûres) à un poste par son supérieur, pour la durée de la mission.
    pub(super) grants: Mutex<BTreeMap<usize, Vec<String>>>,
}

impl Shared {
    /// Subordonnés mobilisables dans la phase.
    pub(super) fn kids(&self, i: usize, phase: Phase) -> Vec<usize> {
        let kids = self.company.children(i);
        match phase {
            Phase::Conception => kids,
            Phase::Realisation => {
                let branch = self.company.build_branch();
                kids.into_iter().filter(|&k| branch[k]).collect()
            }
        }
    }

    /// Enregistre le modèle d'une affectation (le sien, sinon celui hérité du délégant `from`).
    pub(super) fn set_hint(&self, from: usize, a: &Assignment) {
        let mut h = self.hints.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(m) = a.model.clone().or_else(|| h.get(&from).cloned()) {
            h.insert(a.to, m);
        }
    }

    /// Modèle imposé par un délégant à la feuille `i` ; un modèle fixé dans le `.toml` du poste prime.
    fn dynamic_model(&self, i: usize, is_leaf: bool) -> Option<String> {
        if !is_leaf || self.company.seats[i].model.is_some() {
            return None;
        }
        self.hints.lock().unwrap_or_else(PoisonError::into_inner).get(&i).cloned()
    }

    pub(super) fn cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    fn spec(&self, i: usize, phase: Phase, kind: Kind, prompt: String, schema: Option<String>, kids: &[usize], sc: &[String], extra_allow: &[String]) -> Result<RunSpec> {
        let seat = &self.company.seats[i];
        let mut agent = self.agents.iter().find(|a| a.name == seat.agent).cloned().ok_or_else(|| anyhow::anyhow!("agent « {} » introuvable", seat.agent))?;
        if seat.model.is_some() {
            agent.model = seat.model.clone();
        } else if let Some(m) = self.dynamic_model(i, kids.is_empty()) {
            agent.model = Some(m);
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
                    // Une commande qui modifie le système n'est jamais permanente : elle repasse par le supérieur à chaque usage.
                    allowed.extend(seat.allow_bash.iter().filter(|c| !is_risky(c)).map(|c| format!("Bash({}:*)", c.trim())));
                }
                permission_mode = Some("dontAsk".to_string());
            }
            // Supérieur qui rapporte, corrige ou revoit : il lit et vérifie (commandes de lecture / de test), il n'écrit jamais.
            (Kind::Report | Kind::Fix | Kind::Review | Kind::Answer, _) if !kids.is_empty() => {
                tools = "Read,Grep,Glob,Bash".into();
                allowed = vec!["Read".into(), "Grep".into(), "Glob".into()];
                allowed.extend(self.company.verify_commands(i).iter().map(|c| format!("Bash({}:*)", c.trim())));
                permission_mode = Some("dontAsk".to_string());
            }
            _ => {}
        }
        // Autorisations accordées par le supérieur : durables (commandes sûres) puis celles de cet appel seul.
        let mut extra: Vec<String> = self.grants.lock().unwrap_or_else(PoisonError::into_inner).get(&i).cloned().unwrap_or_default();
        extra.extend(extra_allow.iter().cloned());
        if kind == Kind::Work && !extra.is_empty() {
            if !tools.split(',').any(|t| t == "Bash") {
                tools.push_str(if tools.is_empty() { "Bash" } else { ",Bash" });
            }
            for r in ["Read", "Grep", "Glob"] {
                if !allowed.iter().any(|a| a == r) {
                    allowed.push(r.into());
                }
            }
            allowed.extend(extra);
            permission_mode = Some("dontAsk".to_string());
        }
        // Assistants légers (modèle moindre) : tâches mécaniques sans décision. Ils n'écrivent jamais ;
        // ils ne reçoivent le shell que si le poste l'a (un sous-agent ne dépasse pas les outils de son parent).
        if kind == Kind::Work {
            if let Some(model) = &seat.helpers {
                let mut helper_tools = vec!["Read", "Grep", "Glob"];
                if tools.split(',').any(|t| t == "Bash") {
                    helper_tools.push("Bash");
                }
                agents.insert(
                    HELPER.into(),
                    serde_json::json!({
                        "description": "Light assistant for purely mechanical, fully specified tasks (inventories, running a command and reporting its raw output, counting, extracting). Takes no decision, never edits files.",
                        "prompt": "You are a light assistant. Do exactly what is asked, nothing more. Report raw results, concisely. Take no decision, never modify any file, and say so plainly if the task is unclear or fails.",
                        "model": model,
                        "tools": helper_tools,
                    }),
                );
                tools.push_str(if tools.is_empty() { "Agent" } else { ",Agent" });
            }
        }
        Ok(RunSpec {
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
        })
    }

    /// Un appel `claude` : plafond d'appels, parallélisme borné, événements, coût.
    async fn call(self: &Arc<Self>, i: usize, phase: Phase, kind: Kind, prompt: String, schema: Option<String>, kids: &[usize], sc: &[String], sections: &[Section], resume: Option<Resume>) -> Result<NodeResult> {
        if self.cancelled() {
            bail!("cancelled");
        }
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.company.max_calls {
            bail!("call cap reached (max_calls = {})", self.company.max_calls);
        }
        let seat = &self.company.seats[i];
        // Un appel de reprise ou de réponse est un nœud à part (sinon il écraserait le coût affiché du premier).
        let id = if resume.is_some() || kind == Kind::Answer { format!("{}/{}/{}#{n}", seat.id, phase.key(), kind.key()) } else { format!("{}/{}/{}", seat.id, phase.key(), kind.key()) };
        let verb = match (kind, phase) {
            (Kind::Work, _) if resume.is_some() => "resumes",
            (Kind::Delegate, _) => "delegates",
            (Kind::Work, Phase::Conception) => "analyses",
            (Kind::Work, Phase::Realisation) => "builds",
            (Kind::Report, _) => "reports",
            (Kind::Fix, _) => "fixes",
            (Kind::Review, _) => "reviews",
            (Kind::Answer, _) => "answers",
        };
        emit(&self.tx, RunEvent::Label {
            id: id.clone(),
            label: format!(
                "{} · {verb}{}{}",
                seat.title,
                if phase == Phase::Realisation { " ⚒" } else { "" },
                self.dynamic_model(i, kids.is_empty()).map(|m| format!(" [{m}]")).unwrap_or_default()
            ),
            agent: seat.agent.clone(),
            depth: self.company.depth(i),
        });
        let extra = resume.as_ref().map(|r| r.allow.clone()).unwrap_or_default();
        let mut spec = self.spec(i, phase, kind, prompt, schema, kids, sc, &extra)?;
        spec.resume = resume.map(|r| r.session);
        let _permit = self.sem.acquire().await?;
        if self.cancelled() {
            bail!("cancelled");
        }
        emit(&self.tx, RunEvent::NodeStarted { id: id.clone() });

        let (ctx, mut crx) = mpsc::unbounded_channel::<ClaudeEvent>();
        let (etx, eid) = (self.tx.clone(), id.clone());
        let fwd = tokio::spawn(async move {
            while let Some(event) = crx.recv().await {
                emit(&etx, RunEvent::Node { id: eid.clone(), event });
            }
        });
        let job = NodeJob { id: id.clone(), spec };
        let res = (self.runner)(job, ctx, self.cancel.clone()).await;
        // Jointure du relais d'événements : son erreur (panique du relais) n'altère ni le résultat ni le coût.
        let _ = fwd.await;

        let (status, used) = match &res {
            Ok(r) => (node_status(kind, r, sections), r.tokens),
            Err(_) if self.cancelled() => (NodeStatus::Cancelled, 0),
            Err(e) => (NodeStatus::Failed(e.to_string()), 0),
        };
        *cost_guard(&self.cost) += used;
        emit(&self.tx, RunEvent::NodeFinished { id, status, tokens: used });
        res
    }

    /// Délégation avec un nouvel essai si la sortie structurée est absente ou inutilisable.
    pub(super) async fn delegate(self: &Arc<Self>, i: usize, phase: Phase, task: &str, kids: &[usize], sc: &[String]) -> Result<Vec<Assignment>> {
        let ids: Vec<String> = kids.iter().map(|&k| self.company.seats[k].id.clone()).collect();
        let real = phase == Phase::Realisation;
        let parent_scope = real.then_some(sc);
        let mut last = anyhow::anyhow!("delegation impossible");
        for _ in 0..2 {
            let r = self.call(i, phase, Kind::Delegate, delegate_prompt(task, phase, parent_scope), Some(delegation_schema(&ids, real)), kids, sc, &[], None).await;
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
    /// Un employé (Kind::Work) peut interrompre sa tâche `MAX_QUESTIONS` fois, soit pour une question cadrée, soit pour
    /// une commande shell qu'il n'a pas le droit de lancer (refus de permission ou demande explicite). Son supérieur
    /// répond (appel court, schéma borné) puis l'employé reprend sa session (`--resume`) là où il s'était arrêté.
    pub(super) async fn act(self: &Arc<Self>, i: usize, phase: Phase, kind: Kind, prompt: String, kids: &[usize], sc: &[String], sections: &[Section]) -> Outcome {
        let (mut asks, mut tries) = (if kind == Kind::Work { MAX_QUESTIONS } else { 0 }, 0);
        let (first, mut prompt, mut resume) = (prompt.clone(), prompt, None::<Resume>);
        while tries < 2 {
            match self.call(i, phase, kind, prompt.clone(), Some(outcome_schema(sections, asks > 0)), kids, sc, sections, resume.clone()).await {
                Ok(r) => {
                    let Some(o) = parse_outcome(&r, sections) else {
                        tries += 1;
                        continue;
                    };
                    let Some(sid) = r.session_id.clone().filter(|_| asks > 0) else { return o };
                    // Commandes shell voulues : demande explicite, puis refus subis pendant l'appel.
                    let mut wanted: Vec<String> = o.question.iter().filter(|q| q.kind == "permission" && !q.command.is_empty()).map(|q| q.command.clone()).collect();
                    wanted.extend(denied_bash(&r.denials));
                    wanted.dedup();
                    wanted.truncate(3);
                    if !wanted.is_empty() {
                        asks -= 1;
                        let (approved, once) = self.authorize_all(i, phase, &first, &wanted).await;
                        if approved.is_empty() {
                            return o; // refusé : le blocage remonte par le circuit ordinaire
                        }
                        let (what, risky) = (approved.join("; "), if once.is_empty() { "" } else { " Commands that modify the system were approved for this single run only: if you need them again, ask again." });
                        prompt = format!("Your supervisor approved: {what}.{risky} Run only what you need and finish your task.");
                        resume = Some(Resume { session: sid, allow: once });
                        continue;
                    }
                    let Some(q) = &o.question else { return o };
                    asks -= 1;
                    match self.answer_question(i, phase, &first, q).await {
                        Some(a) => {
                            prompt = format!("Answer from your supervisor: {a}\n\nContinue and finish your task.");
                            resume = Some(Resume { session: sid, allow: vec![] });
                        }
                        None => return o, // personne ne peut répondre : la question remonte comme un blocage
                    }
                }
                Err(e) => return Outcome::failed(e.to_string()),
            }
        }
        Outcome::failed("structured result missing")
    }

    /// Soumet chaque commande à la chaîne hiérarchique. Renvoie les commandes approuvées et les règles à usage unique
    /// (commandes à risque) ; les commandes sûres sont accordées au poste pour le reste de la mission.
    async fn authorize_all(self: &Arc<Self>, i: usize, phase: Phase, task_prompt: &str, cmds: &[String]) -> (Vec<String>, Vec<String>) {
        let (mut approved, mut once) = (vec![], vec![]);
        for cmd in cmds {
            // Une règle `Bash(...)` ne peut pas porter de parenthèses : on ne devine pas, la demande reste refusée.
            if cmd.contains(['(', ')']) || !self.authorize(i, phase, task_prompt, cmd).await {
                continue;
            }
            if is_risky(cmd) {
                once.push(format!("Bash({cmd})"));
            } else {
                self.grants.lock().unwrap_or_else(PoisonError::into_inner).entry(i).or_default().push(format!("Bash({cmd}:*)"));
            }
            approved.push(cmd.clone());
        }
        (approved, once)
    }

    /// Le supérieur direct décide ; s'il renvoie plus haut, son propre supérieur, jusqu'au CEO qui tranche. Absent ou
    /// illisible = refus.
    async fn authorize(self: &Arc<Self>, i: usize, phase: Phase, task_prompt: &str, cmd: &str) -> bool {
        let who = self.company.seats[i].title.clone();
        let risky = is_risky(cmd);
        let mut cur = self.company.parent(i);
        while let Some(boss) = cur {
            let up = self.company.parent(boss);
            let kids = self.kids(boss, phase);
            let r = self.call(boss, phase, Kind::Answer, permission_prompt(&who, task_prompt, cmd, risky, up.is_some()), Some(permission_schema()), &kids, &[], &[], None).await;
            match r.map(|r| parse_decision(&r)) {
                Ok(Decision::Approve) => return true,
                Ok(Decision::Escalate) => cur = up,
                _ => return false,
            }
        }
        false
    }

    /// Le supérieur direct de `i` répond à la question de `i`. `None` : pas de supérieur, appel en échec ou « je ne sais pas ».
    async fn answer_question(self: &Arc<Self>, i: usize, phase: Phase, task_prompt: &str, q: &super::Question) -> Option<String> {
        let boss = self.company.parent(i)?;
        let kids = self.kids(boss, phase);
        let who = self.company.seats[i].title.clone();
        let r = self.call(boss, phase, Kind::Answer, answer_prompt(&who, task_prompt, q), Some(answer_schema()), &kids, &[], &[], None).await.ok()?;
        parse_answer(&r)
    }

    /// Plan de correction du poste `i` pour les subordonnés en échec ; remonte si personne ne peut corriger.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn fix(self: &Arc<Self>, i: usize, phase: Phase, task: &str, failed: &[(String, String, String, bool, String)], kids: &[usize], sc: &[String], decision: Option<&str>) -> FixPlan {
        let ids: Vec<String> = kids.iter().map(|&k| self.company.seats[k].id.clone()).collect();
        let real = phase == Phase::Realisation;
        let parent_scope = real.then_some(sc);
        for _ in 0..2 {
            match self.call(i, phase, Kind::Fix, fix_prompt(task, failed, parent_scope, decision), Some(fix_schema(&ids, real)), kids, sc, &[], None).await {
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

    /// Revue du poste `i` : le résultat atteint-il l'objectif ? Sinon, tâches complémentaires (redescente).
    /// Une revue qui ne peut pas se faire (plafond, sortie absente) ne fait pas échouer la mission.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn review(self: &Arc<Self>, i: usize, phase: Phase, task: &str, results: &[(String, String, String, bool, String)], kids: &[usize], sc: &[String], rounds_left: u32) -> ReviewPlan {
        let ids: Vec<String> = kids.iter().map(|&k| self.company.seats[k].id.clone()).collect();
        let real = phase == Phase::Realisation;
        let parent_scope = real.then_some(sc);
        for _ in 0..2 {
            match self.call(i, phase, Kind::Review, review_prompt(task, results, parent_scope, rounds_left), Some(review_schema(&ids, real)), kids, sc, &[], None).await {
                Ok(res) if !res.is_error => {
                    if let Some(v) = &res.structured {
                        return parse_review(v, &self.company, kids, parent_scope);
                    }
                }
                _ => return ReviewPlan::Accepted,
            }
        }
        ReviewPlan::Accepted
    }

    /// Le CEO n'a pas pu régler le blocage : pause, l'utilisateur décide (relancer, arrêter, ou en discuter d'abord).
    pub(super) async fn ask_user(self: &Arc<Self>, summary: String) -> EscalationReply {
        if self.cancelled() || self.calls.load(Ordering::SeqCst) >= self.company.max_calls {
            return EscalationReply::Stop;
        }
        let (rtx, mut rrx) = mpsc::unbounded_channel::<EscalationReply>();
        emit(&self.tx, RunEvent::Escalation { summary, reply: rtx });
        let mut cancel = self.cancel.clone();
        tokio::select! {
            r = rrx.recv() => r.unwrap_or(EscalationReply::Stop),
            _ = cancelled(&mut cancel) => EscalationReply::Stop,
        }
    }

    /// Décision écrite par l'utilisateur pendant la discussion avec le CEO.
    pub(super) fn resolution(&self) -> Option<String> {
        let d = self.opts.mission_dir.as_ref()?;
        std::fs::read_to_string(d.join(missions::RESOLUTION_FILE)).ok().filter(|t| !t.trim().is_empty())
    }
}
