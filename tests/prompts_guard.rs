//! Garde-fou : le contenu essentiel envoyé au modèle (rôle, périmètre, consigne, critères) doit rester présent,
//! quelle que soit l'optimisation des prompts. Les tests cherchent de courtes sous-chaînes (mots-clés), sans figer
//! la formulation. Faux `claude` uniquement.
#![cfg(unix)]
mod common;
use common::*;
use std::sync::OnceLock;

/// Mission M3 (7 postes, 2 de réalisation) exécutée une seule fois pour tous les tests de ce fichier.
fn corpus() -> &'static Vec<Appel> {
    static C: OnceLock<Vec<Appel>> = OnceLock::new();
    C.get_or_init(|| {
        let b = bac("garde-prompts");
        let ent = ecrire_entreprise(&b, "acme", &entreprise_sept(true));
        let l = lancer(&b, &ent, BRIEF_LONG, "nominal", "dev1");
        assert!(l.ok(), "{}\n{}", l.stdout, l.stderr);
        appels(&b)
    })
}

/// Texte complet vu par le modèle pour un appel.
fn tout(a: &Appel) -> String {
    format!("{}\n{}\n{}\n{}", a.sys, a.prompt, a.schema, a.agents)
}

fn contient(a: &Appel, mot: &str) -> bool {
    tout(a).to_lowercase().contains(&mot.to_lowercase())
}

#[test]
fn le_corpus_a_la_forme_attendue() {
    let c = corpus();
    assert_eq!(c.len(), 13, "le plan du boss, puis une seule descente");
    assert!(c.iter().all(|a| !a.sys.is_empty() && !a.prompt.is_empty()));
}

/// Un appel du boss : seul poste à voir le brief. Reconnu par son intitulé dans la fiche de poste.
fn est_boss(a: &Appel) -> bool {
    a.sys.contains("You hold the seat \"CEO\"")
}

#[test]
fn le_brief_ne_va_qu_au_boss() {
    // C'est l'intérêt de la hiérarchie : le boss digère la demande, ses subordonnés ne reçoivent que la
    // spec écrite pour eux. Un brief recopié partout gonfle le contexte de chaque poste, et il est relu
    // à chaque tour d'outil, pas une seule fois.
    let (boss, autres): (Vec<&Appel>, Vec<&Appel>) = corpus().iter().partition(|a| est_boss(a));
    assert!(!boss.is_empty() && !autres.is_empty(), "le corpus doit contenir les deux cas");
    for a in &boss {
        for mot in ["export CSV", "UTF-8", "deux décimales", "erreur d'écriture", "pas de nouvelle dépendance"] {
            assert!(contient(a, mot), "appel {} du boss ({}) : « {mot} » absent", a.n, a.kind);
        }
    }
    // Les critères du brief ne doivent pas apparaître dans la FICHE DE POSTE d'un subordonné. Ils peuvent
    // légitimement figurer dans son prompt si son supérieur a choisi de les lui transmettre : c'est sa
    // décision, pas un contexte imposé à tous.
    for a in &autres {
        for mot in ["deux décimales", "pas de nouvelle dépendance"] {
            assert!(!a.sys.to_lowercase().contains(&mot.to_lowercase()), "appel {} ({}) : « {mot} » imposé par la fiche de poste", a.n, a.kind);
        }
        assert!(
            a.sys.contains("You do not see the company brief"),
            "appel {} ({}) : la fiche de poste doit dire au poste qu'il travaille sur la seule spec de son supérieur",
            a.n,
            a.kind
        );
    }
}

#[test]
fn le_role_est_present_dans_chaque_appel() {
    for a in corpus() {
        assert!(contient(a, "acme"), "appel {} : entreprise absente", a.n);
        assert!(contient(a, "chain of command") || contient(a, "reports to"), "appel {} : hiérarchie absente", a.n);
        assert!(contient(a, "you hold the seat") || contient(a, "seat"), "appel {} : poste absent", a.n);
        assert!(contient(a, "user's language") || contient(a, "language"), "appel {} : langue absente", a.n);
    }
    // Le titre du poste figure dans les appels du poste concerné.
    let titres = ["CEO", "CTO", "CISO", "Engineering Manager", "Backend Engineer", "Frontend Engineer", "Security Analyst"];
    for t in titres {
        assert!(corpus().iter().any(|a| a.sys.contains(&format!("\"{t}\""))), "aucun appel pour le poste {t}");
    }
}

#[test]
fn la_phase_est_dite_dans_chaque_appel() {
    let c = corpus();
    assert!(contient(&c[0], "plan phase") && contient(&c[0], "nothing is built yet"), "appel 1 : phase de plan");
    for a in &c[1..] {
        assert!(contient(a, "realisation phase"), "appel {} : phase de réalisation", a.n);
    }
}

#[test]
fn les_managers_connaissent_leurs_subordonnes() {
    for a in corpus().iter().filter(|a| a.kind == "delegate") {
        assert!(contient(a, "subordinate"), "appel {} : subordonnés absents", a.n);
        assert!(contient(a, "structured output"), "appel {} : sortie structurée absente", a.n);
        assert!(contient(a, "assignments"), "appel {} : schéma d'affectation absent", a.n);
    }
    let racine = corpus().iter().find(|a| a.kind == "delegate").unwrap();
    for sub in ["cto", "ciso"] {
        assert!(contient(racine, sub), "le CEO doit connaître {sub}");
    }
}

#[test]
fn la_consigne_deleguee_arrive_au_poste() {
    let c = corpus();
    let delegations = c.iter().filter(|a| a.kind == "delegate").count();
    assert!(delegations >= 3);
    // Chaque poste de travail reçoit une consigne (« Task ») et doit répondre par la sortie structurée.
    for a in c.iter().filter(|a| a.kind == "work") {
        assert!(contient(a, "task"), "appel {} : consigne absente", a.n);
        assert!(a.prompt.contains("Tâche pour") || a.prompt.contains("Complément pour") || contient(a, "analyse"), "appel {} : contenu de la tâche déléguée perdu", a.n);
        assert!(contient(a, "blocked") && contient(a, "done"), "appel {} : statuts de réponse absents", a.n);
        assert!(contient(a, "status"), "appel {} : champ de statut absent", a.n);
    }
    // La tâche déléguée par un supérieur est reprise dans le prompt du poste, nommée par son identifiant.
    for a in c.iter().filter(|a| a.kind == "work") {
        assert!(a.prompt.contains("[["), "appel {} : tâche déléguée perdue : {}", a.n, a.prompt);
    }
}

#[test]
fn le_perimetre_de_fichiers_est_donne_aux_postes_qui_realisent() {
    let c = corpus();
    let travaux: Vec<&Appel> = c.iter().filter(|a| a.kind == "work" && a.prompt.contains("Carry it out in the project")).collect();
    assert_eq!(travaux.len(), 2, "deux postes de réalisation");
    for a in travaux {
        assert!(contient(a, "file scope"), "appel {} : périmètre absent", a.n);
        assert!(a.sys.contains("**"), "appel {} : motif de périmètre absent", a.n);
        assert!(contient(a, "only within it"), "appel {} : limite de modification absente", a.n);
    }
}

#[test]
fn le_plan_n_a_pas_de_perimetre_de_fichiers() {
    assert!(!corpus()[0].sys.contains("File scope assigned to you"), "le plan ne modifie rien");
}

/// Ce qui remplace les permissions et les boucles de correction : chaque poste sait quoi chercher seul,
/// quoi demander à l'humain, et quand s'arrêter d'insister.
#[test]
fn chaque_poste_sait_chercher_seul_et_demander_a_l_humain() {
    for a in corpus() {
        assert!(contient(a, "look up yourself"), "appel {} : « chercher soi-même » absent", a.n);
        assert!(contient(a, "mcp__essaim__ask_human"), "appel {} : outil de demande à l'humain absent", a.n);
        assert!(contient(a, "never ask your supervisor"), "appel {} : interdiction de questionner le supérieur absente", a.n);
        assert!(contient(a, "at most two attempts"), "appel {} : borne des tentatives absente", a.n);
        assert!(contient(a, "notes/decisions.md"), "appel {} : journal des décisions de l'humain absent", a.n);
        assert!(contient(a, "never modify anything outside"), "appel {} : limite au dossier du projet absente", a.n);
    }
    // Le boss pose ses questions avant d'écrire le plan.
    assert!(contient(&corpus()[0], "before writing, ask the human"));
}

/// La délégation distribue l'information : des pointeurs vers le contexte, pas des copies.
#[test]
fn la_delegation_donne_des_pointeurs_de_contexte() {
    for a in corpus().iter().filter(|a| a.kind == "delegate") {
        assert!(contient(a, "`context`") && contient(a, "instead of copying"), "délégation {} : pointeurs de contexte absents", a.n);
    }
    for a in corpus().iter().filter(|a| a.kind == "work") {
        assert!(contient(a, "context pointers from your supervisor"), "appel {} : les pointeurs du supérieur ne sont pas arrivés", a.n);
    }
}

#[test]
fn la_revue_porte_sur_les_criteres_du_brief() {
    let revues: Vec<&Appel> = corpus().iter().filter(|a| a.kind == "review").collect();
    assert_eq!(revues.len(), 1);
    for a in revues {
        assert!(!contient(a, "assignments"), "revue {} : elle constate, elle ne relance rien", a.n);
        assert!(contient(a, "the human reads it and decides"), "revue {} : la décision revient à l'humain", a.n);
        for mot in ["success criteria", "accepted", "findings"] {
            assert!(contient(a, mot), "revue {} : « {mot} » absent", a.n);
        }
        assert!(contient(a, "UTF-8"), "revue {} : critères du brief absents", a.n);
        // les résultats des subordonnés sont soumis à la revue
        assert!(contient(a, "rapport de"), "revue {} : livrables des subordonnés absents", a.n);
    }
}

#[test]
fn le_rapport_reprend_la_tache_et_les_resultats() {
    for a in corpus().iter().filter(|a| a.kind == "report") {
        assert!(a.prompt.starts_with("Original task"), "rapport {} : tâche d'origine absente", a.n);
        assert!(contient(a, "rapport de"), "rapport {} : résultats des subordonnés absents", a.n);
        assert!(contient(a, "not completed"), "rapport {} : consigne sur ce qui n'a pas abouti absente", a.n);
    }
}

#[test]
fn l_aiguillage_de_modele_reste_propose() {
    // Les prompts de délégation laissent choisir le niveau de modèle par tâche.
    for a in corpus().iter().filter(|a| a.kind == "delegate") {
        assert!(contient(a, "model") && contient(a, "haiku") && contient(a, "sonnet"), "délégation {} : choix de modèle absent", a.n);
        assert!(contient(a, "sections"), "délégation {} : gabarit de réponse absent", a.n);
    }
}
