//! État partagé d'une phase (`Shared`) et appels vers `claude` : un poste, un appel, un plafond, un coût.

use super::prompt::{MissionPaths, blocked_question, delegate_prompt, human_reply_prompt, resume_after_pause_prompt, retry_note, review_prompt, review_question, role_card};
use super::schema::{Review, delegation_schema, node_status, outcome_schema, parse_assignments, parse_outcome, parse_review, review_schema};
use super::{Assignment, Blocker, CompanyOptions, Kind, Outcome, Phase, Section, Status};
use crate::model::agent::AgentDef;
use crate::model::company::Company;
use crate::runtime::bus::DECISIONS_FILE;
use crate::runtime::claude_proc::{ClaudeEvent, RunSpec};
use crate::runtime::executor::{AskKind, HumanAnswer, HumanQuestion, HumanRequest, NodeJob, NodeResult, NodeStatus, RunEvent, Runner, cancelled};
use crate::runtime::mcp;
use crate::runtime::textutil::truncate_middle;
use crate::runtime::{missions, pause};
use crate::tui::settings::Profile;
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::{Semaphore, mpsc, watch};

/// Taille conservée d'une réponse récupérée en texte libre (sortie structurée perdue) : même ordre que ce
/// qu'un poste remonte à son supérieur.
const MAX_SALVAGE_CHARS: usize = 4000;

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

/// Outils de tous les postes, quel que soit leur rang : lire, écrire, lancer des commandes, chercher sur le web.
/// La liste est explicite (plutôt que « tous les outils de `claude` ») pour garder un prompt système court :
/// il est relu à chaque tour.
pub(super) const TOOLS: &str = "Read,Grep,Glob,Edit,Write,Bash,WebSearch,WebFetch";

/// Profil « appareil dédié » : rien n'est refusé ni demandé. En mode non interactif, un refus ne fait pas
/// réfléchir l'agent : il réessaie, contourne, redemande — et chaque tour relit tout son contexte.
pub(super) const AUTO_MODE: &str = "bypassPermissions";

/// Profil « protégé » : fichiers du projet sans demande ; les commandes sont tenues par le bac à sable, et
/// tout ce qui en sort arrive à l'outil de permission d'essaim, donc à l'humain.
pub(super) const GUARDED_MODE: &str = "acceptEdits";

/// Ce que l'humain décide pour un poste qui n'a pas fini.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Verdict {
    /// Relancer le poste dans sa session, avec la réponse de l'humain (vide = simple nouvel essai).
    Retry(String),
    /// Continuer la mission sans ce poste.
    Waive,
    /// Arrêter la mission.
    Stop,
    /// Aucun humain ne répond : le poste reste bloqué et les travaux qui en dépendent ne partent pas.
    Nobody,
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
    /// Nœuds déjà terminés. Un poste rappelé par son supérieur (vague suivante, descente corrective) refait les
    /// mêmes types d'appel : son nouveau nœud prend alors un nom distinct, pour ne pas écraser le coût du premier.
    pub(super) closed: Mutex<std::collections::BTreeSet<String>>,
    /// L'humain a demandé l'arrêt de la mission en réponse à une demande : plus aucun appel ne part.
    pub(super) halted: AtomicBool,
}

impl Shared {
    /// Subordonnés directs : tous sont mobilisables, le délégant ne retient que ceux qui sont utiles.
    pub(super) fn kids(&self, i: usize) -> Vec<usize> {
        self.company.children(i)
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

    /// Plus aucun appel ne doit partir : run annulé, ou arrêt demandé par l'humain.
    pub(super) fn stopped(&self) -> bool {
        *self.cancel.borrow() || self.halted.load(Ordering::SeqCst)
    }

    /// Fichiers de la mission tels qu'un poste les désigne : relatifs au projet quand la mission y est rangée.
    fn paths(&self) -> Option<MissionPaths> {
        let d = self.opts.mission_dir.as_ref()?;
        let rel = |name: &str| d.strip_prefix(&self.opts.base_dir).unwrap_or(d).join(name).display().to_string();
        Some(MissionPaths { brief: rel("brief.md"), plan: rel("conception.md"), notes: rel(missions::NOTES_DIR), decisions: rel(&format!("{}/{DECISIONS_FILE}", missions::NOTES_DIR)) })
    }

    /// Paramètres d'un appel. **Tous les postes ont tous les outils, à tous les types d'appel** : un supérieur
    /// lit le projet avant de découper, une feuille lance les commandes dont elle a besoin. Ce que chacun fait
    /// de ses droits tient à sa fiche de poste ; ce que la machine laisse faire tient au profil de sécurité.
    #[allow(clippy::too_many_arguments)]
    fn spec(&self, i: usize, phase: Phase, kind: Kind, prompt: String, schema: Option<String>, kids: &[usize], sc: &[String]) -> Result<RunSpec> {
        let seat = &self.company.seats[i];
        let mut agent = self.agents.iter().find(|a| a.name == seat.agent).cloned().ok_or_else(|| anyhow::anyhow!("agent « {} » introuvable", seat.agent))?;
        if seat.model.is_some() {
            agent.model = seat.model.clone();
        } else if let Some(m) = self.dynamic_model(i, kids.is_empty()) {
            agent.model = Some(m);
        }
        // La liste d'outils du fichier d'agent ne restreint plus rien : c'est essaim qui équipe les postes.
        agent.tools = None;
        let mut agents = BTreeMap::new();
        agents.insert(agent.name.clone(), agent.to_cli_json());
        let mut tools = TOOLS.to_string();
        // Assistants légers (modèle moindre) : tâches mécaniques sans décision. Ils lisent et lancent des
        // commandes, ils n'écrivent pas de fichiers.
        if kind == Kind::Work
            && let Some(model) = &seat.helpers
        {
            agents.insert(
                HELPER.into(),
                serde_json::json!({
                    "description": "Light assistant for purely mechanical, fully specified tasks (inventories, running a command and reporting its raw output, counting, extracting). Takes no decision, never edits files.",
                    "prompt": "You are a light assistant. Do exactly what is asked, nothing more. Report raw results, concisely. Take no decision, never modify any file, and say so plainly if the task is unclear or fails.",
                    "model": model,
                    "tools": ["Read", "Grep", "Glob", "Bash"],
                }),
            );
            tools.push_str(",Agent");
        }
        let protected = self.opts.access.profile == Profile::Protected;
        Ok(RunSpec {
            prompt,
            cwd: self.opts.base_dir.clone(),
            agents,
            agent: Some(agent.name.clone()),
            tools: Some(tools),
            // Profil protégé, jamais de demande pour : la recherche web (elle ne sort rien de la machine) et
            // `ask_human` (une question à l'humain ne se fait pas précéder d'une demande d'autorisation).
            allowed_tools: match (protected, self.opts.bus.is_some()) {
                (true, true) => vec!["WebSearch".into(), format!("mcp__{}__{}", mcp::SERVER, mcp::ASK_TOOL)],
                (true, false) => vec!["WebSearch".into()],
                _ => vec![],
            },
            permission_mode: Some(if protected { GUARDED_MODE } else { AUTO_MODE }.to_string()),
            // Sans bus, personne ne répondrait aux demandes : `claude` les refuse alors de lui-même.
            permission_prompt_tool: (protected && self.opts.bus.is_some()).then(|| format!("mcp__{}__{}", mcp::SERVER, mcp::PERMISSION_TOOL)),
            settings: self.opts.access.claude_settings(),
            append_system_prompt: Some(role_card(self.opts.lang, &self.company, i, &self.brief, phase, kids, sc, self.paths().as_ref())),
            json_schema: schema,
            // Plafond de dépense : celui du poste s'il en a un, sinon celui de l'entreprise.
            max_budget_usd: seat.max_budget_usd.or(self.company.max_budget_usd),
            ..Default::default()
        })
    }

    /// Un appel `claude`, mis en attente tant que le run est en pause et rejoué tel quel si la limite de
    /// l'abonnement tombe pendant l'appel : le poste n'échoue pas et l'appel n'est pas consommé.
    #[allow(clippy::too_many_arguments)]
    async fn call(self: &Arc<Self>, i: usize, phase: Phase, kind: Kind, prompt: String, schema: Option<String>, kids: &[usize], sc: &[String], sections: &[Section], resume: Option<String>, step: Option<u32>) -> Result<NodeResult> {
        // Un appel arrêté par la pause repart sur sa propre session, avec un prompt de reprise :
        // le prompt et la reprise évoluent donc d'un tour de boucle au suivant.
        let task = prompt.clone();
        let mut prompt = prompt;
        let mut resume = resume;
        loop {
            self.wait_resume().await?;
            let res = self.attempt(i, phase, kind, prompt.clone(), schema.clone(), kids, sc, sections, resume.clone(), step).await;
            match &res {
                Ok(r) if r.limit.is_some() => self.enter_limit_pause(r.limit.and_then(|l| l.resets_at)),
                // Arrêt demandé par la pause : rien n'est perdu. On reprend la session là où l'agent
                // s'est arrêté, en lui rappelant sa tâche et son propre bilan. Sans session annoncée,
                // l'appel repart tel quel (même repli que la limite d'abonnement).
                Ok(r) if r.interrupted.is_some() => {
                    let it = r.interrupted.clone().unwrap_or_default();
                    match it.session {
                        Some(s) => {
                            prompt = resume_after_pause_prompt(&task, &it.note);
                            resume = Some(s);
                        }
                        None => {
                            prompt = task.clone();
                            resume = None;
                        }
                    }
                }
                _ => return res,
            }
        }
    }

    /// Attend l'ouverture de la porte si le run est en pause. Échoue si le run est annulé, avant ou pendant.
    async fn wait_resume(&self) -> Result<()> {
        if self.stopped() {
            bail!("cancelled");
        }
        if !self.opts.pause.paused() {
            return Ok(());
        }
        let mut cancel = self.cancel.clone();
        tokio::select! {
            _ = self.opts.pause.wait() => {}
            _ = cancelled(&mut cancel) => bail!("cancelled"),
        }
        // La reprise n'est annoncée qu'une fois, quel que soit le nombre d'appels qui attendaient.
        if self.opts.pause.take_resumed() {
            if let Some(d) = &self.opts.mission_dir {
                missions::clear_pause(d);
            }
            emit(&self.tx, RunEvent::Resumed);
        }
        if self.stopped() {
            bail!("cancelled");
        }
        Ok(())
    }

    /// Limite de l'abonnement atteinte : le run se met en pause (reprise automatique à la réinitialisation
    /// annoncée) et la mission porte la marque sur disque, pour être reprise même après avoir quitté essaim.
    fn enter_limit_pause(&self, resets_at: Option<u64>) {
        if self.opts.pause.pause(pause::Why::Limit, resets_at) {
            emit(&self.tx, RunEvent::Paused { why: pause::Why::Limit, resets_at });
        }
        if let Some(d) = &self.opts.mission_dir {
            missions::mark_paused(d, resets_at);
        }
    }

    /// Une tentative d'appel : plafond d'appels, parallélisme borné, événements, coût. Un résultat qui porte
    /// `limit` n'est ni facturé ni clos (pas de `NodeFinished`) : l'appelant le rejoue après la pause.
    #[allow(clippy::too_many_arguments)]
    async fn attempt(self: &Arc<Self>, i: usize, phase: Phase, kind: Kind, prompt: String, schema: Option<String>, kids: &[usize], sc: &[String], sections: &[Section], resume: Option<String>, step: Option<u32>) -> Result<NodeResult> {
        if self.stopped() {
            bail!("cancelled");
        }
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.company.max_calls {
            bail!(
                "call cap reached (max_calls = {}): raise it for this company — a seat that splits its work into successive steps makes one call per step, which the pre-launch estimate does not count",
                self.company.max_calls
            );
        }
        let seat = &self.company.seats[i];
        // Un appel de reprise ou de réponse est un nœud à part (sinon il écraserait le coût affiché du premier).
        let base = format!("{}/{}/{}", seat.id, phase.key(), kind.key());
        // Une étape numérotée est un nœud nommé (« …/work@2 ») ; une reprise ou une réponse prend le
        // numéro d'appel, pour ne pas écraser le coût affiché du premier.
        let mut id = match (step, resume.is_some()) {
            (Some(k), false) => format!("{base}@{}", k + 1),
            (Some(k), true) => format!("{base}@{}#{n}", k + 1),
            (None, true) => format!("{base}#{n}"),
            (None, false) => base,
        };
        // Même poste, même type d'appel, une seconde fois (vague suivante, descente corrective) : nœud distinct.
        // Un appel rejoué après une pause garde son nom, son nœud n'ayant jamais été clos.
        if self.closed.lock().unwrap_or_else(PoisonError::into_inner).contains(&id) {
            id = format!("{id}#{n}");
        }
        let builds = seat.builds || self.company.seats.len() == 1;
        let verb = match (kind, phase) {
            (Kind::Work, _) if resume.is_some() => "resumes",
            (Kind::Delegate, _) => "delegates",
            (Kind::Work, Phase::Conception) => "plans",
            (Kind::Work, Phase::Realisation) if builds => "builds ⚒",
            (Kind::Work, Phase::Realisation) => "analyses",
            (Kind::Report, _) => "reports",
            (Kind::Review, _) => "reviews",
        };
        emit(&self.tx, RunEvent::Label {
            id: id.clone(),
            label: format!("{} · {verb}{}", seat.title, self.dynamic_model(i, kids.is_empty()).map(|m| format!(" [{m}]")).unwrap_or_default()),
            agent: seat.agent.clone(),
            depth: self.company.depth(i),
        });
        let mut spec = self.spec(i, phase, kind, prompt, schema, kids, sc)?;
        spec.resume = resume;
        // Le serveur MCP d'essaim, relié à ce nœud : c'est par lui que l'agent s'adresse à l'humain.
        spec.mcp_config = self.opts.bus.as_ref().map(|b| b.mcp_config(&id, &seat.title, spec.permission_prompt_tool.is_some()));
        let _permit = self.sem.acquire().await?;
        if self.stopped() {
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
        let res = (self.runner)(job, ctx, self.cancel.clone(), self.opts.pause.subscribe()).await;
        // Jointure du relais d'événements : son erreur (panique du relais) n'altère ni le résultat ni le coût.
        let _ = fwd.await;

        // Limite de l'abonnement : l'appel n'a rien produit (aucun jeton). Le nœud reste ouvert, le plafond
        // d'appels est rendu, et l'appelant met le run en pause avant de rejouer.
        if res.as_ref().is_ok_and(|r| r.limit.is_some()) {
            self.calls.fetch_sub(1, Ordering::SeqCst);
            return res;
        }
        // Arrêt par la pause : comme la limite, le nœud reste ouvert et le plafond d'appels est rendu —
        // l'appel sera repris, pas rejoué depuis zéro. À la différence de la limite, des jetons ont
        // réellement été consommés : ils sont comptés tout de suite, sinon le coût du run est faux.
        if let Ok(r) = &res
            && r.interrupted.is_some()
        {
            self.calls.fetch_sub(1, Ordering::SeqCst);
            *cost_guard(&self.cost) += r.tokens;
            return res;
        }
        let (status, used) = match &res {
            Ok(r) => (node_status(kind, r, sections), r.tokens),
            Err(_) if self.stopped() => (NodeStatus::Cancelled, 0),
            Err(e) => (NodeStatus::Failed(e.to_string()), 0),
        };
        *cost_guard(&self.cost) += used;
        self.closed.lock().unwrap_or_else(PoisonError::into_inner).insert(id.clone());
        emit(&self.tx, RunEvent::NodeFinished { id, status, tokens: used });
        res
    }

    /// Délégation avec un nouvel essai si la sortie structurée est absente ou inutilisable.
    pub(super) async fn delegate(self: &Arc<Self>, i: usize, phase: Phase, task: &str, kids: &[usize], sc: &[String]) -> Result<Vec<Assignment>> {
        let mut last = anyhow::anyhow!("delegation failed");
        // Second essai dans la même session : le modèle a déjà la tâche et sa propre réponse en contexte,
        // on lui redemande seulement de corriger la forme au lieu de tout renvoyer.
        let mut again: Option<String> = None;
        for attempt in 0..2 {
            let prompt = match (attempt, &again) {
                (0, _) => delegate_prompt(task, sc),
                (_, Some(_)) => retry_note(&last.to_string()),
                _ => format!("{}{}", delegate_prompt(task, sc), retry_note(&last.to_string())),
            };
            let r = self.call(i, phase, Kind::Delegate, prompt, Some(delegation_schema()), kids, sc, &[], again.clone(), None).await;
            match r {
                Ok(res) if !res.is_error => {
                    again = res.session_id.clone();
                    match res.structured.as_ref().map(|v| parse_assignments(v, &self.company, kids, sc)) {
                        Some(Ok(a)) => return Ok(a),
                        Some(Err(e)) => last = e,
                        None => last = anyhow::anyhow!("structured output missing"),
                    }
                }
                Ok(res) => return Err(anyhow::anyhow!(res.output)),
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// Appel de travail ou de rapport : sortie structurée obligatoire, un nouvel essai si elle manque.
    /// `resume` : session à reprendre (réponse de l'humain à un poste bloqué).
    /// `step` : indice de l'étape (0 pour la première) quand un poste en enchaîne plusieurs, afin que
    /// chaque étape soit un nœud distinct dans le journal et dans le coût. `None` = appel unique.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn act(self: &Arc<Self>, i: usize, phase: Phase, kind: Kind, prompt: String, kids: &[usize], sc: &[String], sections: &[Section], step: Option<u32>, resume: Option<String>) -> Outcome {
        let (mut prompt, mut resume) = (prompt, resume);
        // Texte libre du dernier essai sans sortie structurée exploitable : récupéré plutôt que jeté.
        let mut salvage = String::new();
        let mut session = None;
        for _ in 0..2 {
            match self.call(i, phase, kind, prompt.clone(), Some(outcome_schema()), kids, sc, sections, resume.clone(), step).await {
                Ok(r) => {
                    if let Some(o) = parse_outcome(&r, sections) {
                        return o;
                    }
                    salvage = r.output.clone();
                    session = r.session_id.clone().or(session);
                    let cause = if r.structured.is_none() { "no structured output was returned" } else { "the structured output had a missing or unknown `status`" };
                    // Sortie inexploitable : on redemande le format **dans la même session**. Tout le
                    // travail de l'agent (fichiers lus, commandes lancées) est déjà dans son contexte ;
                    // repartir à neuf le ferait tout refaire, et c'est là que la facture double.
                    match r.session_id.clone() {
                        Some(sid) => {
                            prompt = retry_note(cause);
                            resume = Some(sid);
                        }
                        None => prompt = format!("{prompt}{}", retry_note(cause)),
                    }
                }
                Err(e) => return Outcome::failed(e.to_string()),
            }
        }
        // Deux appels sans sortie structurée lisible : le texte libre produit vaut mieux que rien. Il remonte
        // comme un blocage, jamais comme un succès — rien ne prouve que la tâche est terminée.
        let detail = "no usable structured output after two attempts";
        match salvage.trim() {
            "" => Outcome { session, ..Outcome::failed(detail) },
            text => Outcome {
                status: Status::Blocked,
                report: truncate_middle(text, MAX_SALVAGE_CHARS),
                blockers: vec![Blocker { kind: "other".into(), detail: detail.into(), needs: "the text below is the raw answer, unverified".into() }],
                session,
            },
        }
    }

    /// Appel de travail mené jusqu'à une issue : si le poste ne finit pas, **l'humain** est consulté, et lui
    /// seul. Sa réponse relance le poste dans sa session (contexte conservé). Rien ne se rejoue tout seul :
    /// chaque nouvel essai est une décision de l'humain. Rend l'issue et « l'humain a dit de continuer sans lui ».
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn work(self: &Arc<Self>, i: usize, phase: Phase, prompt: String, kids: &[usize], sc: &[String], sections: &[Section], step: Option<u32>) -> (Outcome, bool) {
        let first = prompt.clone();
        let (mut prompt, mut resume) = (prompt, None::<String>);
        loop {
            let o = self.act(i, phase, Kind::Work, prompt, kids, sc, sections, step, resume).await;
            if o.status == Status::Done || self.stopped() {
                return (o, false);
            }
            match self.ask_blocked(i, &o.report, &o.blockers).await {
                Verdict::Retry(note) => match o.session.clone() {
                    Some(sid) => {
                        prompt = human_reply_prompt(&note);
                        resume = Some(sid);
                    }
                    // Session inconnue (appel jamais parti ou tué) : la tâche repart, avec la réponse en tête.
                    None => {
                        prompt = format!("{first}\n\n{}", human_reply_prompt(&note));
                        resume = None;
                    }
                },
                Verdict::Waive => return (o, true),
                Verdict::Stop => {
                    self.halted.store(true, Ordering::SeqCst);
                    return (o, false);
                }
                Verdict::Nobody => return (o, false),
            }
        }
    }

    /// Revue du poste `i` : le résultat atteint-il l'objectif ? Elle constate, elle ne relance rien.
    /// Une revue qui ne peut pas se faire (plafond, sortie absente) ne fait pas échouer la mission.
    pub(super) async fn review(self: &Arc<Self>, i: usize, phase: Phase, task: &str, results: &[(String, String, String, bool, String)], kids: &[usize], sc: &[String]) -> Review {
        // Second essai dans la même session (voir `delegate`) : on redemande le format, pas le travail.
        let mut again: Option<String> = None;
        for attempt in 0..2 {
            let prompt = match (attempt, &again) {
                (0, _) => review_prompt(task, results),
                (_, Some(_)) => retry_note("no structured output was returned"),
                _ => format!("{}{}", review_prompt(task, results), retry_note("no structured output was returned")),
            };
            match self.call(i, phase, Kind::Review, prompt, Some(review_schema()), kids, sc, &[], again.clone(), None).await {
                Ok(res) if !res.is_error => {
                    if let Some(v) = &res.structured {
                        return parse_review(v);
                    }
                    again = res.session_id.clone();
                }
                _ => return Review::Accepted,
            }
        }
        Review::Accepted
    }

    /// Pose une question à l'humain au nom du poste `i` et attend sa réponse. Sans réponse possible (run
    /// arrêté, plafond d'appels atteint : aucune relance ne pourrait partir) : `Unavailable`.
    async fn ask(self: &Arc<Self>, i: usize, kind: AskKind, q: HumanQuestion) -> HumanAnswer {
        if self.stopped() || self.calls.load(Ordering::SeqCst) >= self.company.max_calls {
            return HumanAnswer::Unavailable;
        }
        let (rtx, mut rrx) = mpsc::unbounded_channel();
        emit(&self.tx, RunEvent::HumanRequest(HumanRequest { node: String::new(), seat: self.company.seats[i].title.clone(), kind, questions: vec![q], reply: rtx }));
        let mut cancel = self.cancel.clone();
        tokio::select! {
            r = rrx.recv() => r.and_then(|a| a.into_iter().next()).unwrap_or(HumanAnswer::Unavailable),
            _ = cancelled(&mut cancel) => HumanAnswer::Unavailable,
        }
    }

    /// Le poste `i` n'a pas fini : l'humain répond (le poste repart avec sa réponse), passe outre, ou arrête.
    pub(super) async fn ask_blocked(self: &Arc<Self>, i: usize, report: &str, blockers: &[Blocker]) -> Verdict {
        let l = self.opts.lang;
        let lines: Vec<String> = blockers.iter().map(Blocker::line).collect();
        let q = HumanQuestion {
            text: blocked_question(l, &self.company.seats[i].title, report, &lines),
            options: vec![l.t("Relancer ce poste", "Send this seat back to work").into(), l.t("Continuer sans lui", "Go on without it").into(), l.t("Arrêter la mission", "Stop the mission").into()],
            recommended: None,
        };
        match self.ask(i, AskKind::Blocked, q).await {
            HumanAnswer::Option(0) => Verdict::Retry(String::new()),
            HumanAnswer::Option(1) => Verdict::Waive,
            HumanAnswer::Option(_) => Verdict::Stop,
            HumanAnswer::Text(t) => Verdict::Retry(t),
            HumanAnswer::Unavailable => Verdict::Nobody,
        }
    }

    /// La revue ne valide pas le résultat : l'humain relance une descente corrective (`Retry`, avec sa consigne),
    /// accepte le résultat tel quel (`Waive`), ou arrête.
    pub(super) async fn ask_review(self: &Arc<Self>, i: usize, findings: &str) -> Verdict {
        let l = self.opts.lang;
        let q = HumanQuestion {
            text: review_question(l, findings),
            options: vec![l.t("Relancer pour corriger ces écarts", "Send it back to close these gaps").into(), l.t("Accepter le résultat tel quel", "Accept the result as is").into(), l.t("Arrêter la mission", "Stop the mission").into()],
            recommended: None,
        };
        match self.ask(i, AskKind::Review, q).await {
            HumanAnswer::Option(0) => Verdict::Retry(String::new()),
            HumanAnswer::Option(1) => Verdict::Waive,
            HumanAnswer::Option(_) => Verdict::Stop,
            HumanAnswer::Text(t) => Verdict::Retry(t),
            HumanAnswer::Unavailable => Verdict::Nobody,
        }
    }
}
