//! Organigramme d'une entreprise d'agents : des postes (agent + intitulé) reliés par une hiérarchie libre.

use super::roles::level_label;
use crate::tf;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

fn default_max_calls() -> u32 {
    40
}
fn default_max_parallel() -> usize {
    2
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
    /// Feuille de réalisation : seul type de poste autorisé à modifier le projet.
    #[serde(default, skip_serializing_if = "is_false")]
    pub builds: bool,
    /// Commandes shell autorisées pour ce poste (ex. « cargo test »).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_bash: Vec<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Commande autorisée (`allow_bash`) : elle devient la règle `Bash(<commande>:*)`. Refusé : `, ( ) * :`
/// (séparateurs, jokers et délimiteur de la règle) et caractères de contrôle. Vide = refusé.
pub fn is_safe_command(c: &str) -> bool {
    let c = c.trim();
    !c.is_empty() && !c.contains([',', '(', ')', '*', ':']) && !c.chars().any(char::is_control)
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

    /// Postes mobilisés en phase de réalisation : les `builds` et tous leurs supérieurs.
    pub fn build_branch(&self) -> Vec<bool> {
        let mut flags = vec![false; self.seats.len()];
        for i in 0..self.seats.len() {
            if self.seats[i].builds {
                flags[i] = true;
                for p in self.chain_of_command(i) {
                    flags[p] = true;
                }
            }
        }
        flags
    }

    pub fn estimate_calls(&self) -> CallEstimate {
        let cost = |in_scope: &dyn Fn(usize) -> bool| -> u32 {
            (0..self.seats.len())
                .filter(|&i| in_scope(i))
                .map(|i| {
                    // Poste interne : délégation + rapport ; feuille : un seul appel de travail.
                    if self.children(i).iter().any(|&c| in_scope(c)) { 2 } else { 1 }
                })
                .sum()
        };
        let branch = self.build_branch();
        let realisation = if branch.iter().any(|b| *b) { cost(&|i| branch[i]) } else { 0 };
        CallEstimate { conception: cost(&|_| true), realisation }
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
            if !s.allow_bash.is_empty() && !s.builds {
                bail!("{}", tf!("poste {} : « allow_bash » n'a de sens que pour un poste « builds »", "seat {}: \"allow_bash\" only makes sense for a \"builds\" seat", s.id));
            }
            for cmd in &s.allow_bash {
                if !is_safe_command(cmd) {
                    bail!("{}", tf!("poste {} : commande autorisée invalide {:?} (ni , ( ) * : ni retour à la ligne)", "seat {}: invalid allowed command {:?} (no , ( ) * : or line break)", s.id, cmd));
                }
            }
        }
        if self.max_parallel == 0 || self.max_calls == 0 {
            bail!("{}", tf!("max_calls et max_parallel doivent être ≥ 1", "max_calls and max_parallel must be ≥ 1"));
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
    fn branche_de_realisation_et_estimation() {
        let c = company();
        let b = c.build_branch();
        let ids: Vec<&str> = (0..7).filter(|&i| b[i]).map(|i| c.seats[i].id.as_str()).collect();
        assert_eq!(ids, vec!["ceo", "cto", "mgr", "dev1", "dev2"], "ciso et sec ne participent pas à la réalisation");
        let e = c.estimate_calls();
        // Conception : ceo, cto, ciso, mgr = 2 chacun ; dev1, dev2, sec = 1 chacun.
        assert_eq!(e.conception, 4 * 2 + 3);
        // Réalisation : ceo, cto, mgr = 2 chacun ; dev1, dev2 = 1 chacun.
        assert_eq!(e.realisation, 3 * 2 + 2);
        assert_eq!(e.total(), 11 + 8);
    }

    #[test]
    fn sans_builder_pas_de_realisation() {
        let mut c = company();
        for s in &mut c.seats {
            s.builds = false;
            s.allow_bash.clear();
        }
        assert_eq!(c.estimate_calls().realisation, 0);
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

        let mut c = company();
        let sec = c.index_of("sec").unwrap();
        c.seats[sec].allow_bash = vec!["ls".into()];
        assert!(err(&c).contains("allow_bash"));
    }

    #[test]
    fn commandes_autorisees_sans_injection_de_regle() {
        // Une commande ne peut pas fermer la règle `Bash(...)` ni ajouter une autre règle, ni être un joker.
        for bad in ["cargo test),Bash(rm -rf", "*", "git:*", "ls\nrm", "   "] {
            let mut c = company();
            let dev1 = c.index_of("dev1").unwrap();
            c.seats[dev1].allow_bash = vec![bad.into()];
            assert!(err(&c).contains("commande autorisée invalide"), "{bad:?} doit être refusé");
        }
        let mut c = company();
        let dev1 = c.index_of("dev1").unwrap();
        c.seats[dev1].allow_bash = vec!["cargo test".into(), "cargo clippy --all-targets".into()];
        c.validate(&agents()).unwrap();
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
}
