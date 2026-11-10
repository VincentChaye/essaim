//! Parcours de bout en bout `essaim company run` (cadrage → exécution → revue) avec un faux `claude`.
//! Aucun réseau, aucun vrai modèle : le faux est un script shell déterministe (voir `tests/common/mod.rs`).
#![cfg(unix)]
mod common;
use common::*;
use std::path::PathBuf;

/// Dossier de l'unique mission créée par le lancement.
fn mission(b: &Bac) -> PathBuf {
    let racine = b.racine.join(".essaim/missions");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&racine).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(v.len(), 1, "une seule mission attendue dans {racine:?}");
    v.pop().unwrap()
}

fn kinds(ap: &[Appel]) -> Vec<&str> {
    ap.iter().map(|a| a.kind.as_str()).collect()
}

fn lance(nom: &str, mode: &str, builds: bool, brief: &str) -> (Bac, Lancement, Vec<Appel>) {
    let b = bac(nom);
    let ent = ecrire_entreprise(&b, "acme", &entreprise_sept(builds));
    let l = lancer(&b, &ent, brief, mode, "dev1");
    let ap = appels(&b);
    (b, l, ap)
}

#[test]
fn nominal_conception_puis_realisation() {
    let (b, l, ap) = lance("e2e-nominal", "nominal", true, BRIEF_LONG);
    assert!(l.ok(), "code {:?}\n{}", l.code, l.stderr);
    assert!(l.stderr.contains("conception 12 + réalisation 9"), "annonce des appels : {}", l.stderr);
    assert_eq!(ap.len(), 21, "12 appels en conception + 9 en réalisation : {:?}", kinds(&ap));
    for a in &ap[..12] {
        assert!(a.sys.contains("DESIGN PHASE"), "appel {} ({}) devrait être en conception", a.n, a.kind);
    }
    for a in &ap[12..] {
        assert!(a.sys.contains("REALISATION PHASE"), "appel {} ({}) devrait être en réalisation", a.n, a.kind);
    }
    // cadrage : le CEO délègue en premier, avec le brief ; revue puis rapport à la fin de chaque phase.
    assert_eq!(ap[0].kind, "delegate");
    assert!(ap[0].sys.contains("Concevoir") || ap[0].sys.contains("export CSV"));
    assert_eq!(&kinds(&ap)[10..12], ["review", "report"]);
    assert_eq!(&kinds(&ap)[19..21], ["review", "report"]);
    // la réalisation ne contient que des consignes de construction.
    assert!(ap[12..].iter().filter(|a| a.kind == "work").all(|a| a.prompt.contains("Carry it out in the project")));
    assert!(ap[..12].iter().filter(|a| a.kind == "work").all(|a| a.prompt.contains("Produce your deliverable as text")));
    // livrables
    let m = mission(&b);
    for f in ["brief.md", "conception.md", "conception.ok", "rapport.md", "reussie"] {
        assert!(m.join(f).is_file(), "{f} manquant");
    }
    assert!(std::fs::read_to_string(m.join("brief.md")).unwrap().contains("export CSV"));
    assert!(std::fs::read_to_string(m.join("rapport.md")).unwrap().contains("Rapport de"));
}

#[test]
fn conception_seule_sans_poste_de_realisation() {
    let (b, l, ap) = lance("e2e-conception", "nominal", false, BRIEF_LONG);
    assert!(l.ok(), "{}", l.stderr);
    assert_eq!(ap.len(), 12, "{:?}", kinds(&ap));
    assert!(ap.iter().all(|a| a.sys.contains("DESIGN PHASE") && !a.sys.contains("REALISATION PHASE")));
    let m = mission(&b);
    assert!(m.join("conception.md").is_file());
}

#[test]
fn le_nombre_d_appels_est_reproductible() {
    let (_, _, a) = lance("e2e-repro-a", "nominal", true, BRIEF_LONG);
    let (_, _, c) = lance("e2e-repro-b", "nominal", true, BRIEF_LONG);
    // L'ordre entre postes frères peut varier d'une exécution à l'autre : on compare la composition des appels.
    let trie = |ap: &[Appel]| {
        let mut k: Vec<String> = ap.iter().map(|x| x.kind.clone()).collect();
        k.sort();
        k
    };
    assert_eq!(trie(&a), trie(&c));
}

#[test]
fn revue_rejetee_puis_relance_et_reussite() {
    let (b, l, ap) = lance("e2e-redo", "review_redo", true, BRIEF_LONG);
    assert!(l.ok(), "{}", l.stderr);
    let ks = kinds(&ap);
    assert_eq!(ap.len(), 30, "{ks:?}");
    assert_eq!(ks.iter().filter(|k| **k == "review").count(), 2, "revue de conception rejetée, revue de réalisation acceptée");
    // la revue rejetée est suivie d'une nouvelle délégation (tour complémentaire) dans la même phase.
    assert_eq!(&ks[10..13], ["review", "delegate", "delegate"]);
    assert!(ap[12].sys.contains("DESIGN PHASE"));
    assert!(mission(&b).join("reussie").is_file());
}

#[test]
fn revue_non_atteinte_bloque_la_mission() {
    let (b, l, ap) = lance("e2e-unmet", "review_unmet", true, BRIEF_LONG);
    assert_eq!(l.code, Some(1), "{}", l.stderr);
    assert!(l.stderr.contains("Mission bloquée"), "{}", l.stderr);
    assert!(l.stderr.contains("critère X non atteint"));
    // arrêt en conception : pas de réalisation, ni rapport final, ni marque de réussite.
    assert!(ap.iter().all(|a| a.sys.contains("DESIGN PHASE")), "{:?}", kinds(&ap));
    let m = mission(&b);
    assert!(!m.join("reussie").exists() && !m.join("rapport.md").exists());
    assert!(m.join("conception.md").is_file());
}

#[test]
fn poste_en_echec_declenche_correction_puis_blocage() {
    for mode in ["fail", "crash", "cut"] {
        let (b, l, ap) = lance(&format!("e2e-{mode}"), mode, true, BRIEF_LONG);
        assert_eq!(l.code, Some(1), "{mode} : {}", l.stderr);
        assert!(l.stderr.contains("Mission bloquée, blocages non levés"), "{mode} : {}", l.stderr);
        assert!(l.stderr.contains("Backend Engineer"), "{mode} : le poste en cause est nommé");
        let ks = kinds(&ap);
        assert!(ks.contains(&"fix"), "{mode} : le supérieur est sollicité pour corriger : {ks:?}");
        assert!(!ks.contains(&"review"), "{mode} : pas de revue tant que des blocages restent : {ks:?}");
        let m = mission(&b);
        assert!(!m.join("reussie").exists(), "{mode}");
    }
}

#[test]
fn le_faux_processus_est_seul_appele_et_hors_reseau() {
    // Le lancement réussit sans `claude` dans le PATH : seul ESSAIM_CLAUDE_BIN est utilisé.
    let b = bac("e2e-horsreseau");
    let ent = ecrire_entreprise(&b, "mini", &entreprise_petite());
    let l = lancer(&b, &ent, BRIEF_COURT, "nominal", "dev");
    assert!(l.ok(), "{}", l.stderr);
    let ap = appels(&b);
    assert_eq!(ap.len(), 6, "{:?}", kinds(&ap));
    assert!(ap.iter().all(|a| !a.out.is_empty()));
}

#[test]
fn entreprise_inexistante_echoue_sans_appel() {
    let b = bac("e2e-inconnue");
    let chemin = b.racine.join(".claude/companies/absente.toml");
    let l = lancer(&b, &chemin, BRIEF_COURT, "nominal", "dev1");
    assert!(!l.ok(), "doit échouer");
    assert!(appels(&b).is_empty(), "aucun appel au modèle");
}

/// Limite de l'abonnement atteinte en pleine mission : le run se met en pause au lieu d'échouer, reprend
/// seul à l'heure annoncée, rejoue l'appel refusé et va au bout. Aucune erreur n'est rapportée, et la
/// mission ne garde pas de marque de pause puisqu'elle est allée à son terme.
#[test]
fn limite_d_abonnement_met_en_pause_puis_la_mission_aboutit() {
    let (b, l, ap) = lance("e2e-limite", "limit", false, BRIEF_COURT);
    assert!(l.ok(), "code {:?}\n{}", l.code, l.stderr);
    assert!(l.stderr.contains("⏸ limite de l'abonnement atteinte"), "pause annoncée : {}", l.stderr);
    assert!(l.stderr.contains("▶ reprise du run"), "reprise annoncée : {}", l.stderr);
    assert!(!l.stderr.contains("usage limit reached"), "le message brut de la limite n'est pas rapporté comme une erreur : {}", l.stderr);
    assert!(!l.stderr.contains("✖"), "aucun poste en échec : {}", l.stderr);
    // L'appel refusé est rejoué : dev1 a bien produit son travail (12 appels + la tentative limitée).
    assert_eq!(ap.len(), 13, "{:?}", kinds(&ap));
    let dev1: Vec<&Appel> = ap.iter().filter(|a| a.kind == "work" && a.prompt.contains("[[dev1]]")).collect();
    assert_eq!(dev1.len(), 2, "tentative limitée puis appel rejoué");
    assert!(dev1[0].out.is_empty() && dev1[1].out.contains("\"status\":\"done\""));
    let m = mission(&b);
    assert!(m.join("reussie").is_file() && !m.join("pause").exists(), "mission terminée, plus en pause");
}

/// Agents et entreprise du dossier personnel : `essaim init` les y crée, puis une mission se lance
/// depuis un projet qui ne contient aucune configuration, l'entreprise étant désignée par son seul
/// nom. C'est ce qui rend ce que l'on crée disponible dans tout le système de fichiers.
#[test]
fn entreprise_et_agents_personnels_depuis_un_projet_sans_configuration() {
    let b = bac("e2e-maison");
    let home = b.racine.parent().unwrap().join("home");
    // `essaim init` sans --root : tout va dans ~/.claude.
    let init = std::process::Command::new(env!("CARGO_BIN_EXE_essaim")).arg("init").env("HOME", &home).output().unwrap();
    assert!(init.status.success(), "{}", String::from_utf8_lossy(&init.stderr));
    assert!(home.join(".claude/agents/boss.md").is_file(), "agents créés dans le dossier personnel");
    assert!(home.join(".claude/companies/acme-studio.toml").is_file());

    // L'entreprise de la mission, elle aussi dans le dossier personnel.
    let ent = home.join(".claude/companies/globale.toml");
    std::fs::write(&ent, entreprise_sept(false)).unwrap();
    // Le projet, lui, n'a plus aucune configuration : agents et entreprise ne peuvent venir que de là.
    std::fs::remove_dir_all(b.racine.join(".claude")).unwrap();

    let l = lancer(&b, std::path::Path::new("globale"), BRIEF_COURT, "nominal", "dev1");
    assert!(l.ok(), "code {:?}\n{}", l.code, l.stderr);
    assert_eq!(appels(&b).len(), 12, "mission complète menée avec les agents personnels");
    assert!(mission(&b).join("reussie").is_file());
    // Les missions, elles, restent dans le projet.
    assert!(b.racine.join(".essaim/missions").is_dir());
    assert!(!home.join(".essaim").exists(), "le dossier personnel ne reçoit pas les missions");
}
