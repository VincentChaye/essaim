//! Garde-fous de la chaîne d'approvisionnement : workflow de release, installeur, `.gitignore`.
//! Ces tests lisent les fichiers du dépôt : ils échouent si une régression rouvre une surface d'attaque.

use std::path::Path;

fn lire(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn workflow() -> serde_json::Value {
    serde_norway::from_str(&lire(".github/workflows/release.yml")).expect("release.yml lisible")
}

#[test]
fn workflow_seul_publish_peut_ecrire() {
    let y = workflow();
    assert_eq!(y["permissions"]["contents"], "read", "lecture seule par défaut");
    assert_eq!(y["jobs"]["publish"]["permissions"]["contents"], "write", "seule la publication écrit");
    for (name, job) in y["jobs"].as_object().unwrap() {
        if name != "publish" {
            assert_ne!(job["permissions"]["contents"], "write", "le job {name} ne doit pas écrire");
        }
    }
}

#[test]
fn checkout_sans_identifiants_persistants() {
    let y = workflow();
    for (name, job) in y["jobs"].as_object().unwrap() {
        for step in job["steps"].as_array().into_iter().flatten() {
            if step["uses"].as_str().is_some_and(|u| u.starts_with("actions/checkout@")) {
                assert_eq!(step["with"]["persist-credentials"], false, "job {name}");
            }
        }
    }
}

#[test]
fn installeur_https_seulement_et_somme_obligatoire() {
    let src = lire("install.sh");
    // Seules les commandes exécutées comptent (pas l'exemple d'usage dans l'aide).
    let curls: Vec<&str> = src.lines().filter(|l| l.trim_start().starts_with("curl -fsSL") || l.trim_start().starts_with("if curl -fsSL")).collect();
    assert!(curls.len() >= 2, "téléchargements attendus : {curls:?}");
    for l in &curls {
        assert!(l.contains("--proto '=https'") && l.contains("--tlsv1.2"), "{l}");
    }
    assert!(src.contains("somme de contrôle introuvable") && src.contains("somme de contrôle invalide"), "vérification SHA-256 obligatoire");
}

#[test]
fn donnees_locales_ignorees_par_git() {
    assert!(lire(".gitignore").lines().any(|l| l.trim() == ".essaim/"), ".essaim/ doit figurer dans .gitignore");
}
