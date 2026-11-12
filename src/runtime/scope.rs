//! Périmètres de fichiers (globs relatifs au projet) : inclusion, chevauchement, lots sans conflit.
//!
//! Un périmètre ne donne aucun droit (tous les postes ont tous les outils) : il dit quels fichiers un poste
//! va modifier, pour que deux postes ne travaillent pas sur les mêmes en même temps.
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

/// Motif exploitable : non vide, sans remontée (`..`), sans `~` (domaine de l'utilisateur) ni `\`, sans
/// `,` `(` `)` ni caractère de contrôle. Un motif qui ne l'est pas n'est jamais tenu pour inclus dans un
/// autre, et il est supposé chevaucher tout le reste.
pub fn is_safe(p: &str) -> bool {
    // Contrôle sur le texte brut : un retour à la ligne final ne doit pas être effacé par `trim` avant le test.
    let t = p.trim();
    !t.is_empty() && !t.contains("..") && !t.contains(['~', '\\', ',', '(', ')']) && !p.chars().any(char::is_control)
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
        assert_eq!(normalize("./"), "**");
        assert_eq!(normalize("*"), "*", "`*` seul ne couvre que la racine");
    }

    #[test]
    fn remontee_refusee() {
        for bad in ["../x", "src/../../x", "a\\b", "~/x", "", "  "] {
            assert!(!is_safe(bad), "{bad:?} doit être refusé");
        }
        assert!(!covers("**", "../secret/**"), "`..` ne sort jamais du projet");
        assert!(!covers("src/**", "src/../../secret/**"));
        assert!(overlaps("../secret/**", "src/a.rs"), "motif non sûr : sérialisé avec tout");
    }

    #[test]
    fn etoile_seule_reste_la_racine() {
        assert_eq!(normalize("*"), "*");
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
    fn caracteres_de_regle_refuses() {
        for bad in ["src/a,Bash(rm", "src/a)", "src/(a", "src/a\n", "src/a\0"] {
            assert!(!is_safe(bad), "{bad:?} doit être refusé");
        }
        assert!(!all_covered(&v(&["**"]), &v(&["src/a/**),Bash(rm -rf :*"])), "un motif douteux n'est jamais tenu pour inclus");
        // Un périmètre vide (poste d'analyse) ne chevauche rien : il va dans le premier lot.
        assert_eq!(batches(&[v(&["src/**"]), vec![], vec![]]), vec![vec![0, 1, 2]]);
    }
}
