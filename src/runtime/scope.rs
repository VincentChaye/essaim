//! Périmètres de fichiers (globs relatifs au projet) : inclusion, chevauchement, lots sans conflit,
//! et traduction en règles de permission `Edit(...)` / `Write(...)` de Claude Code.
//!
//! Les comparaisons sont volontairement **conservatrices** : en cas de doute, deux périmètres sont
//! considérés comme se chevauchant (on sérialise) et un périmètre n'est pas considéré comme inclus.

fn has_glob(p: &str) -> bool {
    p.contains(['*', '?', '[', '{'])
}

/// `./src//api/` → `src/api` ; `/src` → `src`. Le périmètre total s'écrit `**`.
/// `*` seul ne désigne que les fichiers de la racine : il n'est pas étendu à `**`.
pub fn normalize(p: &str) -> String {
    let p = p.trim().trim_start_matches("./").trim_start_matches('/');
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    let joined = parts.join("/");
    if joined.is_empty() { "**".into() } else { joined }
}

pub fn is_everything(p: &str) -> bool {
    normalize(p) == "**"
}

/// Motif utilisable dans une règle de permission : non vide, sans remontée (`..`),
/// sans `~` (domaine de l'utilisateur) ni `\`, sans `,` `(` `)` (qui sépareraient ou fermeraient la règle
/// `Edit(...)` jointe par virgules à l'envoi) ni caractère de contrôle. Un motif non sûr n'est jamais inclus,
/// ni n'obtient de règle.
pub fn is_safe(p: &str) -> bool {
    // Contrôle sur le texte brut : un retour à la ligne final ne doit pas être effacé par `trim` avant le test.
    let t = p.trim();
    !t.is_empty() && !t.contains("..") && !t.contains(['~', '\\', ',', '(', ')']) && !p.chars().any(char::is_control)
}

/// Dossiers réservés à essaim, à git et à Claude Code : réservés à **toute profondeur** (`**/.claude`, `src/.git/hooks`).
/// Comparaison insensible à la casse (macOS : `.CLAUDE` désigne `.claude`).
const RESERVED_DIRS: &[&str] = &[".claude", ".git", ".essaim"];

/// Chemins réservés à essaim, à git et à Claude Code (`.claude/settings*`, `.git/hooks`, `.essaim/`…) :
/// un poste ⚒ qui y écrirait pourrait élargir ses propres permissions ou préparer du code exécuté plus tard.
/// Le prédicat examine **tous** les segments du chemin (E1) : un segment est réservé si
/// - il est `{`, `[` ou `?` (motif de classe, qui peut désigner un segment caché) à n'importe quelle profondeur ;
/// - il appartient à `RESERVED_DIRS` à n'importe quelle profondeur ;
/// - il est caché (`.xxx`) et soit premier segment, soit à motif (`*`, `?`, `[`, `{`) : `.g*`, `.cl*`.
/// Un fichier caché ordinaire plus profond (`src/.env.example`) n'est pas réservé.
pub fn is_sensitive(p: &str) -> bool {
    let n = normalize(p);
    n.split('/').enumerate().any(|(i, seg)| {
        let lower = seg.to_ascii_lowercase();
        seg.contains(['{', '[', '?'])
            || RESERVED_DIRS.contains(&lower.as_str())
            || (seg.starts_with('.') && (i == 0 || seg.contains('*')))
    })
}

/// Dossier (ou fichier exact) auquel se rattache le motif : sa partie littérale.
fn base(p: &str) -> String {
    let p = normalize(p);
    if p == "**" {
        return String::new();
    }
    match p.find(['*', '?', '[', '{']) {
        None => p,
        Some(i) => match p[..i].rfind('/') {
            Some(j) => p[..j].to_string(),
            None => String::new(),
        },
    }
}

fn nested(inner: &str, outer: &str) -> bool {
    outer.is_empty() || inner == outer || inner.starts_with(&format!("{outer}/"))
}

/// Les deux motifs peuvent-ils désigner un même fichier ?
pub fn overlaps(a: &str, b: &str) -> bool {
    if !is_safe(a) || !is_safe(b) {
        return true; // motif non sûr : on suppose le pire
    }
    let (ba, bb) = (base(a), base(b));
    nested(&ba, &bb) || nested(&bb, &ba)
}

pub fn any_overlap(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| b.iter().any(|y| overlaps(x, y)))
}

/// `child` est-il inclus dans `parent` ? Seuls `**`, un fichier exact et `dossier/**` sont reconnus.
pub fn covers(parent: &str, child: &str) -> bool {
    if !is_safe(parent) || !is_safe(child) {
        return false;
    }
    let (p, c) = (normalize(parent), normalize(child));
    if p == "**" {
        return true;
    }
    if c == "**" {
        return false;
    }
    if let Some(dir) = p.strip_suffix("/**") {
        return nested(&base(&c), dir) && (!has_glob(&c) || c == p || c.starts_with(&format!("{dir}/")));
    }
    if !has_glob(&p) {
        // Fichier ou dossier exact : l'enfant doit être le même chemin ou se trouver dessous.
        return nested(&c, &p);
    }
    c == p
}

/// Tous les motifs de l'enfant sont-ils inclus dans l'un des motifs du parent ?
pub fn all_covered(parent: &[String], child: &[String]) -> bool {
    !child.is_empty() && child.iter().all(|c| parent.iter().any(|p| covers(p, c)))
}

/// Répartit des périmètres en lots : dans un lot, aucun chevauchement ; les lots s'exécutent l'un
/// après l'autre. Placement glouton, dans l'ordre d'arrivée (stable et prévisible).
pub fn batches(scopes: &[Vec<String>]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = vec![];
    for (i, s) in scopes.iter().enumerate() {
        match out.iter_mut().find(|b| b.iter().all(|&j| !any_overlap(&scopes[j], s))) {
            Some(b) => b.push(i),
            None => out.push(vec![i]),
        }
    }
    out
}

/// Règles `--allowedTools` d'écriture pour un périmètre. `**` → l'outil sans restriction.
/// Les motifs non sûrs (`..`, `~`, `\`, `,`, `()`) et réservés (`.claude`, `.git`…) ne donnent aucune règle : échec fermé.
pub fn write_rules(scope: &[String]) -> Vec<String> {
    let safe: Vec<&String> = scope.iter().filter(|s| is_safe(s) && !is_sensitive(s)).collect();
    let mut rules = vec![];
    for tool in ["Edit", "Write"] {
        if safe.iter().any(|s| is_everything(s)) {
            rules.push(tool.to_string());
        } else {
            rules.extend(safe.iter().map(|s| format!("{tool}({})", normalize(s))));
        }
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalisation() {
        assert_eq!(normalize("./src//api/"), "src/api");
        assert_eq!(normalize("/src/a.rs"), "src/a.rs");
        assert_eq!(normalize("  "), "**");
        assert!(is_everything("**") && is_everything("./"));
        assert!(!is_everything("*"), "`*` seul ne couvre que la racine");
    }

    #[test]
    fn remontee_refusee() {
        for bad in ["../x", "src/../../x", "a\\b", "~/x", "", "  "] {
            assert!(!is_safe(bad), "{bad:?} doit être refusé");
        }
        assert!(!covers("**", "../secret/**"), "`..` ne sort jamais du projet");
        assert!(!covers("src/**", "src/../../secret/**"));
        assert!(overlaps("../secret/**", "src/a.rs"), "motif non sûr : sérialisé avec tout");
        assert_eq!(write_rules(&v(&["../secret/**", "src/a/**"])), vec!["Edit(src/a/**)", "Write(src/a/**)"]);
        assert!(write_rules(&v(&["../x"])).is_empty(), "échec fermé : aucune règle d'écriture");
    }

    #[test]
    fn etoile_seule_reste_la_racine() {
        assert_eq!(normalize("*"), "*");
        assert_eq!(write_rules(&v(&["*"])), vec!["Edit(*)", "Write(*)"]);
        assert!(!covers("src/**", "*"));
        assert!(overlaps("*", "src/a.rs"));
    }

    #[test]
    fn chevauchement() {
        assert!(!overlaps("src/a/**", "src/b/**"));
        assert!(overlaps("src/a/**", "src/a/x.rs"));
        assert!(overlaps("src/**", "src/a/x.rs"));
        assert!(overlaps("**", "src/a/x.rs"));
        assert!(!overlaps("src/a.rs", "src/b.rs"));
        assert!(overlaps("src/*.rs", "src/b.rs"));
        assert!(!overlaps("src/ap/**", "src/api/**"), "frontière de dossier respectée");
        assert!(overlaps("src/api", "src/api/x.rs"), "un dossier contient ses fichiers");
    }

    #[test]
    fn inclusion() {
        assert!(covers("**", "src/a/**"));
        assert!(covers("src/**", "src/a/**"));
        assert!(covers("src/**", "src/a/x.rs"));
        assert!(covers("src/a/**", "src/a/x.rs"));
        assert!(!covers("src/a/**", "src/b/**"));
        assert!(!covers("src/a/**", "**"));
        assert!(!covers("src/a/**", "src/**"), "un périmètre plus large n'est pas inclus");
        assert!(covers("src/a.rs", "src/a.rs"));
        assert!(!covers("src/a.rs", "src/b.rs"));
        assert!(covers("src/api", "src/api/x.rs"));
        assert!(!covers("src/*.rs", "src/sub/x.rs"), "motif partiel : conservateur");
        assert!(all_covered(&v(&["src/**"]), &v(&["src/a/**", "src/b/x.rs"])));
        assert!(!all_covered(&v(&["src/**"]), &v(&["src/a/**", "docs/x.md"])));
        assert!(!all_covered(&v(&["src/**"]), &[]), "périmètre vide refusé");
    }

    #[test]
    fn lots_sans_conflit() {
        let s = vec![v(&["src/a/**"]), v(&["src/b/**"]), v(&["src/a/x.rs"]), v(&["docs/**"]), v(&["src/**"])];
        let b = batches(&s);
        // 0 et 1 et 3 peuvent coexister ; 2 chevauche 0 → lot suivant ; 4 chevauche tout src → dernier lot.
        assert_eq!(b, vec![vec![0, 1, 3], vec![2], vec![4]]);
        for lot in &b {
            for (i, &x) in lot.iter().enumerate() {
                for &y in &lot[i + 1..] {
                    assert!(!any_overlap(&s[x], &s[y]), "{x} et {y} se chevauchent dans le même lot");
                }
            }
        }
        assert!(batches(&[]).is_empty());
    }

    #[test]
    fn regles_de_permission() {
        assert_eq!(write_rules(&v(&["src/a/**", "./docs/x.md"])), vec!["Edit(src/a/**)", "Edit(docs/x.md)", "Write(src/a/**)", "Write(docs/x.md)"]);
        assert_eq!(write_rules(&v(&["**"])), vec!["Edit", "Write"]);
    }

    #[test]
    fn caracteres_de_regle_refuses() {
        // Une virgule ou une parenthèse dans un motif permettrait d'injecter une autre règle (ex. Bash).
        for bad in ["src/a,Bash(rm", "src/a)", "src/(a", "src/a\n", "src/a\0"] {
            assert!(!is_safe(bad), "{bad:?} doit être refusé");
        }
        assert!(write_rules(&v(&["src/a/**),Bash(rm -rf :*"])).is_empty(), "aucune règle injectée");
        assert_eq!(write_rules(&v(&["src/a/**", "src/x,y"])), vec!["Edit(src/a/**)", "Write(src/a/**)"]);
    }

    #[test]
    fn chemins_reserves_jamais_ecrits() {
        // `.claude/settings*` élargirait les permissions ; `.git/hooks` préparerait du code exécuté plus tard.
        for bad in [".claude/settings.json", ".git/hooks/pre-commit", ".essaim/missions/x", ".g*/**", "{.claude}/**", ".gitignore"] {
            assert!(is_sensitive(bad), "{bad} doit être réservé");
        }
        assert!(!is_sensitive("src/.env.example") && !is_sensitive("src/a/**"));
        assert_eq!(write_rules(&v(&["src/a/**", ".claude/**", ".git/hooks/**"])), vec!["Edit(src/a/**)", "Write(src/a/**)"]);
        assert!(write_rules(&v(&[".claude/**"])).is_empty(), "échec fermé");
    }
}
