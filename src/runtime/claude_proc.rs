use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Événement normalisé issu du flux `--output-format stream-json`.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeEvent {
    Init {
        session_id: String,
        model: String,
        /// "none" = authentification par abonnement (OAuth) ; autre = clé API.
        api_key_source: String,
    },
    Text(String),
    ToolUse { name: String, input: Value },
    ToolResult { is_error: bool },
    Result {
        text: String,
        is_error: bool,
        tokens: u64,
        duration_ms: u64,
        /// Sortie structurée (avec `--json-schema`), déjà validée par `claude`.
        structured: Option<Value>,
        /// Appels d'outils refusés par les permissions (« Bash(git status) »…) : jamais passés sous silence.
        denials: Vec<String>,
    },
    /// Ligne ignorée (rate_limit_event, etc.) ou non reconnue.
    Other,
}

/// « Outil(entrée) » d'un refus de permission, court et lisible.
fn denial_label(d: &Value) -> String {
    let tool = d["tool_name"].as_str().unwrap_or("?");
    let input = &d["tool_input"];
    let arg = ["command", "file_path", "path", "pattern"].iter().find_map(|k| input[*k].as_str()).unwrap_or_default();
    let arg: String = arg.chars().take(80).collect();
    if arg.is_empty() { tool.to_string() } else { format!("{tool}({arg})") }
}

/// Parse une ligne NDJSON en une liste d'événements (un message `assistant`
/// peut contenir plusieurs blocs).
pub fn parse_line(line: &str) -> Result<Vec<ClaudeEvent>> {
    let v: Value = serde_json::from_str(line).context("ligne stream-json invalide")?;
    let s = |x: &Value| x.as_str().unwrap_or_default().to_string();
    Ok(match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => vec![ClaudeEvent::Init {
            session_id: s(&v["session_id"]),
            model: s(&v["model"]),
            api_key_source: s(&v["apiKeySource"]),
        }],
        Some("assistant") => v["message"]["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| match b["type"].as_str() {
                        Some("text") => Some(ClaudeEvent::Text(s(&b["text"]))),
                        Some("tool_use") => Some(ClaudeEvent::ToolUse {
                            name: s(&b["name"]),
                            input: b["input"].clone(),
                        }),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some("user") => v["message"]["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "tool_result")
                    .map(|b| ClaudeEvent::ToolResult {
                        // `is_error` absent = résultat douteux : jamais un succès implicite (R2/R5).
                        is_error: b["is_error"].as_bool().unwrap_or(true),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some("result") => vec![ClaudeEvent::Result {
            text: s(&v["result"]),
            // Absent = échec (R2) : un résultat sans statut explicite n'est jamais compté comme un succès.
            is_error: v["is_error"].as_bool().unwrap_or(true),
            tokens:["input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
                .iter()
                .map(|k| v["usage"][*k].as_u64().unwrap_or(0))
                .sum(),
            duration_ms: v["duration_ms"].as_u64().unwrap_or(0),
            structured: Some(v["structured_output"].clone()).filter(|x| !x.is_null()),
            denials: v["permission_denials"].as_array().map(|a| a.iter().map(denial_label).collect()).unwrap_or_default(),
        }],
        _ => vec![ClaudeEvent::Other],
    })
}

/// Variables qui feraient basculer `claude` vers une facturation API / cloud tiers.
/// Elles sont retirées de l'environnement des processus `claude` lancés par essaim
/// (`claude_command` et terminal intégré) : l'outil ne doit jamais consommer que l'abonnement.
pub const BILLING_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// Préfixe des variables de sélection de fournisseur (Bedrock, Vertex, Foundry… et toute variante future).
pub const BILLING_ENV_PREFIX: &str = "CLAUDE_CODE_USE_";

/// Prédicat unique : la variable doit-elle être retirée de l'environnement d'un processus `claude` ?
/// Vrai pour la liste `BILLING_ENV_VARS` et pour tout nom commençant par `BILLING_ENV_PREFIX`.
pub fn is_billing_env(name: &str) -> bool {
    BILLING_ENV_VARS.contains(&name) || name.starts_with(BILLING_ENV_PREFIX)
}

/// Variables de facturation présentes dans l'environnement courant (à retirer avant tout lancement).
/// `vars_os` : une variable non UTF-8 ne fait pas paniquer, elle est simplement ignorée.
pub fn billing_env_to_remove() -> Vec<String> {
    std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).filter(|k| is_billing_env(k)).collect()
}

/// Commande `claude` : environnement hérité du processus, moins les variables de facturation
/// (`is_billing_env` : jamais de clé API ni de cloud tiers). Point d'entrée des lancements `claude -p`, `--bg`,
/// `claude agents`, `stop` et `rm` ; le terminal intégré (`tui::term`) applique le même retrait.
/// `ESSAIM_CLAUDE_BIN` permet de substituer le binaire (tests).
pub fn claude_command() -> Command {
    let mut cmd = Command::new(std::env::var("ESSAIM_CLAUDE_BIN").unwrap_or_else(|_| "claude".into()));
    for var in billing_env_to_remove() {
        cmd.env_remove(var);
    }
    cmd
}

/// Variables dont la valeur est un secret (masquée dans les textes affichés ou persistés).
const SECRET_VARS: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"];

/// Masquage « au mieux » des secrets connus dans un texte destiné à l'écran ou à un fichier :
/// jetons `sk-ant-…` et affectations `NOM_SECRET=valeur`. Ne remplace pas la prudence à la source.
pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            out.push_str(&redact_token(&token));
            token.clear();
            out.push(c);
        } else {
            token.push(c);
        }
    }
    out.push_str(&redact_token(&token));
    out
}

fn redact_token(t: &str) -> String {
    if let Some(i) = t.find("sk-ant-") {
        return format!("{}[secret masqué]", &t[..i]);
    }
    if let Some((k, _)) = t.split_once('=') {
        let name = k.trim_start_matches(['"', '\'', '`']);
        if SECRET_VARS.contains(&name) {
            return format!("{k}=[secret masqué]");
        }
    }
    t.to_string()
}

/// Délai maximal d'une commande courte (`claude agents`, `--bg`, `stop`, `rm`) : au-delà, le processus est tué.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// Délai maximal d'un nœud (`claude -p` ou session `--bg`) : au-delà, le travail est arrêté.
pub const NODE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Paramètres d'un lancement `claude -p`.
#[derive(Debug, Clone, Default)]
pub struct RunSpec {
    pub prompt: String,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub max_budget_usd: Option<f64>,
    /// Sous-agents injectés via `--agents` (nom → définition JSON).
    pub agents: BTreeMap<String, Value>,
    /// Agent principal de la session (`--agent`).
    pub agent: Option<String>,
    pub session_id: Option<String>,
    /// `--tools` : liste d'outils disponibles ; `Some("")` = aucun outil.
    pub tools: Option<String>,
    /// `--allowedTools` : règles autorisées d'office (ex. `Edit(src/**)`), séparées par des virgules à l'envoi.
    pub allowed_tools: Vec<String>,
    /// `--append-system-prompt` : fiche de poste ajoutée au prompt système.
    pub append_system_prompt: Option<String>,
    /// `--json-schema` : schéma de la sortie structurée.
    pub json_schema: Option<String>,
}

impl RunSpec {
    /// Options communes aux modes `-p` et `--bg`.
    fn common_args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec![];
        if let Some(m) = &self.model {
            a.extend(["--model".into(), m.clone()]);
        }
        if let Some(m) = &self.permission_mode {
            a.extend(["--permission-mode".into(), m.clone()]);
        }
        if !self.agents.is_empty() {
            // Sérialisation infaillible (`BTreeMap<String, Value>`) ; repli vide = `claude` refuse l'option (échec visible).
            a.extend(["--agents".into(), serde_json::to_string(&self.agents).unwrap_or_default()]);
        }
        if let Some(n) = &self.agent {
            a.extend(["--agent".into(), n.clone()]);
        }
        if let Some(t) = &self.tools {
            a.extend(["--tools".into(), t.clone()]);
        }
        if !self.allowed_tools.is_empty() {
            a.extend(["--allowedTools".into(), self.allowed_tools.join(",")]);
        }
        if let Some(p) = &self.append_system_prompt {
            a.extend(["--append-system-prompt".into(), p.clone()]);
        }
        if let Some(sc) = &self.json_schema {
            a.extend(["--json-schema".into(), sc.clone()]);
        }
        a
    }

    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose"].map(String::from).into();
        a.extend(self.common_args());
        if let Some(b) = self.max_budget_usd {
            a.extend(["--max-budget-usd".into(), b.to_string()]);
        }
        if let Some(id) = &self.session_id {
            a.extend(["--session-id".into(), id.clone()]);
        }
        // `--` : le prompt ne doit jamais être pris pour une option.
        a.push("--".into());
        a.push(self.prompt.clone());
        a
    }

}

/// Plafond de la sortie d'erreur conservée (octets). Au-delà, le reste est lu et jeté : le processus ne se bloque pas.
pub const STDERR_CAP: usize = 16 * 1024;

/// Lit `reader` jusqu'à la fin et ne garde que `cap` octets. Si la sortie est tronquée, le texte le dit
/// (troncature journalisée dans le texte d'erreur, sans canal supplémentaire).
async fn read_capped<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> String {
    let mut kept: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = buf.get(..n).unwrap_or_default();
                let room = cap.saturating_sub(kept.len());
                if chunk.len() > room {
                    truncated = true;
                }
                kept.extend_from_slice(chunk.get(..room.min(chunk.len())).unwrap_or_default());
            }
            // Erreur de lecture de stderr : fin de la capture. Sans effet sur le résultat (stdout reste la source).
            Err(_) => break,
        }
    }
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if truncated {
        text.push_str(&format!("\n[… sortie d'erreur tronquée à {cap} octets]"));
    }
    text
}

/// Envoie SIGKILL au groupe de processus `pid` (le processus lancé avec `process_group(0)` en est le chef).
/// Sans crate `libc` : commande `kill -KILL -- -<pgid>`. Échec ignoré : le processus peut déjà être terminé,
/// et `Child::kill` reste le repli direct.
#[cfg(unix)]
async fn kill_group(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", "--"])
        .arg(format!("-{pid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

#[cfg(not(unix))]
async fn kill_group(_pid: u32) {}

pub struct ClaudeProc {
    child: Child,
    /// Tâche de lecture de stderr (bornée) ; récupérée une fois le processus terminé.
    /// `allow(dead_code)` TEMPORAIRE : consommée par `executor::run_local` (étape c), hors périmètre de ce poste.
    #[allow(dead_code)]
    stderr: Option<JoinHandle<String>>,
}

impl ClaudeProc {
    /// Lance `claude` et pousse chaque événement dans `tx`. Le canal se ferme à la fin du flux.
    /// Si la session annonce une clé API (`apiKeySource` différent de `none`), émet un `Result` en erreur
    /// et arrête le processus (groupe, voir `kill`).
    /// `cmd` est normalement `claude_command()` ; un autre programme sert aux tests.
    /// Le processus est lancé dans son propre groupe (`process_group(0)`, unix) : `kill` atteint ses sous-processus.
    pub fn spawn_with(mut cmd: Command, spec: &RunSpec, tx: mpsc::UnboundedSender<ClaudeEvent>) -> Result<Self> {
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .args(spec.args())
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("cannot launch `claude` (is it in the PATH?)")?;
        let stdout = child.stdout.take().context("stdout indisponible")?;
        let stderr = child.stderr.take().map(|e| tokio::spawn(read_capped(e, STDERR_CAP)));
        let mut guard = child.id();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(evs) = parse_line(&line) {
                    for e in evs {
                        // Garde-fou : refuser toute session qui ne serait pas sur l'abonnement.
                        if let ClaudeEvent::Init { api_key_source, .. } = &e {
                            if api_key_source != "none" {
                                let _ = tx.send(ClaudeEvent::Result {
                                    text: format!(
                                        "refused: the session uses an API key (apiKeySource={api_key_source}), not the subscription"
                                    ),
                                    is_error: true,
                                    tokens: 0,
                                    duration_ms: 0,
                                    structured: None,
                                    denials: vec![],
                                });
                                if let Some(pid) = guard.take() {
                                    kill_group(pid).await;
                                }
                                return;
                            }
                        }
                        if tx.send(e).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(Self { child, stderr })
    }

    /// Arrête le processus **et ses descendants** (groupe, unix), puis attend sa fin : sans cela, un sous-processus
    /// lancé par `claude` survit à l'annulation (R4). `Child::kill` reste le repli si le groupe est introuvable.
    pub async fn kill(&mut self) -> Result<()> {
        // `id()` vaut None une fois le processus réapé : pas de signal vers un pid réutilisé.
        if let Some(pid) = self.child.id() {
            kill_group(pid).await;
        }
        self.child.kill().await.map_err(Into::into)
    }

    /// Sortie d'erreur capturée (bornée à `STDERR_CAP`), à appeler une fois le processus terminé ;
    /// vide si la tâche ne se termine pas dans 2 s (descendant qui garde stderr ouvert) : diagnostic best-effort.
    /// Les secrets connus sont masqués (`redact_secrets`).
    /// `allow(dead_code)` TEMPORAIRE : appelée par `executor::run_local` (étape c), hors périmètre de ce poste.
    #[allow(dead_code)]
    pub async fn stderr_text(&mut self) -> String {
        let Some(handle) = self.stderr.take() else {
            return String::new();
        };
        match tokio::time::timeout(Duration::from_secs(2), handle).await {
            Ok(Ok(text)) => redact_secrets(&text),
            // Tâche interrompue ou délai dépassé : pas de diagnostic, le code de sortie reste disponible.
            Ok(Err(_)) | Err(_) => String::new(),
        }
    }

    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        self.child.wait().await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fixture_reelle() {
        let src = include_str!("../../tests/fixtures/simple.jsonl");
        let evs: Vec<_> = src
            .lines()
            .flat_map(|l| parse_line(l).unwrap())
            .collect();
        assert!(matches!(&evs[0], ClaudeEvent::Init { model, api_key_source, .. }
            if model.contains("haiku") && api_key_source == "none"));
        assert!(evs.contains(&ClaudeEvent::Text("pong".into())));
        match evs.last().unwrap() {
            ClaudeEvent::Result { text, is_error, tokens, .. } => {
                assert_eq!(text, "pong");
                assert!(!is_error);
                assert!(*tokens > 0);
            }
            e => panic!("dernier événement inattendu : {e:?}"),
        }
    }

    #[test]
    fn parse_tool_use_et_result() {
        let l = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Read","input":{"file_path":"/x"}}]}}"#;
        let evs = parse_line(l).unwrap();
        assert_eq!(evs.len(), 2);
        assert!(matches!(&evs[1], ClaudeEvent::ToolUse { name, .. } if name == "Read"));
        let u = r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true}]}}"#;
        assert_eq!(parse_line(u).unwrap(), vec![ClaudeEvent::ToolResult { is_error: true }]);
    }

    #[test]
    fn args_prompt_apres_double_tiret() {
        let spec = RunSpec {
            prompt: "--rm -rf".into(),
            model: Some("haiku".into()),
            max_budget_usd: Some(0.2),
            ..Default::default()
        };
        let a = spec.args();
        assert_eq!(&a[a.len() - 2..], ["--", "--rm -rf"]);
        assert!(a.windows(2).any(|w| w == ["--model", "haiku"]));
    }

    #[test]
    fn options_de_poste_dans_les_arguments() {
        let spec = RunSpec {
            prompt: "p".into(),
            tools: Some(String::new()),
            allowed_tools: vec!["Edit(src/**)".into(), "Write(src/**)".into()],
            append_system_prompt: Some("fiche".into()),
            json_schema: Some("{}".into()),
            permission_mode: Some("dontAsk".into()),
            ..Default::default()
        };
        let a = spec.args();
        assert!(a.windows(2).any(|w| w == ["--tools", ""]), "aucun outil = chaîne vide");
        assert!(a.windows(2).any(|w| w == ["--allowedTools", "Edit(src/**),Write(src/**)"]));
        assert!(a.windows(2).any(|w| w == ["--append-system-prompt", "fiche"]));
        assert!(a.windows(2).any(|w| w == ["--json-schema", "{}"]));
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "dontAsk"]));
        assert_eq!(&a[a.len() - 2..], ["--", "p"], "le prompt reste après --");
    }

    #[test]
    fn sortie_structuree_dans_le_resultat() {
        let l = r#"{"type":"result","result":"{}","is_error":false,"total_cost_usd":0.01,"duration_ms":5,"structured_output":{"assignments":[{"to":"a","task":"t"}]}}"#;
        match &parse_line(l).unwrap()[0] {
            ClaudeEvent::Result { structured: Some(v), .. } => assert_eq!(v["assignments"][0]["to"], "a"),
            e => panic!("{e:?}"),
        }
        let l = r#"{"type":"result","result":"x","is_error":false}"#;
        assert!(matches!(&parse_line(l).unwrap()[0], ClaudeEvent::Result { structured: None, .. }));
    }

    #[test]
    fn variables_de_facturation_reconnues() {
        for v in BILLING_ENV_VARS {
            assert!(is_billing_env(v), "{v}");
        }
        assert!(is_billing_env("CLAUDE_CODE_USE_MANTLE"), "toute variante CLAUDE_CODE_USE_* est retirée");
        assert!(!is_billing_env("CLAUDE_CODE_USER_NAME"), "pas de faux positif sur un nom voisin");
        assert!(!is_billing_env("PATH") && !is_billing_env("HOME") && !is_billing_env("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    /// `essaim.sh` duplique la liste (`env -u … claude auth status`) : elle doit rester identique au prédicat.
    #[test]
    fn essaim_sh_retire_les_memes_variables() {
        let src = include_str!("../../essaim.sh");
        let start = src.find("env -u").expect("ligne `env -u` introuvable dans essaim.sh");
        let end = start + src[start..].find("claude auth status").expect("`claude auth status` introuvable");
        let tokens: Vec<&str> = src[start..end].split_whitespace().collect();
        let mut listed: Vec<&str> = tokens.windows(2).filter(|w| w[0] == "-u").map(|w| w[1]).collect();
        let mut expected: Vec<&str> = BILLING_ENV_VARS.to_vec();
        listed.sort();
        expected.sort();
        assert_eq!(listed, expected, "essaim.sh et BILLING_ENV_VARS divergent");
        assert!(listed.iter().all(|v| is_billing_env(v)));
    }

    #[test]
    fn masquage_des_secrets_connus() {
        let s = redact_secrets("erreur sk-ant-api03-XYZ123 fin\nANTHROPIC_API_KEY=abc123 ANTHROPIC_AUTH_TOKEN=\"tok\" ok");
        assert!(!s.contains("XYZ123") && !s.contains("abc123") && !s.contains("tok"), "{s}");
        assert!(s.contains("erreur") && s.contains("fin") && s.contains("ok"), "{s}");
        assert_eq!(redact_secrets("rien à voir\n  ici"), "rien à voir\n  ici");
    }

    #[tokio::test]
    async fn refus_cle_api_arrete_le_processus() {
        // Faux `claude` : annonce une session sur clé API puis reste en vie.
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(r#"echo '{"type":"system","subtype":"init","session_id":"s","model":"m","apiKeySource":"ANTHROPIC_API_KEY"}'; exec sleep 30"#);
        let spec = RunSpec { cwd: std::env::temp_dir(), ..Default::default() };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut p = ClaudeProc::spawn_with(cmd, &spec, tx).unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
        assert!(matches!(ev, ClaudeEvent::Result { is_error: true, .. }), "{ev:?}");
        tokio::time::timeout(Duration::from_secs(10), p.wait()).await.expect("processus non arrêté").unwrap();
    }

    #[tokio::test]
    #[ignore = "lance un vrai `claude` (coût ~0,001 $)"]
    async fn spawn_reel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let spec = RunSpec {
            prompt: "Réponds uniquement: pong".into(),
            cwd: std::env::temp_dir(),
            model: Some("haiku".into()),
            max_budget_usd: Some(0.2),
            ..Default::default()
        };
        let mut p = ClaudeProc::spawn_with(claude_command(), &spec, tx).unwrap();
        let mut got = None;
        while let Some(e) = rx.recv().await {
            if let ClaudeEvent::Result { text, .. } = e {
                got = Some(text);
            }
        }
        p.wait().await.unwrap();
        assert_eq!(got.as_deref().map(str::trim), Some("pong"));
    }
}
