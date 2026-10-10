use crate::runtime::claude_proc::{CLI_TIMEOUT, ClaudeEvent, ClaudeProc, NODE_TIMEOUT, RunSpec, claude_command};
use anyhow::{Result, bail};
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

impl NodeStatus {
    pub fn is_ok(&self) -> bool {
        matches!(self, NodeStatus::Succeeded)
    }
}

/// Événements émis vers l'UI / la console pendant un run.
#[derive(Debug, Clone)]
pub enum RunEvent {
    /// Annonce un nœud créé dynamiquement (run d'entreprise) : libellé et niveau d'indentation.
    Label { id: String, label: String, agent: String, depth: usize },
    NodeStarted { id: String },
    /// Pause avant la réalisation : le plan consolidé attend une validation (`true` = lancer, `false` = arrêter).
    Checkpoint { plan: String, reply: mpsc::UnboundedSender<bool> },
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
    pub id: String,
    pub spec: RunSpec,
}

#[derive(Debug, Clone, Default)]
pub struct NodeResult {
    pub output: String,
    pub tokens: u64,
    pub is_error: bool,
    /// Sortie structurée si le nœud utilisait `--json-schema`.
    pub structured: Option<serde_json::Value>,
    /// Appels refusés par les permissions pendant ce nœud.
    pub denials: Vec<String>,
}

/// Exécute un nœud. Injectable pour tester l'ordonnancement sans lancer `claude`.
pub type Runner = Arc<
    dyn Fn(
            NodeJob,
            mpsc::UnboundedSender<ClaudeEvent>,
            watch::Receiver<bool>,
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

/// Runner réel : `claude -p` en local.
pub fn claude_runner() -> Runner {
    Arc::new(|job, tx, cancel| Box::pin(async move { run_local(claude_command(), &job.spec, tx, cancel, NODE_TIMEOUT).await }))
}

/// Lance `cmd` (`claude -p`) et relaie ses événements. Le processus est tué à l'annulation,
/// au dépassement de `limit`, ou si sa sortie ne se termine pas dans `CLI_TIMEOUT` après le résultat.
pub async fn run_local(
    cmd: Command,
    spec: &RunSpec,
    tx: mpsc::UnboundedSender<ClaudeEvent>,
    mut cancel: watch::Receiver<bool>,
    limit: Duration,
) -> Result<NodeResult> {
    let (ctx, mut crx) = mpsc::unbounded_channel();
    let mut proc = ClaudeProc::spawn_with(cmd, spec, ctx)?;
    let mut res = NodeResult { is_error: true, output: "no result received".into(), ..Default::default() };
    let deadline = tokio::time::sleep(limit);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            ev = crx.recv() => match ev {
                Some(ev) => {
                    if let ClaudeEvent::Result { text, is_error, tokens, structured, denials, .. } = &ev {
                        res = NodeResult { output: text.clone(), tokens: *tokens, is_error: *is_error, structured: structured.clone(), denials: denials.clone() };
                    }
                    let _ = tx.send(ev);
                }
                None => break,
            },
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
        let t0 = std::time::Instant::now();
        let err = run_local(cmd, &spec, tx, cancel, Duration::from_millis(200)).await.unwrap_err();
        assert!(err.to_string().contains("timeout"), "{err}");
        assert!(t0.elapsed() < Duration::from_secs(10), "le processus doit être arrêté sans attendre sa fin");
    }
}
