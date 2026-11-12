//! Harnais de mesure du coût agentique (missions M1 à M4, faux `claude`). Méthode et formule : `tests/mesure/README.md`.
//!
//! Formule des jetons synthétiques (FIGÉE) : entrée = ceil(octets(prompt + fiche de poste + schéma + sous-agents) / 4) ;
//! sortie = ceil(octets de la sortie structurée / 4) ; cache = ceil(préfixe commun de la fiche de poste avec l'appel
//! précédent / 4) borné à l'entrée. Durée = temps mural indicatif (exclue de la comparaison de stabilité).
//!
//! Relance : `cargo test --test mesure -- --ignored --nocapture mesure_ecrire_avant` (ou `mesure_ecrire_apres`).
#![cfg(unix)]

mod common;

use common::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn executer(s: &Scenario, tag: &str) -> (Mesure, BTreeMap<String, u64>, u64) {
    let b = bac(&format!("mesure-{}-{tag}", s.id.to_lowercase()));
    let ent = ecrire_entreprise(&b, "mesure", &s.entreprise);
    let l = match s.reponses {
        Some(r) => lancer_dialogue(&b, &ent, s.brief, s.mode, "dev1", r),
        None => lancer(&b, &ent, s.brief, s.mode, "dev1"),
    };
    assert!(l.ok(), "{} : code {:?}\n{}", s.id, l.code, l.stderr);
    let ap = appels(&b);
    let mut par_type = BTreeMap::new();
    for a in &ap {
        *par_type.entry(a.kind.clone()).or_insert(0u64) += 1;
    }
    (mesurer(&ap), par_type, l.duree.as_millis() as u64)
}

/// Deux exécutions par mission (mêmes chiffres exigés) ; renvoie le document de mesure.
fn campagne() -> Value {
    let mut missions = serde_json::Map::new();
    for s in scenarios() {
        let (m1, types, d1) = executer(&s, "a");
        let (m2, _, d2) = executer(&s, "b");
        assert_eq!(m1, m2, "{} : mesure non reproductible", s.id);
        assert!(m1.appels > 0 && m1.entree > 0 && m1.sortie > 0, "{} : mesure vide", s.id);
        let duree = d1.min(d2);
        eprintln!("[{}] {} | appels={} in={} out={} cache={} ({:.1} %) durée={duree} ms | {types:?}", s.id, s.description, m1.appels, m1.entree, m1.sortie, m1.cache, m1.part_cache() * 100.0);
        missions.insert(
            s.id.to_string(),
            json!({
                "description": s.description,
                "appels": m1.appels,
                "jetons_entree": m1.entree,
                "jetons_sortie": m1.sortie,
                "jetons_cache": m1.cache,
                "part_cache": (m1.part_cache() * 10000.0).round() / 10000.0,
                "duree_ms": duree,
                "appels_par_type": types,
                "appels_par_modele": m1.par_modele,
            }),
        );
    }
    json!({
        "duree_methode": "min de 2 exécutions par mission, indicative, hors comparaison (tests/mesure/README.md)",
        "methode": "tests/mesure/README.md",
        "formule": "entree=ceil(octets(prompt+fiche+schema+agents)/4); sortie=ceil(octets(sortie structurée)/4); cache=ceil(préfixe commun de la fiche avec l'appel précédent/4) borné à l'entrée",
        "max_parallel": 1,
        "missions": missions,
    })
}

fn ecrire(nom: &str, doc: &Value) {
    let chemin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mesure").join(nom);
    std::fs::write(&chemin, serde_json::to_string_pretty(doc).unwrap() + "\n").unwrap();
}

#[test]
fn mesure_reproductible() {
    campagne();
}

/// Écrit `tests/mesure/avant.json`.
#[test]
#[ignore = "écrit tests/mesure/avant.json"]
fn mesure_ecrire_avant() {
    ecrire("avant.json", &campagne());
}

/// Écrit `tests/mesure/apres.json`.
#[test]
#[ignore = "écrit tests/mesure/apres.json"]
fn mesure_ecrire_apres() {
    ecrire("apres.json", &campagne());
}
