//! Bus des demandes à l'humain : le côté essaim du serveur MCP (`runtime::mcp`).
//!
//! Chaque `claude` d'un poste lance `essaim mcp`, qui relaie ici ses appels d'outil par une socket Unix.
//! Le bus en fait une `HumanRequest` pour l'interface, attend la réponse et la renvoie : l'agent reprend
//! dans la même session. Deux outils :
//! - `ask_human` : question ou demande d'action, à n'importe quel moment de la tâche ;
//! - `permission` (profil protégé) : une action sort du bac à sable ou du projet et demande un accord.
//!
//! Les demandes de permission que `claude` adresse directement à son hôte (un domaine réseau demandé depuis
//! le bac à sable) arrivent par `permit` et suivent le même chemin.
//!
//! Le bus tient aussi le compte du temps que chaque nœud passe à attendre (ce temps ne compte pas dans son
//! délai), consigne les questions et leurs réponses dans `notes/decisions.md`, et retient les accords
//! donnés « pour la mission ».

use crate::runtime::executor::{AskKind, HumanAnswer, HumanQuestion, HumanRequest, RunEvent};
use crate::runtime::mcp;
use crate::runtime::pause::{Gate, Why};
use crate::runtime::textutil::take_chars;
use crate::tui::settings::Lang;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Fichier des questions posées à l'humain et de ses réponses, dans le dossier de notes de la mission.
pub const DECISIONS_FILE: &str = "decisions.md";
/// Première ligne du fichier. Il est créé avec elle dès le début de la mission : les postes ont la consigne
/// de le lire avant de questionner, et un fichier absent leur coûtait un tour à le signaler.
pub const DECISIONS_HEADER: &str = "# Decisions of the human during the mission\n";

/// Tailles conservées : une demande s'affiche dans un panneau, pas dans un document.
const MAX_QUESTIONS: usize = 4;
const MAX_OPTIONS: usize = 6;
const QUESTION_CHARS: usize = 800;
const OPTION_CHARS: usize = 160;

/// Attentes d'un nœud : nombre de demandes en cours, début de l'attente courante, durée des attentes terminées.
#[derive(Default)]
struct Wait {
    pending: usize,
    since: Option<Instant>,
    done: Duration,
}

pub struct Bus {
    path: PathBuf,
    token: String,
    tx: mpsc::UnboundedSender<RunEvent>,
    /// Dossier de notes de la mission (`decisions.md`) ; absent = rien n'est consigné.
    notes: Option<PathBuf>,
    lang: Lang,
    pause: Arc<Gate>,
    waits: Mutex<HashMap<String, Wait>>,
    /// Intitulé du poste de chaque nœud lancé (pour les demandes qui n'arrivent pas par le serveur MCP).
    seats: Mutex<HashMap<String, String>>,
    /// Accords donnés « pour la mission » (profil protégé) : signature de la demande.
    allowed: Mutex<HashSet<String>>,
    /// Une commande qui reste dans le bac à sable est accordée sans demande (voir `trust_sandbox`).
    sandbox_trusted: AtomicBool,
    /// Écritures de `decisions.md` l'une après l'autre.
    log: Mutex<()>,
    accept: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for Bus {
    fn drop(&mut self) {
        if let Some(h) = self.accept.lock().unwrap_or_else(PoisonError::into_inner).take() {
            h.abort();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Ce qu'a donné l'attente d'une réponse.
enum Waited {
    Answers(Vec<HumanAnswer>),
    /// Le demandeur a disparu (agent arrêté, run annulé) : plus personne à qui répondre.
    Gone,
    /// Le run a été mis en pause à la main avant la réponse.
    Paused,
}

/// Chemin de la socket : court (la limite du système est d'environ 100 octets) et propre à ce processus.
fn socket_path() -> PathBuf {
    let name = format!("essaim-{}-{}.sock", std::process::id(), &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let p = std::env::temp_dir().join(&name);
    if p.as_os_str().len() < 100 { p } else { PathBuf::from("/tmp").join(name) }
}

impl Bus {
    /// Ouvre la socket et commence à écouter. `notes` : dossier de notes de la mission.
    pub fn start(tx: mpsc::UnboundedSender<RunEvent>, notes: Option<PathBuf>, lang: Lang, pause: Arc<Gate>) -> Result<Arc<Bus>> {
        let path = socket_path();
        let listener = UnixListener::bind(&path).with_context(|| format!("socket du bus : {}", path.display()))?;
        // La socket n'est ouverte qu'à l'utilisateur ; le jeton écarte en plus tout autre processus de la machine.
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).ok();
        let bus = Arc::new(Bus {
            path,
            token: uuid::Uuid::new_v4().simple().to_string(),
            tx,
            notes,
            lang,
            pause,
            waits: Mutex::default(),
            seats: Mutex::default(),
            allowed: Mutex::default(),
            sandbox_trusted: AtomicBool::new(false),
            log: Mutex::new(()),
            accept: Mutex::new(None),
        });
        // La boucle d'écoute ne retient pas le bus : il disparaît avec le run, et la socket avec lui.
        let weak: Weak<Bus> = Arc::downgrade(&bus);
        let handle = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let Some(bus) = weak.upgrade() else { break };
                tokio::spawn(async move { bus.serve(stream).await });
            }
        });
        *bus.accept.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
        Ok(bus)
    }

    /// Configuration MCP (`--mcp-config`) d'un appel : le serveur `essaim mcp`, relié à ce bus pour ce nœud.
    pub fn mcp_config(&self, node: &str, seat: &str, permission: bool) -> String {
        self.seats.lock().unwrap_or_else(PoisonError::into_inner).insert(node.to_string(), seat.to_string());
        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "essaim".into());
        let mut args = vec!["mcp".to_string(), "--socket".into(), self.path.display().to_string(), "--node".into(), node.into(), "--seat".into(), seat.into()];
        if permission {
            args.push("--permission".into());
        }
        json!({"mcpServers": {mcp::SERVER: {"command": exe, "args": args, "env": {mcp::TOKEN_ENV: self.token}}}}).to_string()
    }

    /// Profil protégé : `claude` soumet parfois une commande alors qu'elle tournera dans le bac à sable
    /// (mesuré sur 2.1.322 : commandes composées, substitutions `$(…)`). Le système la borne déjà : la
    /// faire valider par l'humain n'ajoute que du bruit, et le bruit apprend à tout accepter. Ces commandes
    /// sont donc accordées d'office — sauf si le projet exclut des commandes du bac à sable
    /// (`sandbox.excludedCommands`) : une commande sans marque pourrait alors tourner hors de lui.
    pub fn trust_sandbox(&self, project: &Path) {
        self.sandbox_trusted.store(!excludes_commands(project), Ordering::SeqCst);
    }

    /// Temps d'attente de l'humain d'un nœud : (attentes terminées, attente en cours ?). Voir `executor::WaitProbe`.
    pub fn waited(&self, node: &str) -> (Duration, bool) {
        let w = self.waits.lock().unwrap_or_else(PoisonError::into_inner);
        w.get(node).map(|x| (x.done, x.pending > 0)).unwrap_or((Duration::ZERO, false))
    }

    /// Début d'une attente ; elle se termine quand la garde rendue disparaît, y compris si la tâche qui
    /// attend est annulée (sans quoi le nœud paraîtrait attendre l'humain pour toujours).
    fn wait_begin(&self, node: &str) -> WaitGuard<'_> {
        let mut w = self.waits.lock().unwrap_or_else(PoisonError::into_inner);
        let x = w.entry(node.to_string()).or_default();
        if x.pending == 0 {
            x.since = Some(Instant::now());
        }
        x.pending += 1;
        WaitGuard { bus: self, node: node.to_string() }
    }

    fn wait_end(&self, node: &str) {
        let mut w = self.waits.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(x) = w.get_mut(node) {
            x.pending = x.pending.saturating_sub(1);
            if x.pending == 0 {
                x.done += x.since.take().map(|s| s.elapsed()).unwrap_or_default();
            }
        }
    }

    /// Une connexion de `essaim mcp` : une requête par ligne, une réponse par ligne.
    async fn serve(self: Arc<Self>, stream: UnixStream) {
        let (r, mut w) = stream.into_split();
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(v) if v["token"].as_str() == Some(self.token.as_str()) => self.handle(&v, &mut lines).await,
                _ => Some(("refused: bad request or token".to_string(), true)),
            };
            // `None` : le demandeur est parti pendant l'attente.
            let Some((text, is_error)) = reply else { break };
            if w.write_all(format!("{}\n", json!({"text": text, "is_error": is_error})).as_bytes()).await.is_err() {
                break;
            }
        }
    }

    async fn handle(&self, v: &Value, lines: &mut Lines<BufReader<OwnedReadHalf>>) -> Option<(String, bool)> {
        let node = v["node"].as_str().unwrap_or_default();
        let seat = v["seat"].as_str().unwrap_or_default();
        match v["tool"].as_str().unwrap_or_default() {
            mcp::ASK_TOOL => self.ask(node, seat, &v["args"], lines).await,
            mcp::PERMISSION_TOOL => {
                let args = &v["args"];
                let decision = self.decide(node, seat, args["tool_name"].as_str().unwrap_or_default(), &args["input"], gone(lines)).await;
                decision.map(|d| (d.to_string(), false))
            }
            t => Some((format!("unknown tool: {t}"), true)),
        }
    }

    /// Émet la demande et attend : la réponse de l'humain, le départ du demandeur (`gone`), ou la pause du run.
    async fn request(&self, node: &str, seat: &str, kind: AskKind, questions: Vec<HumanQuestion>, gone: impl Future<Output = ()>) -> Waited {
        let n = questions.len();
        let (rtx, mut rrx) = mpsc::unbounded_channel();
        let req = HumanRequest { node: node.to_string(), seat: seat.to_string(), kind, questions, reply: rtx };
        if self.tx.send(RunEvent::HumanRequest(req)).is_err() {
            return Waited::Answers(vec![HumanAnswer::Unavailable; n]); // plus d'interface : personne ne répondra
        }
        let _waiting = self.wait_begin(node);
        let mut pause = self.pause.subscribe();
        tokio::select! {
            a = rrx.recv() => {
                // Canal fermé sans réponse = demande écartée par l'interface : aucun humain pour celle-ci.
                let mut a = a.unwrap_or_default();
                a.resize(n, HumanAnswer::Unavailable);
                Waited::Answers(a)
            }
            _ = gone => Waited::Gone,
            _ = user_pause(&mut pause) => Waited::Paused,
        }
    }

    /// Outil `ask_human` : questions de l'agent → réponses de l'humain, en texte.
    async fn ask(&self, node: &str, seat: &str, args: &Value, lines: &mut Lines<BufReader<OwnedReadHalf>>) -> Option<(String, bool)> {
        let kind = match args["kind"].as_str() {
            Some("action") => AskKind::Action,
            Some("blocked") => AskKind::Blocked,
            _ => AskKind::Question,
        };
        let questions = parse_questions(args);
        if questions.is_empty() {
            return Some(("ask_human needs `questions`: at least one item with a non-empty `text`.".into(), true));
        }
        match self.request(node, seat, kind, questions.clone(), gone(lines)).await {
            Waited::Gone => None,
            Waited::Paused => Some(("The run was paused by the user before this was answered. Do not start anything new: stop now and say what is done and what is left. Ask again once the run resumes.".into(), false)),
            Waited::Answers(answers) => {
                let pairs: Vec<(String, String, bool)> = questions.iter().zip(&answers).map(|(q, a)| (q.text.clone(), answer_text(q, a), matches!(a, HumanAnswer::Unavailable))).collect();
                self.record(seat, kind, &questions, &answers);
                let mut out = String::new();
                for (q, a, _) in &pairs {
                    out.push_str(&format!("Q: {q}\nA: {a}\n"));
                }
                if pairs.iter().any(|(_, _, none)| *none) {
                    out.push_str("\nNo human is available for the unanswered item(s). A question: go on with the option you recommended and state that assumption in your report. An action only the human can do: stop and report status blocked with exactly what is needed.");
                }
                Some((out.trim_end().to_string(), false))
            }
        }
    }

    /// Demande de permission que `claude` adresse à son hôte pour le nœud `node` (voir `executor::Permit`).
    /// Rend toujours une décision : sans humain pour répondre, c'est un refus, jamais une attente sans fin.
    pub async fn permit(&self, node: &str, tool: &str, input: &Value) -> Value {
        let seat = self.seats.lock().unwrap_or_else(PoisonError::into_inner).get(node).cloned().unwrap_or_default();
        // Le demandeur ne « part » pas ici : c'est l'appelant qui abandonne l'attente quand son processus s'arrête.
        let decision = self.decide(node, &seat, tool, input, std::future::pending()).await;
        decision.unwrap_or_else(|| json!({"behavior": "deny", "message": "cancelled"}))
    }

    /// Permission (profil protégé) : accord, refus ou demande à l'humain. Rend la décision attendue par
    /// `claude` : `{"behavior":"allow","updatedInput":…}` ou `{"behavior":"deny","message":…}` ; `None` si
    /// le demandeur est parti avant la réponse.
    async fn decide(&self, node: &str, seat: &str, tool: &str, input: &Value, gone: impl Future<Output = ()>) -> Option<Value> {
        let allow = || json!({"behavior": "allow", "updatedInput": input});
        let deny = |m: &str| json!({"behavior": "deny", "message": m});
        // Les outils d'essaim lui-même (`ask_human`) : la question va de toute façon à l'humain.
        if tool.starts_with(&format!("mcp__{}__", mcp::SERVER)) {
            return Some(allow());
        }
        if tool == "Bash" && input["dangerouslyDisableSandbox"] != true && self.sandbox_trusted.load(Ordering::SeqCst) {
            return Some(allow());
        }
        let sig = signature(tool, input);
        if self.allowed.lock().unwrap_or_else(PoisonError::into_inner).contains(&sig) {
            return Some(allow());
        }
        let l = self.lang;
        let site = matches!(tool, "WebFetch" | NETWORK_TOOL);
        let q = HumanQuestion {
            text: describe(tool, input, l),
            options: vec![
                l.t("Autoriser une fois", "Allow once").into(),
                if site { l.t("Autoriser ce site pour la mission", "Allow this site for the mission") } else { l.t("Autoriser pour la mission", "Allow for the mission") }.into(),
                l.t("Refuser", "Deny").into(),
            ],
            recommended: None,
        };
        Some(match self.request(node, seat, AskKind::Permission, vec![q], gone).await {
            Waited::Gone => return None,
            Waited::Paused => deny("The run was paused by the user: do not start anything new, stop now and say what is done and what is left."),
            Waited::Answers(a) => match a.into_iter().next() {
                Some(HumanAnswer::Option(0)) => allow(),
                Some(HumanAnswer::Option(1)) => {
                    self.allowed.lock().unwrap_or_else(PoisonError::into_inner).insert(sig);
                    allow()
                }
                Some(HumanAnswer::Text(t)) if !t.trim().is_empty() => deny(&format!("Refused by the human, who says: {}", t.trim())),
                Some(HumanAnswer::Unavailable) | None => deny("No human is available to approve this. Do without it; if the task cannot be done without it, report status blocked with exactly what is needed."),
                _ => deny("Refused by the human. Do without it; if the task cannot be done without it, report status blocked with exactly what is needed."),
            },
        })
    }

    /// Consigne les questions et leurs réponses : les autres postes les lisent avant de reposer la même.
    fn record(&self, seat: &str, kind: AskKind, questions: &[HumanQuestion], answers: &[HumanAnswer]) {
        let Some(dir) = &self.notes else { return };
        let _guard = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        let mut block = format!("\n## {seat} · {}\n", kind.key());
        for (q, a) in questions.iter().zip(answers) {
            let note = if matches!(a, HumanAnswer::Unavailable) { " (assumed: no human available)" } else { "" };
            block.push_str(&format!("- Q: {}\n  A: {}{note}\n", q.text.replace('\n', " "), answer_text(q, a).replace('\n', " ")));
        }
        // Journal d'appoint : une écriture ratée ne doit pas faire échouer la réponse rendue à l'agent.
        let _ = append(&dir.join(DECISIONS_FILE), &block);
    }
}

/// Fin d'une attente de l'humain (voir `Bus::wait_begin`).
struct WaitGuard<'a> {
    bus: &'a Bus,
    node: String,
}

impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        self.bus.wait_end(&self.node);
    }
}

/// Départ du demandeur côté serveur MCP : il n'envoie rien pendant qu'il attend, donc toute lecture
/// (ligne ou fin de flux) signale qu'il n'est plus là.
async fn gone(lines: &mut Lines<BufReader<OwnedReadHalf>>) {
    let _ = lines.next_line().await;
}

/// Le projet sort-il des commandes du bac à sable (`sandbox.excludedCommands` de ses réglages Claude Code) ?
/// Un fichier illisible ou mal formé compte comme « oui » : dans le doute, l'humain est consulté.
fn excludes_commands(project: &Path) -> bool {
    ["settings.json", "settings.local.json"].iter().any(|name| match std::fs::read_to_string(project.join(".claude").join(name)) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => v["sandbox"]["excludedCommands"].as_array().is_some_and(|a| !a.is_empty()),
            Err(_) => true,
        },
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    })
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    if f.metadata()?.len() == 0 {
        f.write_all(DECISIONS_HEADER.as_bytes())?;
    }
    f.write_all(text.as_bytes())
}

/// Se termine quand l'utilisateur met le run en pause ; une pause de limite d'abonnement n'interrompt pas une demande.
async fn user_pause(rx: &mut tokio::sync::watch::Receiver<Option<crate::runtime::pause::State>>) {
    loop {
        if rx.borrow_and_update().is_some_and(|s| s.why == Why::User) {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Questions lues dans les arguments de `ask_human` : textes bornés, entrées vides écartées.
pub fn parse_questions(args: &Value) -> Vec<HumanQuestion> {
    args["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|q| {
            let text = take_chars(q["text"].as_str()?.trim(), QUESTION_CHARS);
            if text.is_empty() {
                return None;
            }
            let options: Vec<String> = q["options"].as_array().into_iter().flatten().filter_map(|o| o.as_str()).map(|o| take_chars(o.trim(), OPTION_CHARS)).filter(|o| !o.is_empty()).take(MAX_OPTIONS).collect();
            let recommended = q["recommended"].as_u64().map(|i| i as usize).filter(|i| *i < options.len());
            Some(HumanQuestion { text, options, recommended })
        })
        .take(MAX_QUESTIONS)
        .collect()
}

/// Texte d'une réponse : le libellé de l'option choisie, le texte libre, ou le repli sans humain.
pub fn answer_text(q: &HumanQuestion, a: &HumanAnswer) -> String {
    match a {
        HumanAnswer::Option(i) => q.options.get(*i).cloned().unwrap_or_else(|| format!("option {}", i + 1)),
        HumanAnswer::Text(t) => t.trim().to_string(),
        HumanAnswer::Unavailable => match q.recommended.and_then(|i| q.options.get(i)) {
            Some(o) => format!("{o} (your recommended option: no human answered)"),
            None => "no answer: no human is available".to_string(),
        },
    }
}

/// Outil synthétique sous lequel `claude` demande l'accès à un domaine depuis le bac à sable (entrée : `host`).
const NETWORK_TOOL: &str = "SandboxNetworkAccess";

/// Signature d'une demande de permission, pour l'accord « pour la mission » : la commande exacte, le site
/// (et non l'URL : un site accordé l'est pour les commandes comme pour la lecture de pages), ou le fichier.
fn signature(tool: &str, input: &Value) -> String {
    match tool {
        "WebFetch" => format!("site:{}", input["url"].as_str().map(host_of).unwrap_or_default()),
        NETWORK_TOOL => format!("site:{}", input["host"].as_str().map(host_of).unwrap_or_default()),
        "Bash" => format!("Bash:{}", input["command"].as_str().map(String::from).unwrap_or_else(|| input.to_string())),
        _ => format!("{tool}:{}", input["file_path"].as_str().or_else(|| input["notebook_path"].as_str()).map(String::from).unwrap_or_else(|| input.to_string())),
    }
}

/// Hôte d'une URL ou d'un `hôte[:port]`, en minuscules et sans port.
fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.rsplit_once(':').filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit())).map_or(host, |(h, _)| h).to_ascii_lowercase()
}

/// Ce que l'humain lit avant d'accorder ou de refuser.
fn describe(tool: &str, input: &Value, l: Lang) -> String {
    match tool {
        "Bash" => {
            let outside = if input["dangerouslyDisableSandbox"] == true { l.t(" (hors du bac à sable)", " (outside the sandbox)") } else { "" };
            format!("{}{outside} :\n{}", l.t("Commande", "Command"), take_chars(input["command"].as_str().unwrap_or_default(), QUESTION_CHARS))
        }
        "WebFetch" => format!("{} : {}", l.t("Lire une page web", "Fetch a web page"), take_chars(input["url"].as_str().unwrap_or_default(), 300)),
        NETWORK_TOOL => format!("{} : {}", l.t("Accès réseau d'une commande, depuis le bac à sable", "Network access for a command, from inside the sandbox"), take_chars(input["host"].as_str().unwrap_or_default(), 300)),
        _ => match input["file_path"].as_str().or_else(|| input["notebook_path"].as_str()) {
            Some(p) => format!("{tool} {} : {p}", l.t("hors du projet", "outside the project")),
            None => format!("{tool} : {}", take_chars(&input.to_string(), 300)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lance un bus et rend aussi le récepteur des événements (l'« interface » du test).
    fn bus(notes: Option<PathBuf>) -> (Arc<Bus>, mpsc::UnboundedReceiver<RunEvent>, Arc<Gate>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let gate = Gate::new();
        (Bus::start(tx, notes, Lang::Fr, gate.clone()).unwrap(), rx, gate)
    }

    /// Ce que fait `essaim mcp` : une connexion, une requête, une réponse.
    async fn call(bus: &Bus, token: &str, tool: &str, args: Value) -> Option<Value> {
        let mut s = UnixStream::connect(&bus.path).await.unwrap();
        s.write_all(format!("{}\n", json!({"token": token, "node": "dev/realisation/work", "seat": "Dev", "tool": tool, "args": args})).as_bytes()).await.unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).ok()
    }

    async fn next_request(rx: &mut mpsc::UnboundedReceiver<RunEvent>) -> HumanRequest {
        match rx.recv().await {
            Some(RunEvent::HumanRequest(r)) => r,
            e => panic!("demande attendue, reçu {e:?}"),
        }
    }

    #[tokio::test]
    async fn question_posee_reponse_rendue_et_consignee() {
        let dir = std::env::temp_dir().join(format!("essaim-bus-{}", uuid::Uuid::new_v4()));
        let (b, mut rx, _) = bus(Some(dir.clone()));
        let (b2, token) = (b.clone(), b.token.clone());
        let client = tokio::spawn(async move {
            call(&b2, &token, "ask_human", json!({"kind": "question", "questions": [{"text": "snake_case ou camelCase ?", "options": ["snake_case", "camelCase"], "recommended": 0}, {"text": "Quel port ?"}]})).await
        });
        let req = next_request(&mut rx).await;
        assert_eq!((req.kind, req.seat.as_str(), req.node.as_str()), (AskKind::Question, "Dev", "dev/realisation/work"));
        assert_eq!(req.questions.len(), 2);
        assert_eq!(req.questions[0].recommended, Some(0));
        assert!(b.waited("dev/realisation/work").1, "le nœud est compté en attente");
        req.reply.send(vec![HumanAnswer::Option(1), HumanAnswer::Text("8080".into())]).unwrap();
        let r = client.await.unwrap().unwrap();
        assert_eq!(r["is_error"], false);
        let text = r["text"].as_str().unwrap();
        assert!(text.contains("Q: snake_case ou camelCase ?\nA: camelCase") && text.contains("A: 8080"), "{text}");
        assert!(!b.waited("dev/realisation/work").1, "attente terminée");
        let journal = std::fs::read_to_string(dir.join(DECISIONS_FILE)).unwrap();
        assert!(journal.contains("## Dev · question") && journal.contains("A: camelCase") && journal.contains("A: 8080"), "{journal}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sans_humain_la_question_prend_l_option_recommandee() {
        let (b, mut rx, _) = bus(None);
        let (b2, token) = (b.clone(), b.token.clone());
        let client = tokio::spawn(async move { call(&b2, &token, "ask_human", json!({"kind": "question", "questions": [{"text": "A ou B ?", "options": ["A", "B"], "recommended": 1}]})).await });
        let req = next_request(&mut rx).await;
        req.reply.send(req.unattended()).unwrap();
        let text = client.await.unwrap().unwrap()["text"].as_str().unwrap().to_string();
        assert!(text.contains("A: B"), "option recommandée retenue : {text}");
        // Une action sans humain : aucune réponse inventée, l'agent est renvoyé au statut bloqué.
        let (b2, token) = (b.clone(), b.token.clone());
        let client = tokio::spawn(async move { call(&b2, &token, "ask_human", json!({"kind": "action", "questions": [{"text": "Donne le jeton d'API"}]})).await });
        let req = next_request(&mut rx).await;
        assert_eq!(req.unattended(), vec![HumanAnswer::Unavailable]);
        req.reply.send(req.unattended()).unwrap();
        let text = client.await.unwrap().unwrap()["text"].as_str().unwrap().to_string();
        assert!(text.contains("no human is available") && text.contains("report status blocked"), "{text}");
    }

    #[tokio::test]
    async fn jeton_faux_ou_demande_vide_refuses() {
        let (b, mut rx, _) = bus(None);
        let r = call(&b, "mauvais", "ask_human", json!({"questions": [{"text": "x"}]})).await.unwrap();
        assert_eq!(r["is_error"], true);
        let r = call(&b, &b.token, "ask_human", json!({"questions": [{"text": "  "}]})).await.unwrap();
        assert_eq!(r["is_error"], true, "aucune question lisible");
        assert!(rx.try_recv().is_err(), "rien n'est demandé à l'humain");
    }

    #[tokio::test]
    async fn permission_une_fois_pour_la_mission_ou_refusee() {
        let (b, mut rx, _) = bus(None);
        let ask = |cmd: &'static str| {
            let (b2, token) = (b.clone(), b.token.clone());
            tokio::spawn(async move { call(&b2, &token, "permission", json!({"tool_name": "Bash", "input": {"command": cmd, "dangerouslyDisableSandbox": true}, "tool_use_id": "t"})).await })
        };
        let decision = |r: Option<Value>| -> Value { serde_json::from_str(r.unwrap()["text"].as_str().unwrap()).unwrap() };

        // Une fois : accordé, non retenu.
        let c = ask("curl https://exemple.fr");
        let req = next_request(&mut rx).await;
        assert_eq!(req.kind, AskKind::Permission);
        assert!(req.questions[0].text.contains("curl https://exemple.fr") && req.questions[0].text.contains("hors du bac à sable"), "{}", req.questions[0].text);
        assert_eq!(req.questions[0].options.len(), 3);
        req.reply.send(vec![HumanAnswer::Option(0)]).unwrap();
        let d = decision(c.await.unwrap());
        assert_eq!(d["behavior"], "allow");
        assert_eq!(d["updatedInput"]["command"], "curl https://exemple.fr", "l'entrée est rendue telle quelle");

        // Pour la mission : la même demande suivante ne remonte plus à l'humain.
        let c = ask("curl https://exemple.fr");
        next_request(&mut rx).await.reply.send(vec![HumanAnswer::Option(1)]).unwrap();
        assert_eq!(decision(c.await.unwrap())["behavior"], "allow");
        assert_eq!(decision(ask("curl https://exemple.fr").await.unwrap())["behavior"], "allow");
        assert!(rx.try_recv().is_err(), "accord retenu : aucune nouvelle demande");

        // Refus, puis refus avec consigne, puis absence d'humain : jamais d'accord par défaut.
        let c = ask("rm -rf ~/x");
        next_request(&mut rx).await.reply.send(vec![HumanAnswer::Option(2)]).unwrap();
        assert_eq!(decision(c.await.unwrap())["behavior"], "deny");
        let c = ask("rm -rf ~/x");
        next_request(&mut rx).await.reply.send(vec![HumanAnswer::Text("utilise le dossier tmp du projet".into())]).unwrap();
        let d = decision(c.await.unwrap());
        assert!(d["behavior"] == "deny" && d["message"].as_str().unwrap().contains("utilise le dossier tmp du projet"), "{d}");
        let c = ask("rm -rf ~/x");
        let req = next_request(&mut rx).await;
        req.reply.send(req.unattended()).unwrap();
        assert_eq!(decision(c.await.unwrap())["behavior"], "deny");
    }

    #[test]
    fn signature_par_commande_site_ou_fichier() {
        assert_eq!(signature("Bash", &json!({"command": "ls ~"})), "Bash:ls ~");
        assert_eq!(signature("WebFetch", &json!({"url": "https://Docs.RS/serde/latest?x=1"})), "site:docs.rs", "accord par site, pas par page");
        assert_eq!(signature(NETWORK_TOOL, &json!({"host": "docs.rs:443"})), "site:docs.rs", "même accord pour une commande et pour une page");
        assert_eq!(signature("Write", &json!({"file_path": "/tmp/a"})), "Write:/tmp/a");
        assert_ne!(signature("Bash", &json!({"command": "ls"})), signature("Bash", &json!({"command": "ls -la"})), "commande exacte");
    }

    /// Demande que `claude` adresse à son hôte (domaine réseau depuis le bac à sable) : elle va à l'humain,
    /// et un site accordé pour la mission l'est ensuite pour les commandes comme pour la lecture de pages.
    #[tokio::test]
    async fn domaine_reseau_demande_a_l_humain_puis_retenu_pour_la_mission() {
        let (b, mut rx, _) = bus(None);
        b.mcp_config("dev/realisation/work", "Dev", true);
        let b2 = b.clone();
        let c = tokio::spawn(async move { b2.permit("dev/realisation/work", NETWORK_TOOL, &json!({"host": "example.com"})).await });
        let req = next_request(&mut rx).await;
        assert_eq!((req.kind, req.seat.as_str()), (AskKind::Permission, "Dev"), "le poste est retrouvé par son nœud");
        assert!(req.questions[0].text.contains("example.com") && req.questions[0].text.contains("bac à sable"), "{}", req.questions[0].text);
        assert!(req.questions[0].options[1].contains("ce site"), "{:?}", req.questions[0].options);
        req.reply.send(vec![HumanAnswer::Option(1)]).unwrap();
        let d = c.await.unwrap();
        assert_eq!(d["behavior"], "allow");
        assert_eq!(d["updatedInput"]["host"], "example.com");
        // Retenu : ni la commande d'un autre poste ni la lecture d'une page du même site ne redemandent.
        assert_eq!(b.permit("autre/realisation/work", NETWORK_TOOL, &json!({"host": "example.com"})).await["behavior"], "allow");
        assert_eq!(b.permit("autre/realisation/work", "WebFetch", &json!({"url": "https://example.com/a"})).await["behavior"], "allow");
        assert!(rx.try_recv().is_err(), "aucune nouvelle demande");
        // Sans humain : refus, jamais d'attente sans fin.
        let b2 = b.clone();
        let c = tokio::spawn(async move { b2.permit("dev/realisation/work", NETWORK_TOOL, &json!({"host": "autre.example"})).await });
        let req = next_request(&mut rx).await;
        req.reply.send(req.unattended()).unwrap();
        assert_eq!(c.await.unwrap()["behavior"], "deny");
    }

    /// Une attente abandonnée (processus de l'agent arrêté) ne laisse pas le nœud « en attente de l'humain ».
    #[tokio::test]
    async fn attente_abandonnee_le_noeud_n_attend_plus() {
        let (b, mut rx, _) = bus(None);
        let b2 = b.clone();
        let c = tokio::spawn(async move { b2.permit("n", NETWORK_TOOL, &json!({"host": "example.com"})).await });
        let req = next_request(&mut rx).await;
        assert!(b.waited("n").1 && req.pending());
        c.abort();
        let _ = c.await;
        assert!(!b.waited("n").1, "l'attente se termine avec la tâche");
        assert!(!req.pending(), "la demande se referme pour l'interface");
    }

    /// Commande qui reste dans le bac à sable : accordée d'office quand le bac à sable couvre tout,
    /// demandée à l'humain sinon. `ask_human` ne passe jamais par une autorisation.
    #[tokio::test]
    async fn commande_dans_le_bac_a_sable_accordee_sans_demande() {
        let (b, mut rx, _) = bus(None);
        let dir = std::env::temp_dir().join(format!("essaim-bus-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let cmd = json!({"command": "[ \"$(cat a.txt)\" = x ] && echo ok"});

        // Tant que rien ne dit que le bac à sable couvre tout : l'humain est consulté.
        let (b2, cmd2) = (b.clone(), cmd.clone());
        let c = tokio::spawn(async move { b2.permit("n", "Bash", &cmd2).await });
        next_request(&mut rx).await.reply.send(vec![HumanAnswer::Option(2)]).unwrap();
        assert_eq!(c.await.unwrap()["behavior"], "deny");

        b.trust_sandbox(&dir);
        assert_eq!(b.permit("n", "Bash", &cmd).await["behavior"], "allow");
        assert_eq!(b.permit("n", "mcp__essaim__ask_human", &json!({"questions": []})).await["behavior"], "allow");
        assert!(rx.try_recv().is_err(), "aucune demande");
        // Hors du bac à sable : toujours demandé.
        let b2 = b.clone();
        let c = tokio::spawn(async move { b2.permit("n", "Bash", &json!({"command": "ls ~", "dangerouslyDisableSandbox": true})).await });
        next_request(&mut rx).await.reply.send(vec![HumanAnswer::Option(0)]).unwrap();
        assert_eq!(c.await.unwrap()["behavior"], "allow");

        // Le projet sort des commandes du bac à sable : une commande sans marque peut tourner dehors, donc on demande.
        std::fs::write(dir.join(".claude/settings.json"), r#"{"sandbox":{"excludedCommands":["docker"]}}"#).unwrap();
        b.trust_sandbox(&dir);
        let (b2, cmd2) = (b.clone(), cmd.clone());
        let c = tokio::spawn(async move { b2.permit("n", "Bash", &cmd2).await });
        next_request(&mut rx).await.reply.send(vec![HumanAnswer::Option(0)]).unwrap();
        assert_eq!(c.await.unwrap()["behavior"], "allow");
        // Réglages illisibles : dans le doute, on demande aussi.
        std::fs::write(dir.join(".claude/settings.json"), "{pas du json").unwrap();
        assert!(excludes_commands(&dir));
        std::fs::write(dir.join(".claude/settings.json"), r#"{"sandbox":{"excludedCommands":[]}}"#).unwrap();
        assert!(!excludes_commands(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Le demandeur disparaît pendant l'attente (agent arrêté) : la demande n'a plus cours pour l'interface.
    #[tokio::test]
    async fn demandeur_parti_la_demande_se_ferme() {
        let (b, mut rx, _) = bus(None);
        let mut s = UnixStream::connect(&b.path).await.unwrap();
        s.write_all(format!("{}\n", json!({"token": b.token, "node": "n", "seat": "Dev", "tool": "ask_human", "args": {"questions": [{"text": "x ?"}]}})).as_bytes()).await.unwrap();
        let req = next_request(&mut rx).await;
        assert!(req.pending());
        drop(s);
        tokio::time::timeout(Duration::from_secs(2), async {
            while req.pending() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("la demande doit se fermer quand le demandeur part");
        assert!(!b.waited("n").1);
    }

    /// Pause demandée à la main pendant une question : l'agent est prévenu et peut rendre son bilan.
    #[tokio::test]
    async fn pause_manuelle_libere_l_agent_en_attente() {
        let (b, mut rx, gate) = bus(None);
        let (b2, token) = (b.clone(), b.token.clone());
        let client = tokio::spawn(async move { call(&b2, &token, "ask_human", json!({"questions": [{"text": "x ?"}]})).await });
        let _req = next_request(&mut rx).await;
        gate.pause(Why::User, None);
        let text = client.await.unwrap().unwrap()["text"].as_str().unwrap().to_string();
        assert!(text.contains("paused by the user"), "{text}");
    }

    #[test]
    fn configuration_mcp_sans_jeton_dans_les_arguments() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (b, _rx, _) = rt.block_on(async { bus(None) });
        let v: Value = serde_json::from_str(&b.mcp_config("dev/realisation/work", "Dev", true)).unwrap();
        let srv = &v["mcpServers"]["essaim"];
        let args: Vec<&str> = srv["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
        assert_eq!(args[0], "mcp");
        assert!(args.contains(&"--permission") && args.contains(&"dev/realisation/work"), "{args:?}");
        assert!(!args.iter().any(|a| *a == b.token), "le jeton ne passe pas en argument");
        assert_eq!(srv["env"]["ESSAIM_BUS_TOKEN"], b.token);
        assert!(!serde_json::from_str::<Value>(&b.mcp_config("n", "s", false)).unwrap()["mcpServers"]["essaim"]["args"].as_array().unwrap().iter().any(|a| a == "--permission"));
    }
}
