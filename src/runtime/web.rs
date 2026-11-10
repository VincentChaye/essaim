//! Tableau de bord local : une page web qui montre où partent les jetons.
//!
//! Serveur volontairement minuscule, posé sur la boucle tokio déjà présente :
//! - écoute sur `127.0.0.1` seulement (jamais une adresse joignable depuis le réseau) ;
//! - une clé tirée au hasard est exigée à chaque requête (`?k=…`), page comprise : un autre
//!   processus de la machine ne lit pas vos dossiers de projet par simple curiosité ;
//! - l'en-tête `Host` doit désigner la boucle locale (garde-fou contre le *DNS rebinding*) ;
//! - `GET` et `HEAD` uniquement, en-têtes bornés, aucune écriture : la page ne fait que lire.
//!
//! Les chiffres viennent de trois sources : les transcriptions de Claude Code (`usage`), la liste des
//! sessions `claude` vivantes (`claude agents --json --all`) et les traces de runs d'essaim
//! (`.essaim/runs`), qui disent quel poste a dépensé quoi.

use crate::runtime::live;
use crate::runtime::sessions::{self, Session};
use crate::runtime::store::{self, RunRecord};
use crate::runtime::usage::Scanner;
use crate::tui::settings::Lang;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Page du tableau de bord (aucune ressource distante : la page fonctionne hors ligne).
const PAGE: &str = include_str!("web/page.html");

/// Port par défaut, puis les suivants s'il est déjà pris.
pub const DEFAULT_PORT: u16 = 8787;
const PORT_TRIES: u16 = 16;

/// Taille maximale des en-têtes d'une requête.
const MAX_HEAD: usize = 16 * 1024;

/// Durée de validité de la liste des sessions vivantes (un appel `claude agents` coûte une seconde).
const LIVE_TTL_MS: u64 = 4_000;

/// Runs d'essaim relus pour la page (les plus récents).
const MAX_RUNS: usize = 40;

/// Tableau de bord en service.
pub struct Dashboard {
    /// Adresse écoutée (le port est aussi dans `url`). `allow(dead_code)` : lue par les tests et par
    /// un appelant qui voudrait l'afficher autrement.
    #[allow(dead_code)]
    pub addr: SocketAddr,
    /// URL complète, clé comprise : à ouvrir dans un navigateur.
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Dashboard {
    /// Le serveur s'arrête avec son `Dashboard` (les connexions en cours sont abandonnées).
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct State {
    root: PathBuf,
    lang: Lang,
    key: String,
    scan: Mutex<Scanner>,
    live: tokio::sync::Mutex<Live>,
    /// Faux dans les tests : aucune commande `claude agents` n'est lancée.
    live_enabled: bool,
}

#[derive(Default)]
struct Live {
    at_ms: u64,
    sessions: Vec<Session>,
    error: Option<String>,
}

/// Démarre le serveur et renvoie son URL. `port` = 0 : `DEFAULT_PORT`, puis les ports suivants.
pub async fn start(root: PathBuf, lang: Lang, port: u16) -> Result<Dashboard> {
    start_inner(root, lang, port, true).await
}

async fn start_inner(root: PathBuf, lang: Lang, port: u16, live_enabled: bool) -> Result<Dashboard> {
    let first = if port == 0 { DEFAULT_PORT } else { port };
    let mut last = None;
    let mut bound = None;
    // Un port occupé n'est pas une erreur : on prend le suivant (sauf si l'utilisateur l'a imposé).
    let tries = if port == 0 { PORT_TRIES } else { 1 };
    for p in first..first.saturating_add(tries) {
        match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, p)).await {
            Ok(l) => {
                bound = Some(l);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let listener = match bound {
        Some(l) => l,
        None => return Err(last.map(anyhow::Error::new).unwrap_or_else(|| anyhow::anyhow!("aucun port libre"))).context(format!("écoute impossible sur 127.0.0.1:{first}")),
    };
    let addr = listener.local_addr().context("adresse du tableau de bord inconnue")?;
    let key = uuid::Uuid::new_v4().simple().to_string();
    let url = format!("http://127.0.0.1:{}/?k={key}", addr.port());
    let state = Arc::new(State { root, lang, key, scan: Mutex::new(Scanner::new()), live: Default::default(), live_enabled });
    let task = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { continue };
            let st = state.clone();
            tokio::spawn(async move {
                let _ = serve_one(sock, st).await;
            });
        }
    });
    Ok(Dashboard { addr, url, task })
}

/// Ouvre une URL dans le navigateur de l'utilisateur. Échec sans conséquence : l'URL reste affichée.
pub fn open_in_browser(url: &str) -> Result<()> {
    let bin = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let st = std::process::Command::new(bin)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| anyhow::anyhow!("{bin} : {e}"))?;
    anyhow::ensure!(st.success(), "{bin} : {st}");
    Ok(())
}

/// Lit une requête, répond, ferme. Une connexion = une réponse (`Connection: close`).
async fn serve_one(mut sock: TcpStream, st: Arc<State>) -> Result<()> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    // En-têtes seulement : la fin du bloc suffit, le corps éventuel n'est jamais lu.
    let head = loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break String::from_utf8_lossy(&buf[..i]).into_owned();
        }
        if buf.len() > MAX_HEAD {
            return reply(&mut sock, 431, "text/plain; charset=utf-8", b"en-tetes trop longs").await;
        }
    };
    let mut lines = head.lines();
    let req = lines.next().unwrap_or_default();
    let host = lines
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("host")).map(|(_, v)| v.trim().to_string()))
        .unwrap_or_default();
    let mut parts = req.split_whitespace();
    let (method, target) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    // Nom d'hôte autre que la boucle locale : requête venue d'ailleurs (DNS rebinding) — refusée.
    if !local_host(&host) {
        return reply(&mut sock, 403, "text/plain; charset=utf-8", b"hote refuse").await;
    }
    if !matches!(method, "GET" | "HEAD") {
        return reply(&mut sock, 405, "text/plain; charset=utf-8", b"methode refusee").await;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if param(query, "k").as_deref() != Some(st.key.as_str()) {
        return reply(&mut sock, 403, "text/plain; charset=utf-8", b"cle manquante ou invalide").await;
    }
    match path {
        "/" | "/index.html" => reply(&mut sock, 200, "text/html; charset=utf-8", PAGE.as_bytes()).await,
        "/api/snapshot" => {
            let since = param(query, "since").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let body = snapshot(&st, since).await.to_string();
            reply(&mut sock, 200, "application/json; charset=utf-8", body.as_bytes()).await
        }
        _ => reply(&mut sock, 404, "text/plain; charset=utf-8", b"page inconnue").await,
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Vrai si l'en-tête `Host` désigne la boucle locale.
fn local_host(host: &str) -> bool {
    let name = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host).trim_matches(['[', ']']);
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

/// Valeur d'un paramètre de requête (sans décodage : les valeurs attendues sont alphanumériques).
fn param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|p| p.split_once('=').filter(|(k, _)| *k == name).map(|(_, v)| v.to_string()))
}

async fn reply(sock: &mut TcpStream, code: u16, mime: &str, body: &[u8]) -> Result<()> {
    let reason = match code {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    // La page n'a besoin d'aucune ressource extérieure : la politique de sécurité le dit.
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; form-action 'none'; base-uri 'none'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body).await?;
    sock.flush().await?;
    Ok(())
}

/// Instantané complet : jetons par session, par projet et par modèle, terminaux ouverts, runs d'essaim.
async fn snapshot(st: &Arc<State>, since: u64) -> Value {
    let now = store::now_ms();
    let (live, live_error) = live_sessions(st).await;
    let ids: Vec<String> = live.iter().filter_map(|s| s.session_id.clone()).collect();
    let s = st.clone();
    let built = tokio::task::spawn_blocking(move || {
        let mut g = s.scan.lock().unwrap_or_else(|e| e.into_inner());
        let refreshed = g.refresh();
        let win = g.window(since, now);
        // Consommation de toute la vie d'un terminal ouvert : il a souvent commencé avant la fenêtre affichée.
        // Coût pondéré **et** volume brut : le tableau de bord bascule entre les deux.
        let all_time: HashMap<String, (u64, u64)> = ids.into_iter().filter_map(|id| g.session(&id).map(|s| (id, (s.tally.weighted(), s.tally.volume())))).collect();
        (win, refreshed, g.files(), all_time, store::load_all(&s.root), live::latest(&s.root))
    })
    .await;
    let (win, refreshed, files, all_time, runs, en_cours) = match built {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("lecture des transcriptions interrompue : {e}")}),
    };

    // Jointure poste ↔ session : les traces de runs disent quel poste a lancé quelle session `claude`.
    let mut seat_of: HashMap<&str, String> = HashMap::new();
    for r in &runs {
        for n in &r.nodes {
            if let Some(sid) = n.session_id.as_deref().filter(|s| !s.is_empty()) {
                let label = if n.label.is_empty() { n.id.clone() } else { n.label.clone() };
                seat_of.entry(sid).or_insert(format!("{} · {label}", r.pipeline));
            }
        }
    }
    let live_of: HashMap<&str, &Session> = live.iter().filter_map(|s| s.session_id.as_deref().map(|id| (id, s))).collect();

    let sessions: Vec<Value> = win
        .sessions
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "label": s.label,
                "cwd": s.cwd,
                "first_ms": s.first_ms,
                "last_ms": s.last_ms,
                "u": s.tally,
                "sidechain": s.sidechain,
                "models": s.models,
                "seat": seat_of.get(s.id.as_str()),
                "live": live_of.get(s.id.as_str()).map(|l| live_json(l, (0, 0))),
            })
        })
        .collect();

    let terminals: Vec<Value> = live.iter().map(|l| live_json(l, l.session_id.as_deref().and_then(|id| all_time.get(id)).copied().unwrap_or((0, 0)))).collect();

    let mut par_agent: HashMap<&str, (u64, u64)> = HashMap::new();
    for r in runs.iter().take(MAX_RUNS) {
        for n in &r.nodes {
            let e = par_agent.entry(n.agent.as_str()).or_default();
            e.0 += n.tokens;
            e.1 += 1;
        }
    }
    let mut agents: Vec<Value> = par_agent.iter().map(|(a, (t, c))| json!({"name": a, "tokens": t, "calls": c})).collect();
    agents.sort_by_key(|v| std::cmp::Reverse(v["tokens"].as_u64().unwrap_or(0)));

    json!({
        "now_ms": now,
        "since_ms": since,
        "step_ms": win.step_ms,
        "lang": if st.lang == Lang::En { "en" } else { "fr" },
        "root": st.root.display().to_string(),
        "scan": {"files": files, "new_bytes": refreshed.new_bytes, "messages": refreshed.messages},
        "total": win.total,
        "buckets": win.buckets.iter().map(|(t, u)| json!({"t": t, "u": u})).collect::<Vec<_>>(),
        "sessions": sessions,
        "projects": win.projects.iter().map(|(n, u)| json!({"name": n, "u": u})).collect::<Vec<_>>(),
        "models": win.models.iter().map(|(n, u)| json!({"name": n, "u": u})).collect::<Vec<_>>(),
        "terminals": {"list": terminals, "error": live_error},
        "run": en_cours.map(|r| live_run_json(&r, now)),
        "runs": runs.iter().take(MAX_RUNS).map(run_json).collect::<Vec<_>>(),
        "agents": agents,
    })
}

/// Run suivi en direct : l'organigramme et l'état de chaque poste, plus deux totaux calculés ici pour
/// que la page n'ait pas à refaire la somme.
fn live_run_json(r: &live::LiveRun, now: u64) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("running".into(), json!(r.running(now)));
        o.insert("tokens".into(), json!(r.tokens()));
        o.insert("calls".into(), json!(r.nodes.iter().filter(|n| n.state != "waiting").count()));
    }
    v
}

fn live_json(s: &Session, (tokens, volume): (u64, u64)) -> Value {
    json!({
        "kind": s.kind,
        "name": s.label(),
        "pid": s.pid,
        "cwd": s.cwd,
        "started_ms": s.started_at,
        "status": s.status,
        "state": s.state,
        "session_id": s.session_id,
        "essaim": s.is_essaim(),
        "tokens": tokens,
        "volume": volume,
    })
}

fn run_json(r: &RunRecord) -> Value {
    json!({
        "id": r.id,
        "pipeline": r.pipeline,
        "started_ms": r.started_ms,
        "finished": r.finished,
        "tokens": r.tokens.max(r.nodes.iter().map(|n| n.tokens).sum()),
        "nodes": r.nodes.iter().map(|n| json!({
            "label": if n.label.is_empty() { n.id.clone() } else { n.label.clone() },
            "agent": n.agent,
            "tokens": n.tokens,
            "depth": n.depth,
            "session_id": n.session_id,
        })).collect::<Vec<_>>(),
    })
}

/// Sessions `claude` vivantes, relues au plus une fois par `LIVE_TTL_MS`.
async fn live_sessions(st: &Arc<State>) -> (Vec<Session>, Option<String>) {
    let now = store::now_ms();
    let mut g = st.live.lock().await;
    if st.live_enabled && (g.at_ms == 0 || now.saturating_sub(g.at_ms) > LIVE_TTL_MS) {
        match sessions::list_all().await {
            Ok(v) => {
                g.sessions = v;
                g.error = None;
            }
            // `claude` absent ou muet : la page le dit et garde le reste des chiffres.
            Err(e) => g.error = Some(format!("{e:#}")),
        }
        g.at_ms = now;
    }
    (g.sessions.clone(), g.error.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hote_local_seulement() {
        for ok in ["127.0.0.1:8787", "localhost:8787", "127.0.0.1", "localhost", "[::1]:8787"] {
            assert!(local_host(ok), "{ok} doit être accepté");
        }
        for bad in ["essaim.example.com:8787", "192.168.1.20:8787", "", "attaquant.fr"] {
            assert!(!local_host(bad), "{bad} doit être refusé");
        }
    }

    #[test]
    fn parametres_de_requete() {
        assert_eq!(param("k=abc&since=12", "k").as_deref(), Some("abc"));
        assert_eq!(param("k=abc&since=12", "since").as_deref(), Some("12"));
        assert_eq!(param("key=abc", "k"), None);
        assert_eq!(param("", "k"), None);
    }

    #[test]
    fn page_autonome() {
        // Aucune ressource distante : la page doit s'afficher sans réseau (hors ligne, sans CDN).
        for marqueur in ["<link", "<script src", "<img", "@import", "https://"] {
            assert!(!PAGE.contains(marqueur), "la page ne doit pas charger {marqueur}");
        }
        // Seule adresse http tolérée : l'espace de noms SVG, qui n'est jamais chargé.
        assert_eq!(PAGE.matches("http://").count(), PAGE.matches("http://www.w3.org/2000/svg").count());
        assert!(PAGE.contains("/api/snapshot"));
    }

    /// Requête HTTP complète envoyée au serveur ; renvoie (statut, corps).
    async fn get(addr: SocketAddr, target: &str, host: &str) -> (u16, String) {
        use tokio::io::AsyncReadExt;
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        let code = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        (code, body)
    }

    #[tokio::test]
    async fn sert_la_page_et_refuse_sans_cle() {
        let root = std::env::temp_dir().join(format!("essaim-web-{}", uuid::Uuid::new_v4()));
        let d = start_inner(root, Lang::Fr, 0, false).await.unwrap();
        let key = d.url.rsplit("k=").next().unwrap().to_string();
        let host = format!("127.0.0.1:{}", d.addr.port());

        let (code, body) = get(d.addr, &format!("/?k={key}"), &host).await;
        assert_eq!(code, 200);
        assert!(body.contains("<!DOCTYPE html>"));

        assert_eq!(get(d.addr, "/", &host).await.0, 403, "sans clé : refusé");
        assert_eq!(get(d.addr, "/?k=faux", &host).await.0, 403);
        assert_eq!(get(d.addr, &format!("/?k={key}"), "attaquant.fr").await.0, 403, "autre hôte : refusé");
        assert_eq!(get(d.addr, &format!("/autre?k={key}"), &host).await.0, 404);
    }

    /// Un run suivi en direct doit arriver dans l'instantané avec son organigramme et ses jetons.
    #[tokio::test]
    async fn instantane_du_run_en_cours() {
        use crate::runtime::executor::RunEvent;
        use crate::runtime::live::Tracker;
        let root = std::env::temp_dir().join(format!("essaim-web-{}", uuid::Uuid::new_v4()));
        let company = crate::model::company::Company::parse(
            "name = \"acme\"\n[[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"boss\"\n[[seat]]\nid = \"dev\"\ntitle = \"Dev\"\nagent = \"developer\"\nreports_to = \"ceo\"\nbuilds = true\n",
        )
        .unwrap();
        let mut tr = Tracker::new(&root, &store::new_id(1), &company, "Site vitrine", store::now_ms());
        tr.on_event(&RunEvent::Label { id: "ceo/conception/delegate".into(), label: "CEO · conception".into(), agent: "boss".into(), depth: 0 });
        tr.on_event(&RunEvent::NodeStarted { id: "ceo/conception/delegate".into() });
        tr.on_event(&RunEvent::NodeFinished { id: "ceo/conception/delegate".into(), status: crate::runtime::executor::NodeStatus::Succeeded, tokens: 4242 });

        let d = start_inner(root, Lang::Fr, 0, false).await.unwrap();
        let key = d.url.rsplit("k=").next().unwrap().to_string();
        let host = format!("127.0.0.1:{}", d.addr.port());
        let (code, body) = get(d.addr, &format!("/api/snapshot?k={key}&since=0"), &host).await;
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["run"]["company"], "acme");
        assert_eq!(v["run"]["mission"], "Site vitrine");
        assert_eq!(v["run"]["running"], true);
        assert_eq!(v["run"]["tokens"], 4242);
        assert_eq!(v["run"]["calls"], 1);
        assert_eq!(v["run"]["seats"][1]["reports_to"], "ceo");
        assert_eq!(v["run"]["nodes"][0]["state"], "ok");
    }

    #[tokio::test]
    async fn instantane_json_complet() {
        let root = std::env::temp_dir().join(format!("essaim-web-{}", uuid::Uuid::new_v4()));
        // Un run d'essaim à relire : ses postes doivent apparaître.
        let rec = RunRecord {
            id: store::new_id(1),
            pipeline: "acme".into(),
            started_ms: 1_793_000_000_000,
            finished: Some(true),
            tokens: 1234,
            nodes: vec![crate::runtime::store::NodeRecord {
                id: "ceo/conception".into(),
                agent: "boss".into(),
                label: "CEO · conception".into(),
                depth: 0,
                status: None,
                tokens: 1234,
                session_id: Some("sid-1".into()),
                log: vec![],
            }],
        };
        store::save(&root, &rec).unwrap();
        let d = start_inner(root, Lang::Fr, 0, false).await.unwrap();
        let key = d.url.rsplit("k=").next().unwrap().to_string();
        let host = format!("127.0.0.1:{}", d.addr.port());
        let (code, body) = get(d.addr, &format!("/api/snapshot?k={key}&since=0"), &host).await;
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("JSON illisible ({e}) : {body}"));
        assert!(v["now_ms"].as_u64().unwrap_or(0) > 0);
        assert_eq!(v["agents"][0]["name"], "boss");
        assert_eq!(v["agents"][0]["tokens"], 1234);
        assert_eq!(v["runs"][0]["pipeline"], "acme");
        assert_eq!(v["runs"][0]["nodes"][0]["label"], "CEO · conception");
        assert!(v["total"]["messages"].is_u64());
        assert!(v["buckets"].is_array() && v["sessions"].is_array() && v["terminals"]["list"].is_array());
        assert!(v["run"].is_null(), "aucun run suivi dans ce projet");
    }
}
