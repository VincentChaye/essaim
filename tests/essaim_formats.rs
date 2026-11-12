//! Compatibilité des formats persistés `.essaim` / `.claude` : des fichiers écrits par une version antérieure
//! (tests/fixtures/formats/) doivent rester lisibles, et une exécution ne doit ni en changer le sens ni
//! introduire de nouveau fichier dans les dossiers de mission. Faux `claude` uniquement.
#![cfg(unix)]
mod common;
use common::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/formats")
}

fn copier(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let (s, d) = (e.path(), dst.join(e.file_name()));
        if s.is_dir() {
            copier(&s, &d);
        } else {
            std::fs::copy(&s, &d).unwrap();
        }
    }
}

/// Bac avec les fixtures copiées dans le projet.
fn bac_fixtures(nom: &str) -> Bac {
    let b = bac(nom);
    copier(&fixtures().join("companies"), &b.racine.join(".claude/companies"));
    copier(&fixtures().join("agents"), &b.racine.join(".claude/agents"));
    copier(&fixtures().join("missions"), &b.racine.join(".essaim/missions"));
    b
}

fn essaim(b: &Bac, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_essaim"))
        .args(args)
        .arg("--root")
        .arg(&b.racine)
        .current_dir(&b.racine)
        .env("ESSAIM_CLAUDE_BIN", &b.fake_bin)
        .env("FAKE_DIR", &b.fake_dir)
        .env("FAKE_MODE", "nominal")
        .env("FAKE_TARGET", "dev")
        .env("HOME", b.racine.parent().unwrap().join("home"))
        .env("ESSAIM_CONFIG_DIR", config_dir(b))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn texte(o: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn entreprises_anciennes_se_valident() {
    let b = bac_fixtures("formats-check");
    for f in ["ancienne", "minimale"] {
        let chemin = b.racine.join(format!(".claude/companies/{f}.toml"));
        let o = essaim(&b, &["company", "check", chemin.to_str().unwrap()]);
        assert!(o.status.success(), "{f} : {}", texte(&o));
    }
}

#[test]
fn l_organigramme_d_une_ancienne_entreprise_est_relu_en_entier() {
    let b = bac_fixtures("formats-show");
    let chemin = b.racine.join(".claude/companies/ancienne.toml");
    // `company show` n'a pas d'option --root : commande directe.
    let o = Command::new(env!("CARGO_BIN_EXE_essaim")).args(["company", "show"]).arg(&chemin).current_dir(&b.racine).env("HOME", b.racine.parent().unwrap().join("home")).output().unwrap();
    assert!(o.status.success(), "{}", texte(&o));
    let t = texte(&o);
    for mot in ["CEO", "Tech Lead", "Backend Engineer"] {
        assert!(t.contains(mot), "« {mot} » absent de l'organigramme :\n{t}");
    }
}

#[test]
fn un_agent_ecrit_a_la_main_est_liste() {
    let b = bac_fixtures("formats-agents");
    let o = essaim(&b, &["agents"]);
    assert!(o.status.success(), "{}", texte(&o));
    let t = texte(&o);
    assert!(t.contains("revue-externe"), "{t}");
    assert!(t.contains("developer"), "agents d'exemple conservés : {t}");
}

#[test]
fn les_valeurs_par_defaut_d_une_entreprise_minimale_sont_stables() {
    // Une entreprise ne précisant que ses postes doit toujours être exécutable : valeurs par défaut inchangées.
    let b = bac_fixtures("formats-minimale");
    let ent = b.racine.join(".claude/companies/minimale.toml");
    let l = lancer(&b, &ent, BRIEF_COURT, "nominal", "dev");
    assert!(l.ok(), "{}", l.stderr);
    assert!(l.stderr.contains("plafond 40"), "max_calls par défaut = 40 : {}", l.stderr);
    assert!(l.stderr.contains("2 en parallèle"), "max_parallel par défaut = 2 : {}", l.stderr);
}

#[test]
fn une_mission_ancienne_reprend_a_la_realisation_sans_rejouer_la_conception() {
    let b = bac_fixtures("formats-reprise");
    let avant = std::fs::read_to_string(b.racine.join(".essaim/missions/0000000000001-fix1/conception.md")).unwrap();
    let ent = b.racine.join(".claude/companies/ancienne.toml");
    let o = essaim(&b, &["company", "run", ent.to_str().unwrap(), "--mission", "0000000000001-fix1", "--resume", "--yes"]);
    assert!(o.status.success(), "{}", texte(&o));
    let ap = appels(&b);
    assert!(!ap.is_empty());
    assert!(ap.iter().all(|a| a.sys.contains("REALISATION PHASE")), "la conception ne doit pas être rejouée");
    // le plan de conception est transmis tel quel à la réalisation
    assert!(ap.iter().any(|a| tout(a).contains("src/export/**")), "plan de conception non transmis");
    let m = b.racine.join(".essaim/missions/0000000000001-fix1");
    assert_eq!(std::fs::read_to_string(m.join("brief.md")).unwrap(), std::fs::read_to_string(fixtures().join("missions/0000000000001-fix1/brief.md")).unwrap());
    assert_eq!(std::fs::read_to_string(m.join("conception.md")).unwrap(), avant);
    assert!(m.join("rapport.md").is_file() && m.join("reussie").is_file());
}

fn tout(a: &Appel) -> String {
    format!("{}\n{}", a.sys, a.prompt)
}

#[test]
fn une_execution_n_ajoute_que_des_fichiers_de_mission_connus() {
    let b = bac("formats-noms");
    let ent = ecrire_entreprise(&b, "acme", &entreprise_sept(true));
    let l = lancer(&b, &ent, BRIEF_LONG, "nominal", "dev1");
    assert!(l.ok(), "{}", l.stderr);
    // `notes` : dossier des notes partagées (livrables longs des postes, décisions de l'humain), ajouté avec la descente unique.
    let connus = ["brief.md", "conception.md", "conception.ok", "rapport.md", "reussie", "company", "after", "resolution.md", "compte-rendu.md", "notes"];
    let racine = b.racine.join(".essaim/missions");
    let mut vus = 0;
    for m in std::fs::read_dir(&racine).unwrap().flatten() {
        for f in std::fs::read_dir(m.path()).unwrap().flatten() {
            let nom = f.file_name().to_string_lossy().into_owned();
            assert!(connus.contains(&nom.as_str()), "fichier de mission inattendu : {nom} (format persisté modifié ?)");
            vus += 1;
        }
    }
    assert!(vus >= 4);
    // Le marqueur d'entreprise reste le nom du fichier .toml, lisible par les anciennes versions.
    let m = std::fs::read_dir(&racine).unwrap().flatten().next().unwrap().path();
    if let Ok(c) = std::fs::read_to_string(m.join("company")) {
        assert!(c.trim().ends_with(".toml"), "{c}");
    }
}

#[test]
fn les_fichiers_de_mission_sont_prives() {
    use std::os::unix::fs::PermissionsExt;
    let b = bac("formats-droits");
    let ent = ecrire_entreprise(&b, "acme", &entreprise_sept(false));
    let l = lancer(&b, &ent, BRIEF_LONG, "nominal", "dev1");
    assert!(l.ok(), "{}", l.stderr);
    let m = std::fs::read_dir(b.racine.join(".essaim/missions")).unwrap().flatten().next().unwrap().path();
    for f in ["brief.md", "conception.md"] {
        let mode = std::fs::metadata(m.join(f)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{f} doit rester privé (0600)");
    }
}
