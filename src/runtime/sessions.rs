use super::claude_proc::{CLI_TIMEOUT, claude_command, redact_secrets};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

/// Une session `claude` listée par `claude agents --json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Session {
    pub id: Option<String>,
    pub pid: Option<u32>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub kind: String,
    #[serde(rename = "startedAt")]
    pub started_at: Option<u64>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Working,
    Done,
    Failed(String),
    /// État non répertorié (ex. en attente d'une réponse) : on continue de surveiller.
    Other(String),
}

impl Session {
    pub fn phase(&self) -> Phase {
        match self.state.as_deref() {
            Some("working") => Phase::Working,
            Some("done") => Phase::Done,
            Some(s @ ("failed" | "error" | "crashed" | "killed")) => Phase::Failed(s.into()),
            Some(s) => Phase::Other(s.into()),
            None => Phase::Other("unknown".into()),
        }
    }

    pub fn label(&self) -> String {
        self.name.clone().or_else(|| self.id.clone()).unwrap_or_else(|| "?".into())
    }

    /// Session créée par cet outil (convention de nommage).
    pub fn is_essaim(&self) -> bool {
        self.name.as_deref().is_some_and(|n| n.starts_with("essaim-"))
    }
}

/// Ne garde que les sessions background (les interactives n'ont ni id ni état).
pub fn parse_sessions(json: &str) -> Result<Vec<Session>> {
    let all: Vec<Session> = serde_json::from_str(json).context("sortie de `claude agents --json` illisible")?;
    Ok(all.into_iter().filter(|s| s.kind == "background" && s.id.is_some()).collect())
}

/// `cmd.output()` borné par `limit` : au dépassement, le future est abandonné et le processus tué (kill_on_drop).
async fn output_limited(mut cmd: Command, limit: Duration) -> Result<std::process::Output> {
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(limit, cmd.output()).await.with_context(|| format!("timeout ({} s)", limit.as_secs()))?;
    Ok(out?)
}

async fn run(args: &[&str], cwd: Option<&PathBuf>) -> Result<String> {
    let mut cmd = claude_command();
    cmd.args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let out = output_limited(cmd, CLI_TIMEOUT).await.context("impossible de lancer `claude`")?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        // Sortie brute de `claude` : secrets éventuels masqués avant affichage.
        bail!("`claude {}` failed: {}", args.first().copied().unwrap_or(""), redact_secrets(text.trim()));
    }
    Ok(text)
}

/// Identifiant de session sûr (passé à `claude` ou joint à un chemin) : lettres, chiffres, `-` et `_`,
/// sans séparateur, sans `..` et sans tiret initial (qui serait lu comme une option).
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('-') && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub async fn list() -> Result<Vec<Session>> {
    parse_sessions(&run(&["agents", "--json", "--all"], None).await?)
}

/// Transcription d'une session : `~/.claude/projects/<dossier>/<sessionId>.jsonl`.
pub fn find_transcript(session_id: &str) -> Option<PathBuf> {
    if !is_valid_id(session_id) {
        return None;
    }
    let base = directories::UserDirs::new()?.home_dir().join(".claude/projects");
    std::fs::read_dir(base).ok()?.flatten().map(|d| d.path().join(format!("{session_id}.jsonl"))).find(|p| p.is_file())
}

pub async fn stop(id: &str) -> Result<()> {
    anyhow::ensure!(is_valid_id(id), "identifiant de session invalide : {id:?}");
    run(&["stop", id], None).await.map(|_| ())
}

pub async fn remove(id: &str) -> Result<()> {
    anyhow::ensure!(is_valid_id(id), "identifiant de session invalide : {id:?}");
    run(&["rm", id], None).await.map(|_| ())
}

/// Lignes lisibles de la transcription, pour l'onglet Sessions.
pub fn transcript_lines(src: &str) -> Vec<String> {
    let mut out = vec![];
    for l in src.lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
        match v["type"].as_str() {
            Some("user") => {
                if let Some(t) = v["message"]["content"].as_str() {
                    out.push(format!("› {t}"));
                }
            }
            Some("assistant") => {
                for b in v["message"]["content"].as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some("text") => out.extend(b["text"].as_str().unwrap_or("").lines().map(String::from)),
                        Some("tool_use") => out.push(format!("⚙ outil : {}", b["name"].as_str().unwrap_or("?"))),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENTS_JSON: &str = r#"[
      {"pid":26204,"cwd":"/p","kind":"interactive","startedAt":1,"sessionId":"aaa","name":"main","status":"busy"},
      {"pid":27745,"id":"6b8013c8","cwd":"/p","kind":"background","startedAt":1791460891217,"sessionId":"6b8013c8-c1a6","name":"essaim-probe","status":"idle","state":"done"},
      {"pid":27746,"id":"7c9024d9","cwd":"/p","kind":"background","startedAt":2,"sessionId":"7c90","name":"autre","status":"busy","state":"working"}
    ]"#;

    #[test]
    fn liste_ne_garde_que_le_background() {
        let s = parse_sessions(AGENTS_JSON).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].phase(), Phase::Done);
        assert!(s[0].is_essaim());
        assert_eq!(s[1].phase(), Phase::Working);
        assert!(!s[1].is_essaim());
        assert_eq!(s[1].label(), "autre");
    }

    #[test]
    fn etats_inconnus_sont_surveilles() {
        let mut s = parse_sessions(AGENTS_JSON).unwrap().remove(0);
        s.state = Some("needs-input".into());
        assert_eq!(s.phase(), Phase::Other("needs-input".into()));
        s.state = Some("failed".into());
        assert_eq!(s.phase(), Phase::Failed("failed".into()));
    }

    const TRANSCRIPT: &str = concat!(
        r#"{"type":"custom-title","title":"x"}"#, "\n",
        r#"{"type":"user","message":{"role":"user","content":"Réponds: pong"}}"#, "\n",
        r#"{"type":"attachment"}"#, "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}],"stop_reason":"tool_use"}}"#, "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","is_error":false}]}}"#, "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"pong\nfin"}],"stop_reason":"end_turn"}}"#, "\n",
        r#"{"type":"last-prompt"}"#, "\n",
    );

    #[test]
    fn lignes_lisibles() {
        let l = transcript_lines(TRANSCRIPT);
        assert_eq!(l, vec!["› Réponds: pong", "⚙ outil : Bash", "pong", "fin"]);
    }

    #[test]
    fn identifiants_de_session_refuses() {
        assert!(is_valid_id("6b8013c8-c1a6") && is_valid_id("7c9024d9"));
        for bad in ["../../secret", "--all", "a/b", "", "a b", "x\\y"] {
            assert!(!is_valid_id(bad), "{bad:?} doit être refusé");
        }
        // Un identifiant hostile ne doit jamais atteindre le système de fichiers ni la commande `claude`.
        assert!(find_transcript("../../etc/passwd").is_none());
        assert!(find_transcript("").is_none());
    }

    #[tokio::test]
    async fn commande_courte_bornee_par_un_delai() {
        // Un `claude` bloqué ne doit pas bloquer l'outil : le délai interrompt l'attente et tue le processus.
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let t0 = std::time::Instant::now();
        let err = output_limited(cmd, Duration::from_millis(200)).await.unwrap_err();
        assert!(err.to_string().contains("timeout"), "{err:#}");
        assert!(t0.elapsed() < Duration::from_secs(10));
    }

}
