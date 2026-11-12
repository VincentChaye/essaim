//! Organigramme d'une entreprise d'agents : des postes (agent + intitulé) reliés par une hiérarchie libre.

use super::ConfigDirs;
use super::roles::level_label;
use crate::tf;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn default_max_calls() -> u32 {
    40
}
fn default_max_parallel() -> usize {
    2
}
fn default_node_timeout_s() -> u64 {
    15 * 60
}
fn default_max_fix_rounds() -> u32 {
    2
}
fn default_max_review_rounds() -> u32 {
    1
}
fn default_checkpoint() -> bool {
    true
}

/// Un poste : un agent placé dans l'organigramme.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seat {
    pub id: String,
    pub title: String,
    pub agent: String,
    /// Modèle imposé à ce poste (surcharge celui de l'agent) ; absent = modèle de l'agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Absent = racine (le boss).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_to: Option<String>,
    /// Domaine de compétence, vu par le supérieur pour déléguer (absent : seul l'intitulé est présenté).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expertise: Option<String>,
    /// Feuille de réalisation : ce poste réalise dans le projet (les autres feuilles analysent et conseillent).
    /// C'est un rôle, annoncé dans la fiche de poste, et non un droit : tous les postes ont tous les outils.
    #[serde(default, skip_serializing_if = "is_false")]
    pub builds: bool,
    /// Obsolète : tous les postes peuvent lancer toutes les commandes. Lu pour ne pas casser les anciens
    /// fichiers, conservé à l'écriture, ignoré à l'exécution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_bash: Vec<String>,
    /// Modèle des assistants légers (ex. « haiku ») : le poste leur confie des tâches mécaniques et entièrement
    /// spécifiées (inventaires, lancement de commandes, résumés de sortie), sans décision ; absent = pas d'assistants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helpers: Option<String>,
    /// Plafond de dépense estimée pour un appel de ce poste, en dollars ; absent = celui de l'entreprise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_budget_usd: Option<f64>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Company {
    pub name: String,
    /// Plafond d'appels `claude` par mission (garde-fou de quota).
    #[serde(default = "default_max_calls")]
    pub max_calls: u32,
    #[serde(default = "default_max_parallel")]
    pub max_parallel: usize,
    /// Pause pour valider le plan avant la réalisation.
    #[serde(default = "default_checkpoint")]
    pub checkpoint: bool,
    /// Délai maximal d'un appel `claude`, en secondes : garde-fou contre un poste qui s'emballe
    /// (boucle d'outils sans fin). Au-delà, l'appel est arrêté et le poste échoue.
    #[serde(default = "default_node_timeout_s")]
    pub node_timeout_s: u64,
    /// Plafond de dépense estimée par appel, en dollars (`--max-budget-usd`), surchargé par poste.
    /// Absent = pas de plafond : la bonne valeur dépend du modèle du poste.
    #[serde(default)]
    pub max_budget_usd: Option<f64>,
    /// Obsolète : il n'y a plus de tours de correction, un poste bloqué s'adresse à l'humain. Lu, ignoré.
    #[serde(default = "default_max_fix_rounds")]
    pub max_fix_rounds: u32,
    /// Ancien réglage de la revue du boss : 0 = pas de revue. Remplacé par `review`, qui prime s'il est présent.
    #[serde(default = "default_max_review_rounds")]
    pub max_review_rounds: u32,
    /// Revue finale du boss : il vérifie le résultat contre l'objectif ; un écart est présenté à l'humain,
    /// qui décide. Absent = `max_review_rounds > 0` (donc oui par défaut).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<bool>,
    /// Demande aux postes qui réalisent de clore chaque étape par un commit qui ne couvre qu'elle.
    #[serde(default, skip_serializing_if = "is_false")]
    pub commit_steps: bool,
    #[serde(default, rename = "seat")]
    pub seats: Vec<Seat>,
}

/// Une ligne de l'arbre affiché.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    pub idx: usize,
    pub depth: usize,
    /// Tracé (`│  `, `├─ `, `└─ `) à afficher avant le poste.
    pub prefix: String,
}

/// Largeur d'une boîte du graphe et pas horizontal entre deux feuilles voisines (en cellules).
pub const GRAPH_BOX_W: usize = 20;
pub const GRAPH_SLOT: usize = 22;

/// Un poste placé dans le graphe : même ordre que `tree_rows`, `cx` = centre horizontal de la boîte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphNode {
    pub idx: usize,
    pub depth: usize,
    pub cx: usize,
}

/// Navigation 2D dans le graphe, par numéro de ligne (= ordre de `Company::graph`).
pub mod graph_nav {
    use super::GraphNode;

    /// Supérieur : le plus proche poste précédent (préfixe) un niveau plus haut.
    pub fn parent(g: &[GraphNode], row: usize) -> Option<usize> {
        let d = g.get(row)?.depth;
        (0..row).rev().find(|&r| g[r].depth + 1 == d)
    }

    /// Premier subordonné.
    pub fn first_child(g: &[GraphNode], row: usize) -> Option<usize> {
        let d = g.get(row)?.depth;
        g.get(row + 1).filter(|n| n.depth == d + 1).map(|_| row + 1)
    }

    /// Subordonnés directs.
    pub fn children(g: &[GraphNode], row: usize) -> Vec<usize> {
        let Some(d) = g.get(row).map(|n| n.depth) else { return vec![] };
        let mut out = vec![];
        for r in row + 1..g.len() {
            match g[r].depth {
                x if x <= d => break,
                x if x == d + 1 => out.push(r),
                _ => {}
            }
        }
        out
    }

    /// Voisin du même niveau, à droite ou à gauche (traverse les branches).
    pub fn side(g: &[GraphNode], row: usize, right: bool) -> Option<usize> {
        let d = g.get(row)?.depth;
        if right { (row + 1..g.len()).find(|&r| g[r].depth == d) } else { (0..row).rev().find(|&r| g[r].depth == d) }
    }
}

/// Majorant du nombre d'appels `claude` d'une mission (la délégation étant dynamique, le réel est ≤).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallEstimate {
    pub conception: u32,
    pub realisation: u32,
}

impl CallEstimate {
    pub fn total(&self) -> u32 {
        self.conception + self.realisation
    }
}

/// Fichiers d'entreprise visibles : ceux du projet, puis ceux de l'utilisateur (disponibles depuis
/// tous les dossiers). Un même nom de fichier des deux côtés : celui du projet l'emporte.
pub fn files(dirs: &ConfigDirs) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    for dir in dirs.companies() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml")).collect();
        files.sort();
        let neufs: Vec<PathBuf> = files.into_iter().filter(|p| !out.iter().any(|q| q.file_name() == p.file_name())).collect();
        out.extend(neufs);
    }
    out
}

/// Entreprise désignée en ligne de commande : un chemin de fichier, ou le nom d'une entreprise
/// connue (projet ou utilisateur), avec ou sans `.toml`.
pub fn resolve(dirs: &ConfigDirs, arg: &Path) -> Result<PathBuf> {
    if arg.exists() {
        return Ok(arg.to_path_buf());
    }
    let wanted = arg.file_name().map(|n| n.to_string_lossy().trim_end_matches(".toml").to_string()).unwrap_or_default();
    if arg.parent().is_some_and(|p| !p.as_os_str().is_empty()) || wanted.is_empty() {
        bail!("{}", tf!("entreprise introuvable : {}", "company not found: {}", arg.display()));
    }
    let known = files(dirs);
    match known.iter().find(|p| p.file_stem().is_some_and(|n| n == wanted.as_str())) {
        Some(p) => Ok(p.clone()),
        None => {
            let names: Vec<String> = known.iter().filter_map(|p| p.file_stem().map(|n| n.to_string_lossy().into_owned())).collect();
            let list = if names.is_empty() { tf!("aucune", "none") } else { names.join(", ") };
            bail!("{}", tf!("entreprise introuvable : {} (connues : {list})", "company not found: {} (known: {list})", arg.display()))
        }
    }
}

impl Company {
    pub fn parse(src: &str) -> Result<Self> {
        toml::from_str(src).context("entreprise TOML invalide")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let src = super::read_config(path).with_context(|| tf!("lecture de {}", "reading {}", path.display()))?;
        Self::parse(&src)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.seats.iter().position(|s| s.id == id)
    }

    pub fn parent(&self, i: usize) -> Option<usize> {
        self.seats[i].reports_to.as_deref().and_then(|p| self.index_of(p))
    }

    /// Subordonnés directs, dans l'ordre du fichier.
    pub fn children(&self, i: usize) -> Vec<usize> {
        let id = &self.seats[i].id;
        (0..self.seats.len()).filter(|&j| self.seats[j].reports_to.as_deref() == Some(id.as_str())).collect()
    }

    pub fn is_leaf(&self, i: usize) -> bool {
        self.children(i).is_empty()
    }

    /// Postes sans supérieur valide (une organisation valide n'en a qu'un).
    pub fn roots(&self) -> Vec<usize> {
        (0..self.seats.len()).filter(|&i| self.parent(i).is_none()).collect()
    }

    pub fn root(&self) -> Option<usize> {
        let r = self.roots();
        (r.len() == 1).then(|| r[0])
    }

    /// Profondeur (racine = 0). Protégé contre les cycles.
    pub fn depth(&self, i: usize) -> usize {
        let (mut d, mut cur) = (0, i);
        while let Some(p) = self.parent(cur) {
            d += 1;
            cur = p;
            if d > self.seats.len() {
                break;
            }
        }
        d
    }

    /// `N`, `N-1`, `N-2`…
    pub fn level(&self, i: usize) -> String {
        level_label(self.depth(i))
    }

    /// Chaîne hiérarchique du poste vers le haut, du supérieur direct jusqu'à la racine.
    pub fn chain_of_command(&self, i: usize) -> Vec<usize> {
        let mut out = vec![];
        let mut cur = i;
        while let Some(p) = self.parent(cur) {
            if out.contains(&p) {
                break;
            }
            out.push(p);
            cur = p;
        }
        out
    }

    /// Lignes de l'arbre, en parcours préfixe, avec leur tracé.
    pub fn tree_rows(&self) -> Vec<TreeRow> {
        fn walk(c: &Company, i: usize, depth: usize, trail: &mut Vec<bool>, last: bool, seen: &mut HashSet<usize>, out: &mut Vec<TreeRow>) {
            if !seen.insert(i) {
                return;
            }
            let mut prefix = String::new();
            if depth > 0 {
                prefix.extend(trail.iter().map(|l| if *l { "   " } else { "│  " }));
                prefix.push_str(if last { "└─ " } else { "├─ " });
            }
            out.push(TreeRow { idx: i, depth, prefix });
            let kids = c.children(i);
            if depth > 0 {
                trail.push(last);
            }
            for (k, &j) in kids.iter().enumerate() {
                walk(c, j, depth + 1, trail, k + 1 == kids.len(), seen, out);
            }
            if depth > 0 {
                trail.pop();
            }
        }
        let mut out = vec![];
        let mut seen = HashSet::new();
        for r in self.roots() {
            walk(self, r, 0, &mut vec![], true, &mut seen, &mut out);
        }
        out
    }

    /// Placement en arbre : feuilles côte à côte, chaque supérieur centré au-dessus de ses subordonnés.
    pub fn graph(&self) -> Vec<GraphNode> {
        fn walk(c: &Company, i: usize, depth: usize, slot: &mut usize, seen: &mut HashSet<usize>, out: &mut Vec<GraphNode>) -> Option<usize> {
            if !seen.insert(i) {
                return None;
            }
            let pos = out.len();
            out.push(GraphNode { idx: i, depth, cx: 0 });
            let xs: Vec<usize> = c.children(i).into_iter().filter_map(|j| walk(c, j, depth + 1, slot, seen, out)).collect();
            let cx = match (xs.first(), xs.last()) {
                (Some(a), Some(b)) => (a + b) / 2,
                _ => {
                    *slot += 1;
                    (*slot - 1) * GRAPH_SLOT + GRAPH_BOX_W / 2
                }
            };
            out[pos].cx = cx;
            Some(cx)
        }
        let mut out = vec![];
        let (mut seen, mut slot) = (HashSet::new(), 0);
        for r in self.roots() {
            walk(self, r, 0, &mut slot, &mut seen, &mut out);
        }
        out
    }

    /// Organigramme en texte, une ligne par poste (affichage CLI et tests).
    pub fn outline(&self) -> Vec<String> {
        self.tree_rows()
            .iter()
            .map(|r| {
                let s = &self.seats[r.idx];
                format!(
                    "{}{:<4} {} · {} → {}{}",
                    r.prefix,
                    self.level(r.idx),
                    s.title,
                    s.id,
                    s.agent,
                    if s.builds { "  ⚒" } else { "" }
                )
            })
            .collect()
    }

    /// La revue finale du boss a-t-elle lieu ?
    pub fn reviews(&self) -> bool {
        self.review.unwrap_or(self.max_review_rounds > 0)
    }

    /// Majorant du flux de base : le plan (un appel du boss), puis la descente — délégation et rapport pour
    /// chaque poste interne, un appel de travail par feuille — et la revue du boss s'il a des subordonnés.
    pub fn estimate_calls(&self) -> CallEstimate {
        let descent: u32 = (0..self.seats.len()).map(|i| if self.is_leaf(i) { 1 } else { 2 }).sum();
        let review = u32::from(self.reviews() && self.root().is_some_and(|r| !self.is_leaf(r)));
        CallEstimate { conception: 1, realisation: descent + review }
    }

    /// Validation complète : ids, hiérarchie, agents, postes de réalisation.
    pub fn validate(&self, known_agents: &[String]) -> Result<()> {
        if self.seats.is_empty() {
            bail!("{}", tf!("l'entreprise n'a aucun poste", "the company has no seat"));
        }
        let mut seen = HashSet::new();
        for s in &self.seats {
            if s.id.is_empty() || !s.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                bail!("{}", tf!("id de poste invalide : {:?} (lettres, chiffres, - et _ uniquement)", "invalid seat id: {:?} (letters, digits, - and _ only)", s.id));
            }
            if !seen.insert(s.id.as_str()) {
                bail!("{}", tf!("id de poste dupliqué : {}", "duplicate seat id: {}", s.id));
            }
        }
        for s in &self.seats {
            if let Some(p) = &s.reports_to {
                if self.index_of(p).is_none() {
                    bail!("{}", tf!("le poste {} dépend de {} qui n'existe pas", "seat {} depends on {} which does not exist", s.id, p));
                }
            }
        }
        let roots = self.roots();
        if roots.len() != 1 {
            let names: Vec<&str> = roots.iter().map(|&i| self.seats[i].id.as_str()).collect();
            bail!("{}", tf!("il faut exactement un boss (poste sans supérieur), trouvé {} : {}", "exactly one boss is required (seat without supervisor), found {}: {}", roots.len(), names.join(", ")));
        }
        let reached = self.tree_rows().len();
        if reached != self.seats.len() {
            let missing: Vec<&str> = (0..self.seats.len())
                .filter(|&i| !self.tree_rows().iter().any(|r| r.idx == i))
                .map(|i| self.seats[i].id.as_str())
                .collect();
            bail!("{}", tf!("cycle hiérarchique : poste(s) non rattaché(s) au boss : {}", "hierarchy cycle: seat(s) not attached to the boss: {}", missing.join(", ")));
        }
        for (i, s) in self.seats.iter().enumerate() {
            if !known_agents.iter().any(|a| a == &s.agent) {
                bail!("{}", tf!("poste {} : agent inconnu {:?}", "seat {}: unknown agent {:?}", s.id, s.agent));
            }
            if s.builds {
                if !self.is_leaf(i) {
                    bail!("{}", tf!("poste {} : « builds » n'est permis que sur une feuille (il a des subordonnés)", "seat {}: \"builds\" is only allowed on a leaf (it has subordinates)", s.id));
                }
                if self.parent(i).is_none() {
                    bail!("{}", tf!("poste {} : le boss ne peut pas être un poste de réalisation", "seat {}: the boss cannot be a realisation seat", s.id));
                }
            }
            if let Some(m) = &s.helpers {
                if m.is_empty() || !m.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')) {
                    bail!("{}", tf!("poste {} : « helpers » attend un nom de modèle (ex. haiku)", "seat {}: \"helpers\" expects a model name (e.g. haiku)", s.id));
                }
            }
        }
        if self.max_parallel == 0 || self.max_calls == 0 {
            bail!("{}", tf!("max_calls et max_parallel doivent être ≥ 1", "max_calls and max_parallel must be ≥ 1"));
        }
        if self.node_timeout_s < 60 {
            bail!("{}", tf!("node_timeout_s doit être ≥ 60 (secondes)", "node_timeout_s must be ≥ 60 (seconds)"));
        }
        // Un plafond nul ou négatif arrêterait tout appel avant son premier jeton : refusé plutôt que subi.
        for b in std::iter::once(&self.max_budget_usd).chain(self.seats.iter().map(|s| &s.max_budget_usd)) {
            if b.is_some_and(|v| !(v.is_finite() && v > 0.0)) {
                bail!("{}", tf!("max_budget_usd doit être un nombre > 0", "max_budget_usd must be a number > 0"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const SRC: &str = r#"
name = "acme"
max_calls = 30

[[seat]]
id = "ceo"
title = "CEO"
agent = "boss"

[[seat]]
id = "cto"
title = "CTO"
agent = "vp"
reports_to = "ceo"

[[seat]]
id = "ciso"
title = "CISO"
agent = "vp"
reports_to = "ceo"

[[seat]]
id = "mgr"
title = "Engineering Manager"
agent = "manager"
reports_to = "cto"

[[seat]]
id = "dev1"
title = "Backend Engineer"
agent = "dev"
reports_to = "mgr"
builds = true
allow_bash = ["cargo test"]

[[seat]]
id = "dev2"
title = "Frontend Engineer"
agent = "dev"
reports_to = "mgr"
builds = true

[[seat]]
id = "sec"
title = "Security Analyst"
agent = "dev"
reports_to = "ciso"
"#;

    fn agents() -> Vec<String> {
        ["boss", "vp", "manager", "dev"].map(String::from).into()
    }

    fn company() -> Company {
        Company::parse(SRC).unwrap()
    }

    #[test]
    fn parse_valeurs_par_defaut_et_validation() {
        let c = company();
        assert_eq!(c.seats.len(), 7);
        assert_eq!(c.max_calls, 30);
        assert_eq!(c.max_parallel, 2);
        assert!(c.checkpoint);
        c.validate(&agents()).unwrap();
    }

    #[test]
    fn niveaux_et_hierarchie() {
        let c = company();
        let i = |id: &str| c.index_of(id).unwrap();
        assert_eq!(c.level(i("ceo")), "N");
        assert_eq!(c.level(i("cto")), "N-1");
        assert_eq!(c.level(i("mgr")), "N-2");
        assert_eq!(c.level(i("dev1")), "N-3");
        assert_eq!(c.children(i("mgr")), vec![i("dev1"), i("dev2")]);
        assert_eq!(c.chain_of_command(i("dev1")), vec![i("mgr"), i("cto"), i("ceo")]);
        assert_eq!(c.root(), Some(i("ceo")));
    }

    #[test]
    fn arbre_avec_trace() {
        let c = company();
        let lines: Vec<String> = c.tree_rows().iter().map(|r| format!("{}{}", r.prefix, c.seats[r.idx].id)).collect();
        assert_eq!(
            lines,
            vec!["ceo", "├─ cto", "│  └─ mgr", "│     ├─ dev1", "│     └─ dev2", "└─ ciso", "   └─ sec"]
        );
    }

    #[test]
    fn graphe_centre_et_navigation() {
        use super::graph_nav as nav;
        let c = company();
        let g = c.graph();
        let ids: Vec<&str> = g.iter().map(|n| c.seats[n.idx].id.as_str()).collect();
        assert_eq!(ids, c.tree_rows().iter().map(|r| c.seats[r.idx].id.as_str()).collect::<Vec<_>>(), "même ordre que l'arbre");
        let row = |id: &str| ids.iter().position(|x| *x == id).unwrap();
        // Un supérieur est centré sur ses subordonnés ; deux voisins d'un niveau ne se chevauchent pas.
        assert_eq!(g[row("mgr")].cx, (g[row("dev1")].cx + g[row("dev2")].cx) / 2);
        for a in &g {
            for b in g.iter().filter(|b| b.depth == a.depth && b.cx > a.cx) {
                assert!(b.cx - a.cx >= GRAPH_BOX_W + 1, "boîtes chevauchées");
            }
        }
        assert_eq!(nav::parent(&g, row("dev1")), Some(row("mgr")));
        assert_eq!(nav::parent(&g, row("ciso")), Some(row("ceo")));
        assert_eq!(nav::parent(&g, row("ceo")), None);
        assert_eq!(nav::first_child(&g, row("cto")), Some(row("mgr")));
        assert_eq!(nav::first_child(&g, row("dev1")), None);
        assert_eq!(nav::children(&g, row("ceo")), vec![row("cto"), row("ciso")]);
        assert_eq!(nav::side(&g, row("cto"), true), Some(row("ciso")));
        assert_eq!(nav::side(&g, row("dev2"), false), Some(row("dev1")));
        assert_eq!(nav::side(&g, row("ciso"), true), None);
        // Traverse les branches : mgr (sous cto) ↔ sec (sous ciso) sont au même niveau.
        assert_eq!(nav::side(&g, row("mgr"), true), Some(row("sec")));
        assert_eq!(nav::side(&g, row("sec"), false), Some(row("mgr")));
    }

    #[test]
    fn plan_texte() {
        let o = company().outline();
        assert_eq!(o[0], "N    CEO · ceo → boss");
        assert_eq!(o[1], "├─ N-1  CTO · cto → vp");
        assert_eq!(o[3], "│     ├─ N-3  Backend Engineer · dev1 → dev  ⚒");
    }

    #[test]
    fn estimation_un_plan_puis_une_descente() {
        let c = company();
        let e = c.estimate_calls();
        assert_eq!(e.conception, 1, "le plan : un appel du boss");
        // Descente : ceo, cto, ciso, mgr = 2 chacun ; dev1, dev2, sec = 1 chacun ; + 1 revue du boss.
        assert_eq!(e.realisation, 4 * 2 + 3 + 1);
        assert_eq!(e.total(), 13);
    }

    #[test]
    fn revue_reglable_par_le_nouveau_champ_ou_l_ancien() {
        let mut c = company();
        assert!(c.reviews(), "revue par défaut");
        c.max_review_rounds = 0;
        assert!(!c.reviews(), "ancien réglage : 0 = pas de revue");
        assert_eq!(c.estimate_calls().realisation, 4 * 2 + 3);
        c.review = Some(true);
        assert!(c.reviews(), "le nouveau champ prime");
        // Les anciens fichiers restent lisibles : champs obsolètes lus, ignorés à l'exécution.
        let ancien = Company::parse("name = \"a\"\nmax_fix_rounds = 5\ncommit_steps = true\n[[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"boss\"\n[[seat]]\nid = \"dev\"\ntitle = \"Dev\"\nagent = \"dev\"\nreports_to = \"ceo\"\nallow_bash = [\"cargo test),Bash(rm\"]\n").unwrap();
        ancien.validate(&agents()).unwrap();
        assert!(ancien.commit_steps);
    }

    #[test]
    fn entreprise_d_un_seul_poste_sans_revue() {
        let c = Company::parse("name = \"solo\"\n[[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"boss\"\n").unwrap();
        assert_eq!(c.estimate_calls().total(), 2, "le plan, puis le travail du boss lui-même");
    }

    fn err(c: &Company) -> String {
        c.validate(&agents()).unwrap_err().to_string()
    }

    #[test]
    fn refus_structure_invalide() {
        let mut c = company();
        c.seats[1].id = "ceo".into();
        assert!(err(&c).contains("dupliqué"));

        let mut c = company();
        c.seats[1].reports_to = Some("fantome".into());
        assert!(err(&c).contains("n'existe pas"));

        let mut c = company();
        c.seats[1].reports_to = None; // deux boss
        assert!(err(&c).contains("exactement un boss"));

        let mut c = company();
        c.seats[0].reports_to = Some("dev1".into()); // plus de racine
        assert!(err(&c).contains("exactement un boss"));

        let mut c = company();
        let idx = c.index_of("sec").unwrap();
        c.seats[idx].id = "mauvais id!".into();
        assert!(err(&c).contains("invalide"));
    }

    #[test]
    fn refus_cycle_detache() {
        let mut c = company();
        // cto ↔ mgr se désignent mutuellement : détachés du boss, mais le boss reste unique.
        let (a, b) = (c.index_of("cto").unwrap(), c.index_of("mgr").unwrap());
        c.seats[a].reports_to = Some("mgr".into());
        c.seats[b].reports_to = Some("cto".into());
        let msg = err(&c);
        assert!(msg.contains("cycle") || msg.contains("exactement un boss"), "{msg}");
    }

    #[test]
    fn refus_agents_et_builders() {
        let c = company();
        assert!(c.validate(&["boss".into()]).unwrap_err().to_string().contains("agent inconnu"));

        let mut c = company();
        let mgr = c.index_of("mgr").unwrap();
        c.seats[mgr].builds = true; // a des subordonnés
        assert!(err(&c).contains("feuille"));

        let mut c = company();
        c.seats[0].builds = true; // le boss
        assert!(err(&c).contains("feuille") || err(&c).contains("boss"));
    }

    #[test]
    fn aller_retour_toml() {
        let c = company();
        let tmp = std::env::temp_dir().join(format!("essaim-co-{}.toml", uuid::Uuid::new_v4()));
        c.save(&tmp).unwrap();
        assert_eq!(Company::load(&tmp).unwrap(), c);
    }

    #[test]
    fn entreprise_vide_refusee() {
        let c = Company::parse("name = \"x\"").unwrap();
        assert!(c.validate(&agents()).unwrap_err().to_string().contains("aucun poste"));
    }

    /// Les entreprises de l'utilisateur sont visibles depuis n'importe quel projet, et désignables
    /// par leur seul nom en ligne de commande.
    #[test]
    fn entreprises_du_projet_et_de_l_utilisateur() {
        let base = std::env::temp_dir().join(format!("essaim-ent-{}", uuid::Uuid::new_v4()));
        let dirs = ConfigDirs { project: base.join("projet"), user: Some(base.join("maison/.claude")) };
        std::fs::create_dir_all(dirs.project.join(".claude/companies")).unwrap();
        std::fs::create_dir_all(dirs.user.clone().unwrap().join("companies")).unwrap();
        let mini = "name = \"m\"\n[[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"a\"\n";
        std::fs::write(dirs.project.join(".claude/companies/locale.toml"), mini).unwrap();
        std::fs::write(dirs.user.clone().unwrap().join("companies/globale.toml"), mini).unwrap();
        // Doublon de nom : le fichier du projet masque celui de l'utilisateur.
        std::fs::write(dirs.project.join(".claude/companies/deux.toml"), mini).unwrap();
        std::fs::write(dirs.user.clone().unwrap().join("companies/deux.toml"), mini).unwrap();

        let noms: Vec<String> = files(&dirs).iter().map(|p| p.file_stem().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(noms, vec!["deux", "locale", "globale"]);
        assert_eq!(files(&dirs).iter().filter(|p| p.file_stem().unwrap() == "deux").count(), 1);
        assert!(files(&dirs)[0].starts_with(&dirs.project));

        assert_eq!(resolve(&dirs, Path::new("globale")).unwrap(), dirs.user.clone().unwrap().join("companies/globale.toml"));
        assert_eq!(resolve(&dirs, Path::new("globale.toml")).unwrap(), dirs.user.clone().unwrap().join("companies/globale.toml"));
        let chemin = dirs.project.join(".claude/companies/locale.toml");
        assert_eq!(resolve(&dirs, &chemin).unwrap(), chemin, "un chemin existant est pris tel quel");
        let err = resolve(&dirs, Path::new("fantome")).unwrap_err().to_string();
        assert!(err.contains("fantome") && err.contains("globale"), "{err}");
        assert!(resolve(&dirs, Path::new("dossier/absent.toml")).is_err());
    }
}
