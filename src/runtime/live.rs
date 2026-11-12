//! Suivi d'un run en temps réel, pour le tableau de bord.
//!
//! La trace complète (`run.json`) porte les journaux : elle est écrite au début, aux pauses et à la fin.
//! Trop lourde pour être réécrite chaque seconde. On écrit donc à côté un fichier léger,
//! `.essaim/runs/<id>/live.json` : l'organigramme de l'entreprise, l'état de chaque poste et ses jetons,
//! y compris ceux du message en cours. Quelques kilo-octets, réécrits au plus une fois par seconde.
//!
//! Le même suivi sert à la TUI et à `essaim company run` : les deux reçoivent les mêmes `RunEvent`.

use super::executor::{NodeStatus, RunEvent};
use super::store::{now_ms, runs_dir, write_private};
use crate::model::company::Company;
use crate::runtime::claude_proc::ClaudeEvent;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Intervalle minimal entre deux écritures (les jetons du message en cours changent sans arrêt).
const MIN_WRITE_MS: u64 = 1000;

/// Battement de cœur : même sans rien de neuf, le fichier est réécrit de temps en temps pour dire
/// que le run est toujours là.
const HEARTBEAT_MS: u64 = 15_000;

/// Au-delà, un `live.json` qui n'a pas bougé n'est plus « en direct » (essaim tué, machine éteinte).
pub const STALE_MS: u64 = 60_000;

/// Un poste de l'organigramme, tel que le tableau de bord le dessine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSeat {
    pub id: String,
    pub title: String,
    pub agent: String,
    #[serde(default)]
    pub reports_to: Option<String>,
    /// Poste de réalisation (⚒) : le seul type autorisé à modifier le projet.
    #[serde(default)]
    pub builds: bool,
}

/// Un appel `claude` du run, rattaché à son poste.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LiveNode {
    pub id: String,
    /// Poste qui passe l'appel (premier segment de `id`).
    pub seat: String,
    pub label: String,
    pub agent: String,
    pub depth: usize,
    /// `waiting`, `running`, `ok`, `failed`, `blocked`, `skipped`, `cancelled`.
    pub state: String,
    /// Jetons facturés une fois l'appel terminé.
    pub tokens: u64,
    /// Jetons déjà consommés par un appel en cours (messages soldés + message en cours).
    pub live: u64,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub started_ms: u64,
    #[serde(default)]
    pub finished_ms: u64,
}

impl LiveNode {
    /// Jetons à afficher : ceux de l'appel terminé, sinon ceux déjà consommés.
    pub fn used(&self) -> u64 {
        if self.tokens > 0 { self.tokens } else { self.live }
    }
}

/// État d'un run, relu par le tableau de bord.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LiveRun {
    pub id: String,
    pub company: String,
    /// Titre de la mission (vide pour un run sans mission).
    #[serde(default)]
    pub mission: String,
    pub started_ms: u64,
    pub updated_ms: u64,
    /// `None` = en cours.
    #[serde(default)]
    pub finished: Option<bool>,
    /// Pause en cours : `limit` (abonnement) ou `user`.
    #[serde(default)]
    pub paused: Option<String>,
    /// Heure de reprise automatique (epoch, secondes), si elle est connue.
    #[serde(default)]
    pub resets_at: Option<u64>,
    #[serde(default)]
    pub max_calls: u32,
    pub seats: Vec<LiveSeat>,
    pub nodes: Vec<LiveNode>,
}

impl LiveRun {
    pub fn tokens(&self) -> u64 {
        self.nodes.iter().map(|n| n.used()).sum()
    }

    /// Vrai si le run tourne encore et que le fichier a été écrit récemment.
    pub fn running(&self, now: u64) -> bool {
        self.finished.is_none() && now.saturating_sub(self.updated_ms) < STALE_MS
    }
}

/// Écrit `live.json` au fil des événements du run.
pub struct Tracker {
    path: PathBuf,
    run: LiveRun,
    last_write_ms: u64,
    dirty: bool,
    /// Message en cours par nœud : (id de message, jetons) — remplacé, jamais cumulé.
    current: Vec<(String, String, u64)>,
}

impl Tracker {
    pub fn new(root: &Path, run_id: &str, company: &Company, mission: &str, started_ms: u64) -> Self {
        let run = LiveRun {
            id: run_id.to_string(),
            company: company.name.clone(),
            mission: mission.to_string(),
            started_ms,
            updated_ms: started_ms,
            max_calls: company.max_calls,
            seats: company
                .seats
                .iter()
                .map(|s| LiveSeat { id: s.id.clone(), title: s.title.clone(), agent: s.agent.clone(), reports_to: s.reports_to.clone(), builds: s.builds })
                .collect(),
            ..Default::default()
        };
        let mut t = Self { path: runs_dir(root).join(run_id).join("live.json"), run, last_write_ms: 0, dirty: true, current: vec![] };
        t.write();
        t
    }

    /// Met l'état à jour. Les changements d'état sont écrits tout de suite, les jetons au plus une fois
    /// par seconde (ils bougent à chaque message).
    pub fn on_event(&mut self, ev: &RunEvent) {
        let mut force = false;
        // Toute activité d'un nœud qui attendait l'humain signifie que la réponse est arrivée.
        if let RunEvent::Node { id, .. } = ev
            && self.node(id).state == "asking"
        {
            self.node(id).state = "running".into();
            force = true;
        }
        match ev {
            RunEvent::Label { id, label, agent, depth } => {
                let n = self.node(id);
                n.label = label.clone();
                n.agent = agent.clone();
                n.depth = *depth;
                force = true;
            }
            RunEvent::NodeStarted { id } => {
                let now = now_ms();
                let n = self.node(id);
                n.state = "running".into();
                n.started_ms = now;
                force = true;
            }
            RunEvent::NodeFinished { id, status, tokens } => {
                let now = now_ms();
                let n = self.node(id);
                n.state = state_of(status).into();
                n.tokens = *tokens;
                n.finished_ms = now;
                force = true;
            }
            RunEvent::RunFinished { ok, .. } => {
                self.run.finished = Some(*ok);
                self.run.paused = None;
                force = true;
            }
            RunEvent::Paused { why, resets_at } => {
                self.run.paused = Some(match why {
                    super::pause::Why::Limit => "limit".into(),
                    super::pause::Why::User => "user".to_string(),
                });
                self.run.resets_at = *resets_at;
                force = true;
            }
            RunEvent::Resumed => {
                self.run.paused = None;
                self.run.resets_at = None;
                force = true;
            }
            // Le poste s'adresse à l'humain : il est à l'arrêt tant que la réponse n'est pas donnée.
            RunEvent::HumanRequest(req) => {
                if !req.node.is_empty() {
                    self.node(&req.node).state = "asking".into();
                }
                force = true;
            }
            RunEvent::Node { id, event } => match event {
                ClaudeEvent::Init { session_id, .. } => {
                    let sid = (!session_id.is_empty()).then(|| session_id.clone());
                    self.node(id).session_id = sid;
                    force = true;
                }
                // Un même `id` de message est remplacé, pas cumulé : seul le dernier état compte.
                ClaudeEvent::Usage { id: msg, tokens, .. } => {
                    let (msg, tokens) = (msg.clone(), *tokens);
                    let node = id.clone();
                    match self.current.iter_mut().find(|(n, m, _)| *n == node && *m == msg) {
                        Some(e) => e.2 = tokens,
                        None => self.current.push((node.clone(), msg, tokens)),
                    }
                    let total = self.current.iter().filter(|(n, _, _)| *n == node).map(|(_, _, t)| *t).sum();
                    self.node(id).live = total;
                }
                _ => {}
            },
            RunEvent::Checkpoint { .. } => force = true,
        }
        self.dirty = true;
        if force {
            self.write();
        } else {
            self.tick();
        }
    }

    /// Écrit si quelque chose a changé et que la dernière écriture est assez ancienne, ou si le
    /// battement de cœur est dû (un run silencieux ne doit pas passer pour mort).
    pub fn tick(&mut self) {
        let depuis = now_ms().saturating_sub(self.last_write_ms);
        if (self.dirty && depuis >= MIN_WRITE_MS) || (self.run.finished.is_none() && depuis >= HEARTBEAT_MS) {
            self.write();
        }
    }

    /// Nœud existant, ou créé à la volée (la délégation est dynamique : les nœuds apparaissent en route).
    fn node(&mut self, id: &str) -> &mut LiveNode {
        match self.run.nodes.iter().position(|n| n.id == id) {
            Some(i) => &mut self.run.nodes[i],
            None => {
                let seat = id.split('/').next().unwrap_or(id).to_string();
                self.run.nodes.push(LiveNode { id: id.to_string(), seat, label: id.to_string(), state: "waiting".into(), ..Default::default() });
                self.run.nodes.last_mut().expect("nœud ajouté")
            }
        }
    }

    fn write(&mut self) {
        self.run.updated_ms = now_ms();
        self.last_write_ms = self.run.updated_ms;
        self.dirty = false;
        // Échec d'écriture sans conséquence : le run continue, le tableau de bord montre l'état précédent.
        let Ok(bytes) = serde_json::to_vec(&self.run) else { return };
        let Some(dir) = self.path.parent() else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let tmp = self.path.with_extension("json.tmp");
        if write_private(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

fn state_of(s: &NodeStatus) -> &'static str {
    match s {
        NodeStatus::Succeeded => "ok",
        NodeStatus::Failed(_) => "failed",
        NodeStatus::Blocked(_) => "blocked",
        NodeStatus::Skipped(_) => "skipped",
        NodeStatus::Cancelled => "cancelled",
    }
}

/// Le run suivi le plus récent du projet (en cours de préférence), s'il y en a un.
pub fn latest(root: &Path) -> Option<LiveRun> {
    let rd = std::fs::read_dir(runs_dir(root)).ok()?;
    let mut runs: Vec<LiveRun> = rd
        .flatten()
        .filter_map(|e| std::fs::read(e.path().join("live.json")).ok())
        .filter_map(|b| serde_json::from_slice::<LiveRun>(&b).ok())
        .collect();
    let now = now_ms();
    // Un run encore vivant passe devant un run terminé, même plus récent.
    runs.sort_by_key(|r| (r.running(now), r.updated_ms));
    runs.pop()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::store::new_id;

    fn company() -> Company {
        Company::parse(
            "name = \"acme\"\n\
             [[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"boss\"\n\
             [[seat]]\nid = \"cto\"\ntitle = \"CTO\"\nagent = \"vp-tech\"\nreports_to = \"ceo\"\n\
             [[seat]]\nid = \"dev\"\ntitle = \"Dev\"\nagent = \"developer\"\nreports_to = \"cto\"\nbuilds = true\n",
        )
        .unwrap()
    }

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("essaim-live-{}", uuid::Uuid::new_v4()))
    }

    fn relu(root: &Path) -> LiveRun {
        latest(root).expect("live.json lisible")
    }

    #[test]
    fn organigramme_ecrit_des_le_depart() {
        let root = tmp();
        let t = Tracker::new(&root, &new_id(1), &company(), "Site vitrine", 1_793_000_000_000);
        drop(t);
        let r = relu(&root);
        assert_eq!(r.company, "acme");
        assert_eq!(r.mission, "Site vitrine");
        assert_eq!(r.seats.len(), 3);
        assert_eq!(r.seats[2].reports_to.as_deref(), Some("cto"));
        assert!(r.seats[2].builds);
        assert_eq!(r.finished, None);
        assert!(r.nodes.is_empty());
    }

    #[test]
    fn jetons_et_etats_suivis_poste_par_poste() {
        let root = tmp();
        let id = new_id(2);
        let mut t = Tracker::new(&root, &id, &company(), "m", 1_793_000_000_000);
        t.on_event(&RunEvent::Label { id: "ceo/conception/delegate".into(), label: "CEO · conception".into(), agent: "boss".into(), depth: 0 });
        t.on_event(&RunEvent::NodeStarted { id: "ceo/conception/delegate".into() });
        t.on_event(&RunEvent::Node { id: "ceo/conception/delegate".into(), event: ClaudeEvent::Init { session_id: "sess-1".into(), model: "sonnet".into(), api_key_source: "none".into() } });
        // Message en cours : la valeur du même id est remplacée, pas cumulée.
        t.on_event(&RunEvent::Node { id: "ceo/conception/delegate".into(), event: ClaudeEvent::Usage { id: "m1".into(), tokens: 100, volume: 100, cached: 0 } });
        t.on_event(&RunEvent::Node { id: "ceo/conception/delegate".into(), event: ClaudeEvent::Usage { id: "m1".into(), tokens: 180, volume: 180, cached: 20 } });
        t.on_event(&RunEvent::Node { id: "ceo/conception/delegate".into(), event: ClaudeEvent::Usage { id: "m2".into(), tokens: 20, volume: 20, cached: 0 } });
        // Les jetons du message en cours sont suivis tout de suite ; leur écriture, elle, est espacée.
        let n = &t.run.nodes[0];
        assert_eq!((n.seat.as_str(), n.state.as_str()), ("ceo", "running"));
        assert_eq!(n.live, 200, "180 (message remplacé) + 20");
        assert_eq!(n.used(), 200);
        assert_eq!(n.session_id.as_deref(), Some("sess-1"));
        assert_eq!(t.run.tokens(), 200);
        assert!(n.started_ms > 0 && n.finished_ms == 0);
        assert_eq!(relu(&root).nodes[0].state, "running", "l'état, lui, est déjà sur le disque");

        t.on_event(&RunEvent::NodeFinished { id: "ceo/conception/delegate".into(), status: NodeStatus::Succeeded, tokens: 250 });
        t.on_event(&RunEvent::Label { id: "dev/realisation/work".into(), label: "Dev · réalisation".into(), agent: "developer".into(), depth: 2 });
        t.on_event(&RunEvent::NodeFinished { id: "dev/realisation/work".into(), status: NodeStatus::Blocked("tests rouges".into()), tokens: 90 });
        let r = relu(&root);
        assert_eq!(r.nodes[0].state, "ok");
        assert_eq!(r.nodes[0].used(), 250, "les jetons finaux remplacent le suivi en direct");
        assert_eq!((r.nodes[1].seat.as_str(), r.nodes[1].state.as_str()), ("dev", "blocked"));
        assert_eq!(r.tokens(), 340);
        assert!(r.running(now_ms()), "run encore en cours");

        t.on_event(&RunEvent::RunFinished { ok: true, tokens: 340 });
        let r = relu(&root);
        assert_eq!(r.finished, Some(true));
        assert!(!r.running(now_ms()));
    }

    #[test]
    fn un_poste_qui_attend_l_humain_est_visible_puis_repart() {
        use crate::runtime::executor::{AskKind, HumanRequest};
        let root = tmp();
        let mut t = Tracker::new(&root, &new_id(4), &company(), "m", 1);
        t.on_event(&RunEvent::NodeStarted { id: "dev/realisation/work".into() });
        let (reply, _rx) = tokio::sync::mpsc::unbounded_channel();
        t.on_event(&RunEvent::HumanRequest(HumanRequest { node: "dev/realisation/work".into(), seat: "Dev".into(), kind: AskKind::Question, questions: vec![], reply }));
        assert_eq!(relu(&root).nodes[0].state, "asking", "écrit tout de suite : c'est l'humain qu'on attend");
        // La réponse donnée, l'agent reprend : son premier message le remet « en cours ».
        t.on_event(&RunEvent::Node { id: "dev/realisation/work".into(), event: ClaudeEvent::Usage { id: "m1".into(), tokens: 10, volume: 10, cached: 0 } });
        assert_eq!(relu(&root).nodes[0].state, "running");
        // Une demande d'essaim lui-même (poste bloqué, revue) ne vise aucun nœud en cours.
        let (reply, _rx) = tokio::sync::mpsc::unbounded_channel();
        t.on_event(&RunEvent::HumanRequest(HumanRequest { node: String::new(), seat: "CEO".into(), kind: AskKind::Review, questions: vec![], reply }));
        assert_eq!(relu(&root).nodes.len(), 1);
    }

    #[test]
    fn pause_et_reprise_visibles() {
        let root = tmp();
        let mut t = Tracker::new(&root, &new_id(3), &company(), "m", 1);
        t.on_event(&RunEvent::Paused { why: crate::runtime::pause::Why::Limit, resets_at: Some(1_793_000_123) });
        let r = relu(&root);
        assert_eq!(r.paused.as_deref(), Some("limit"));
        assert_eq!(r.resets_at, Some(1_793_000_123));
        t.on_event(&RunEvent::Resumed);
        assert_eq!(relu(&root).paused, None);
    }

    #[test]
    fn le_run_vivant_passe_devant_un_run_termine_plus_recent() {
        let root = tmp();
        let mut vieux = Tracker::new(&root, &new_id(10), &company(), "en cours", 1);
        vieux.on_event(&RunEvent::NodeStarted { id: "ceo/conception/delegate".into() });
        let mut neuf = Tracker::new(&root, &new_id(20), &company(), "terminée", 2);
        neuf.on_event(&RunEvent::RunFinished { ok: true, tokens: 0 });
        assert_eq!(latest(&root).unwrap().mission, "en cours");
        // Plus aucun run vivant : le plus récemment écrit l'emporte. `updated_ms` est en
        // millisecondes, donc sans cette attente les deux écritures peuvent porter le même instant
        // et l'ordre dépend alors de celui du dossier (test intermittent).
        std::thread::sleep(std::time::Duration::from_millis(2));
        vieux.on_event(&RunEvent::RunFinished { ok: false, tokens: 0 });
        assert_eq!(latest(&root).unwrap().mission, "en cours", "le dernier écrit reste en tête");
    }

    #[test]
    fn ecriture_limitee_a_une_par_seconde_hors_changement_d_etat() {
        let root = tmp();
        let mut t = Tracker::new(&root, &new_id(4), &company(), "m", 1);
        t.on_event(&RunEvent::Label { id: "ceo/conception/delegate".into(), label: "CEO".into(), agent: "boss".into(), depth: 0 });
        let ecrit = |t: &Tracker| std::fs::metadata(&t.path).unwrap().len();
        let avant = ecrit(&t);
        for i in 0..50u64 {
            t.on_event(&RunEvent::Node { id: "ceo/conception/delegate".into(), event: ClaudeEvent::Usage { id: format!("m{i}"), tokens: 10, volume: 10, cached: 0 } });
        }
        assert_eq!(ecrit(&t), avant, "les jetons seuls n'ont pas déclenché 50 écritures");
        assert!(t.dirty, "la mise à jour reste en attente");
        // Un changement d'état écrit tout de suite, avec les jetons accumulés.
        t.on_event(&RunEvent::NodeStarted { id: "ceo/conception/delegate".into() });
        assert_eq!(relu(&root).nodes[0].live, 500);
    }

    #[test]
    fn aucun_run_suivi() {
        assert!(latest(&tmp()).is_none());
        let root = tmp();
        let dir = runs_dir(&root).join("casse");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("live.json"), "{pas du json").unwrap();
        assert!(latest(&root).is_none(), "un fichier illisible ne doit pas faire échouer la lecture");
    }
}
