use crate::runtime::claude_proc::{CLI_TIMEOUT, ClaudeEvent, ClaudeProc, PAUSE_GRACE, RunSpec, UsageLimit, budget_exhausted, claude_command, usage_limit};
use anyhow::{Context, Result, bail};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NodeStatus {
    Succeeded,
    Failed(String),
    /// La tâche n'a pas pu aboutir (tests rouges, refus de permission, prérequis absent…) : à corriger ou escalader.
    Blocked(String),
    Skipped(String),
    Cancelled,
}

/// Événements émis vers l'UI / la console pendant un run.
#[derive(Debug, Clone)]
pub enum RunEvent {
    /// Annonce un nœud créé dynamiquement (run d'entreprise) : libellé et niveau d'indentation.
    Label { id: String, label: String, agent: String, depth: usize },
    NodeStarted { id: String },
    /// Pause avant la réalisation : le plan consolidé attend une validation (`true` = lancer, `false` = arrêter).
    Checkpoint { plan: String, reply: mpsc::UnboundedSender<bool> },
    /// Pause du run : plus aucun appel `claude` ne part tant qu'elle dure (limite de l'abonnement atteinte,
    /// ou pause demandée). `resets_at` : instant de la reprise automatique, s'il est connu.
    Paused { why: crate::runtime::pause::Why, resets_at: Option<u64> },
    /// Reprise du run après une pause : annoncée une seule fois, quel que soit le nombre d'appels en attente.
    Resumed,
    /// Pause : le CEO n'a pas pu résoudre un blocage. L'utilisateur relance la correction, arrête, ou en discute d'abord avec le CEO.
    Escalation { summary: String, reply: mpsc::UnboundedSender<EscalationReply> },
    Node { id: String, event: ClaudeEvent },
    NodeFinished { id: String, status: NodeStatus, tokens: u64 },
    RunFinished { ok: bool, tokens: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationReply {
    /// Nouveau tour de correction du CEO (avec la décision de l'utilisateur, s'il y en a une).
    Retry,
    Stop,
}

/// Tâche confiée à un `Runner`.
#[derive(Debug, Clone)]
pub struct NodeJob {
    /// Identifiant du nœud : fait partie du contrat du `Runner` (les runners injectés des tests de
    /// `company_exec` l'utilisent pour router les scénarios) mais le runner réel ne le lit pas.
    #[allow(dead_code)]
    pub id: String,
    pub spec: RunSpec,
}

/// Résultat d'un nœud (`crate::runtime::executor::NodeResult`). Contrat d'interface : `cached: u64` et
/// `duration_ms: u64` sont renseignés par `run_local` depuis l'événement `Result` du flux ; ils valent 0 par
/// défaut (`Default`), donc les constructions `NodeResult { .., ..Default::default() }` existantes restent valides.
#[derive(Debug, Clone, Default)]
pub struct NodeResult {
    pub output: String,
    pub tokens: u64,
    pub is_error: bool,
    /// Sortie structurée si le nœud utilisait `--json-schema`.
    pub structured: Option<serde_json::Value>,
    /// Appels refusés par les permissions pendant ce nœud.
    pub denials: Vec<String>,
    /// Session `claude` de l'appel (pour la reprise `--resume`) ; absente si le flux ne l'a pas annoncée.
    pub session_id: Option<String>,
    /// Jetons servis par le cache de prompt (`cache_read_input_tokens`), part de `tokens` ; 0 si inconnu.
    /// `allow(dead_code)` TEMPORAIRE : lu par `company_exec` (agent-opt) une fois branché.
    #[allow(dead_code)]
    pub cached: u64,
    /// Durée de l'appel annoncée par le flux `claude` (`duration_ms` du résultat), en ms ; 0 si inconnue.
    #[allow(dead_code)]
    pub duration_ms: u64,
    /// Limite de l'abonnement atteinte : l'appel n'a rien produit et n'est pas un échec du poste.
    /// L'appelant met le run en pause et rejoue l'appel à la reprise.
    pub limit: Option<UsageLimit>,
    /// Arrêt demandé par la pause pendant l'appel : le poste **n'échoue pas**, et l'appelant reprend
    /// la session (`--resume`) après la reprise du run. Voir `Interrupted`.
    pub interrupted: Option<Interrupted>,
}

/// Un appel arrêté en vol par la pause. Le travail déjà fait n'est pas perdu : la session `claude`
/// garde tout le contexte, et `note` est le bilan que l'agent a rendu avant de s'arrêter (vide s'il
/// a fallu le tuer sans qu'il réponde).
#[derive(Debug, Clone, Default)]
pub struct Interrupted {
    /// Session à reprendre (`--resume`) ; absente si le flux ne l'avait pas encore annoncée.
    pub session: Option<String>,
    /// Bilan rendu par l'agent : ce qui est fait, ce qui reste.
    pub note: String,
    /// L'agent n'a pas répondu dans le délai de grâce et a été tué.
    pub killed: bool,
}

/// Exécute un nœud. Injectable pour tester l'ordonnancement sans lancer `claude`.
/// Signal de pause vu par un appel **en cours** : `Some` = arrêt demandé à l'agent en vol.
pub type PauseSignal = watch::Receiver<Option<crate::runtime::pause::State>>;

pub type Runner = Arc<
    dyn Fn(
            NodeJob,
            mpsc::UnboundedSender<ClaudeEvent>,
            watch::Receiver<bool>,
            PauseSignal,
        ) -> Pin<Box<dyn Future<Output = Result<NodeResult>> + Send>>
        + Send
        + Sync,
>;

/// Se termine quand l'annulation est demandée ; reste en attente si l'émetteur a disparu.
pub async fn cancelled(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Runner réel : `claude -p` en local. `node_timeout` borne la durée d'un appel.
pub fn claude_runner(node_timeout: Duration) -> Runner {
    Arc::new(move |job, tx, cancel, pause| Box::pin(async move { run_local(claude_command(), &job.spec, tx, cancel, pause, node_timeout).await }))
}

/// Prompt prioritaire poussé dans la session d'un agent en vol quand le run passe en pause.
/// Il demande un arrêt **reprenable** : terminer l'action en cours sans rien laisser à moitié fait,
/// puis rendre un bilan court qui servira de point de reprise. En anglais, comme les autres
/// instructions envoyées aux postes ; l'agent répond dans la langue de l'utilisateur.
pub const PAUSE_STOP_PROMPT: &str = "STOP REQUESTED BY THE USER (priority message). Do not start any new action, do not open a new file, do not launch a new command. Finish or cleanly abandon the one action in flight — never leave a file half-written or a change half-applied. Then answer immediately, without the structured output, with a short handover: what you completed, what is left to do, and exactly where you stopped. Your session will be resumed later from this point, so write what your future self needs to continue.";

/// Note portée au journal du nœud au moment où l'arrêt est demandé (visible dans la TUI).
fn stop_notice() -> &'static str {
    crate::tui::settings::tr("⏸ arrêt demandé : l'agent termine son action en cours", "⏸ stop requested: the agent is finishing its current action")
}

/// Se termine quand l'utilisateur demande la pause ; reste en attente si l'émetteur a disparu.
///
/// Seule une pause **demandée à la main** arrête un agent en vol. Une pause de limite d'abonnement
/// laisse les appels en cours aller au bout : ils sont déjà partis, et s'ils butent eux aussi sur la
/// limite ils repassent par le chemin de rejeu, qui ne consomme pas d'appel. Les interrompre ne
/// gagnerait rien et ferait perdre leur travail.
async fn pause_requested(rx: &mut PauseSignal) {
    loop {
        if rx.borrow_and_update().is_some_and(|s| s.why == crate::runtime::pause::Why::User) {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Lance `cmd` (`claude -p`) et relaie ses événements. Le processus est tué à l'annulation,
/// au dépassement de `limit`, ou si sa sortie ne se termine pas dans `CLI_TIMEOUT` après le résultat.
///
/// Pause : quand `pause` passe à `Some`, un prompt prioritaire part sur stdin pour que l'agent
/// s'arrête **de façon reprenable** (il termine son action en vol et rend un bilan). S'il ne répond
/// pas dans `PAUSE_GRACE`, il est tué. Dans les deux cas le résultat porte `interrupted` : le poste
/// n'échoue pas et l'appelant reprend la session après la reprise du run.
pub async fn run_local(
    cmd: Command,
    spec: &RunSpec,
    tx: mpsc::UnboundedSender<ClaudeEvent>,
    mut cancel: watch::Receiver<bool>,
    mut pause: PauseSignal,
    limit: Duration,
) -> Result<NodeResult> {
    let (ctx, mut crx) = mpsc::unbounded_channel();
    let mut proc = ClaudeProc::spawn_with(cmd, spec, ctx)?;
    proc.send_prompt().await.context("prompt non transmis à `claude`")?;
    let mut res = NodeResult { is_error: true, output: "no result received".into(), ..Default::default() };
    // Arrêt demandé : on n'envoie le prompt d'arrêt qu'une fois, puis on compte le délai de grâce.
    let mut stopping = false;
    let mut got_result = false;
    let deadline = tokio::time::sleep(limit);
    // Armé seulement quand l'arrêt est demandé (`reset`) : d'ici là, une échéance hors de portée.
    let grace = tokio::time::sleep(Duration::from_secs(365 * 24 * 3600));
    tokio::pin!(deadline, grace);
    loop {
        tokio::select! {
            ev = crx.recv() => match ev {
                Some(ev) => {
                    match &ev {
                        ClaudeEvent::Init { session_id, .. } if !session_id.is_empty() => res.session_id = Some(session_id.clone()),
                        ClaudeEvent::Init { .. } => res.session_id = None,
                        ClaudeEvent::Result { text, is_error, tokens, cached, duration_ms, structured, denials, .. } => {
                            got_result = true;
                            res = NodeResult {
                                output: text.clone(),
                                tokens: *tokens,
                                is_error: *is_error,
                                structured: structured.clone(),
                                denials: denials.clone(),
                                session_id: res.session_id.take(),
                                cached: *cached,
                                duration_ms: *duration_ms,
                                limit: None,
                                interrupted: None,
                            };
                            // Le résultat est là : stdin n'a plus d'utilité et sa fermeture laisse
                            // `claude` sortir (sinon il attend d'autres messages indéfiniment).
                            proc.close_stdin();
                        }
                        _ => {}
                    }
                    let _ = tx.send(ev);
                }
                None => break,
            },
            // Pause demandée pendant l'appel : prompt prioritaire, puis délai de grâce.
            _ = pause_requested(&mut pause), if !stopping && !got_result => {
                stopping = true;
                let _ = tx.send(ClaudeEvent::Text(stop_notice().into()));
                // Échec d'écriture = processus déjà parti : le délai de grâce mènera au kill.
                proc.send_user(PAUSE_STOP_PROMPT).await.ok();
                // Plus rien à envoyer : fermer stdin autorise `claude` à sortir dès son bilan rendu.
                proc.close_stdin();
                grace.as_mut().reset(tokio::time::Instant::now() + PAUSE_GRACE);
            }
            _ = &mut grace => {
                proc.kill().await.ok();
                res.interrupted = Some(Interrupted { session: res.session_id.clone(), note: String::new(), killed: true });
                return Ok(res);
            }
            _ = cancelled(&mut cancel) => {
                proc.kill().await.ok();
                bail!("cancelled");
            }
            _ = &mut deadline => {
                proc.kill().await.ok();
                bail!("timeout ({} s): process stopped", limit.as_secs());
            }
        }
    }
    if tokio::time::timeout(CLI_TIMEOUT, proc.wait()).await.is_err() {
        proc.kill().await.ok();
    }
    // Limite de l'abonnement : reconnue dans le résultat, sinon dans la sortie d'erreur (le processus peut
    // s'arrêter sans jamais produire de résultat). La lecture de stderr n'a lieu que sur un appel en erreur.
    if res.is_error {
        let err = proc.stderr_text().await;
        res.limit = usage_limit(&res.output).or_else(|| usage_limit(&err));
        // Plafond de budget épuisé : dit explicitement, sinon le poste paraît avoir raté son travail.
        // Pas de rejeu : seul un `max_budget_usd` plus haut changerait quelque chose.
        if res.limit.is_none() && (budget_exhausted(&res.output) || budget_exhausted(&err)) {
            res.output = format!(
                "budget cap reached for this call (max_budget_usd = {}): raise it for this seat or the company, the work was cut short, not failed",
                spec.max_budget_usd.map(|b| b.to_string()).unwrap_or_else(|| "?".into())
            );
        }
    }
    // Arrêt demandé et bilan rendu : l'appel n'a pas abouti, il reprendra sur la même session.
    // La limite d'abonnement garde la priorité : elle a son propre chemin de reprise, déjà éprouvé.
    if stopping && res.limit.is_none() {
        res.interrupted = Some(Interrupted { session: res.session_id.clone(), note: res.output.clone(), killed: false });
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delai_depasse_arrete_le_processus() {
        // Faux `claude` qui ne répond jamais : le délai doit interrompre le nœud.
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("exec sleep 30");
        let spec = RunSpec { cwd: std::env::temp_dir(), ..Default::default() };
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_keep, cancel) = watch::channel(false);
        let (_keep_p, pause) = watch::channel(None);
        let t0 = std::time::Instant::now();
        let err = run_local(cmd, &spec, tx, cancel, pause, Duration::from_millis(200)).await.unwrap_err();
        assert!(err.to_string().contains("timeout"), "{err}");
        assert!(t0.elapsed() < Duration::from_secs(10), "le processus doit être arrêté sans attendre sa fin");
    }

    /// Faux `claude` qui lit le prompt d'arrêt sur stdin et s'arrête en rendant son bilan : c'est le
    /// chemin nominal de la pause. Le poste **n'échoue pas** et la session est rendue pour la reprise.
    #[tokio::test]
    async fn la_pause_arrete_l_agent_en_vol_de_maniere_reprenable() {
        let mut cmd = Command::new("sh");
        // Annonce sa session, puis attend un second message sur stdin (le prompt d'arrêt) avant de
        // rendre un résultat : sans arrêt demandé, il ne rend rien avant le délai.
        cmd.arg("-c").arg(
            r#"printf '{"type":"system","subtype":"init","session_id":"sid-42","model":"fake","apiKeySource":"none"}\n'
               read -r _premier
               read -r _arret
               printf '{"type":"result","is_error":false,"result":"Fait : A. Reste : B.","usage":{"input_tokens":1,"output_tokens":1}}\n'"#,
        );
        let spec = RunSpec { prompt: "tâche".into(), cwd: std::env::temp_dir(), ..Default::default() };
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_keep, cancel) = watch::channel(false);
        let (gate, pause) = watch::channel(None);
        // La pause tombe pendant que l'appel tourne.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = gate.send(Some(crate::runtime::pause::State { why: crate::runtime::pause::Why::User, resets_at: None }));
        });
        let res = run_local(cmd, &spec, tx, cancel, pause, Duration::from_secs(20)).await.expect("pas une erreur : un arrêt reprenable");
        let it = res.interrupted.expect("le résultat doit porter l'interruption");
        assert_eq!(it.session.as_deref(), Some("sid-42"), "session rendue pour le --resume");
        assert!(!it.killed, "l'agent a répondu de lui-même, pas besoin de le tuer");
        assert!(it.note.contains("Reste : B."), "le bilan de reprise est conservé : {:?}", it.note);
        assert!(!res.is_error, "un arrêt demandé n'est pas une erreur du poste");
    }

    /// Une pause de **limite d'abonnement** ne touche pas aux appels déjà lancés : ils vont au bout.
    #[tokio::test]
    async fn la_pause_de_limite_laisse_finir_l_appel_en_cours() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(
            r#"printf '{"type":"system","subtype":"init","session_id":"sid-7","model":"fake","apiKeySource":"none"}\n'
               read -r _premier
               sleep 0.3
               printf '{"type":"result","is_error":false,"result":"fini","usage":{"input_tokens":1,"output_tokens":1}}\n'"#,
        );
        let spec = RunSpec { prompt: "tâche".into(), cwd: std::env::temp_dir(), ..Default::default() };
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_keep, cancel) = watch::channel(false);
        let (gate, pause) = watch::channel(None);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = gate.send(Some(crate::runtime::pause::State { why: crate::runtime::pause::Why::Limit, resets_at: None }));
        });
        let res = run_local(cmd, &spec, tx, cancel, pause, Duration::from_secs(20)).await.unwrap();
        assert!(res.interrupted.is_none(), "une pause de limite n'interrompt pas l'appel en vol");
        assert_eq!(res.output, "fini");
    }
}
