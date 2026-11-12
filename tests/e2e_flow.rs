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
fn nominal_plan_du_boss_puis_une_seule_descente() {
    let (b, l, ap) = lance("e2e-nominal", "nominal", true, BRIEF_LONG);
    assert!(l.ok(), "code {:?}\n{}", l.code, l.stderr);
    assert!(l.stderr.contains("plan 1 + réalisation 12"), "annonce des appels : {}", l.stderr);
    // Le plan d'abord, la délégation du boss ensuite, sa revue puis son rapport à la fin ; entre les deux,
    // les branches voisines s'entrelacent (seule la composition est fixe).
    let ks = kinds(&ap);
    assert_eq!(ap.len(), 13, "{ks:?}");
    assert_eq!(&ks[..2], ["plan", "delegate"]);
    assert_eq!(&ks[11..], ["review", "report"]);
    let compte = |k: &str| ks.iter().filter(|x| **x == k).count();
    assert_eq!((compte("plan"), compte("delegate"), compte("work"), compte("report"), compte("review")), (1, 4, 3, 4, 1), "{ks:?}");
    // Le plan : un seul appel, celui du boss, avec le brief. Tout le reste est la réalisation.
    assert!(ap[0].sys.contains("PLAN PHASE") && ap[0].sys.contains("export CSV"));
    assert!(ap[0].prompt.contains("ask the human") && ap[0].prompt.contains("Backend Engineer"), "questions à l'humain et organigramme complet dans la consigne du plan");
    for a in &ap[1..] {
        assert!(a.sys.contains("REALISATION PHASE"), "appel {} ({}) devrait être en réalisation", a.n, a.kind);
    }
    // Le plan validé revient au boss, qui le découpe.
    assert!(ap[1].prompt.contains("Plan de ceo"), "{}", ap[1].prompt);
    // Les postes ⚒ réalisent ; le poste d'analyse rend un livrable. Chacun reçoit les pointeurs de son supérieur.
    let travaux: Vec<&Appel> = ap.iter().filter(|a| a.kind == "work").collect();
    assert_eq!(travaux.iter().filter(|a| a.prompt.contains("Carry it out in the project")).count(), 2);
    assert_eq!(travaux.iter().filter(|a| a.prompt.contains("Produce your deliverable")).count(), 1);
    assert!(travaux.iter().all(|a| a.prompt.contains("Context pointers from your supervisor") && a.prompt.contains("/mod.rs:1-40")));
    // livrables
    let m = mission(&b);
    for f in ["brief.md", "conception.md", "conception.ok", "rapport.md", "reussie"] {
        assert!(m.join(f).is_file(), "{f} manquant");
    }
    assert!(m.join("notes").is_dir(), "dossier des notes partagées");
    assert!(std::fs::read_to_string(m.join("conception.md")).unwrap().contains("Plan de ceo"));
    assert!(std::fs::read_to_string(m.join("rapport.md")).unwrap().contains("Rapport de"));
}

/// Profil « appareil dédié » : tous les postes, à tous les types d'appel, ont tous les outils et rien n'est
/// refusé. Chacun a le serveur MCP d'essaim, par lequel il s'adresse à l'humain.
#[test]
fn profil_dedie_tous_les_droits_pour_tous_les_postes() {
    let (_b, l, ap) = lance("e2e-dedie", "nominal", true, BRIEF_COURT);
    assert!(l.ok(), "{}", l.stderr);
    assert!(l.stderr.contains("profil : Appareil dédié"), "{}", l.stderr);
    for a in &ap {
        assert_eq!(a.droit("--tools"), "Read,Grep,Glob,Edit,Write,Bash,WebSearch,WebFetch", "appel {} ({})", a.n, a.kind);
        assert_eq!(a.droit("--permission-mode"), "bypassPermissions", "appel {} ({})", a.n, a.kind);
        assert!(a.droit("--settings").is_empty() && a.droit("--permission-prompt-tool").is_empty() && a.droit("--allowedTools").is_empty(), "appel {} : {}", a.n, a.droits);
        let mcp: serde_json::Value = serde_json::from_str(a.droit("--mcp-config")).unwrap_or_else(|e| panic!("appel {} : configuration MCP illisible ({e}) : {}", a.n, a.droits));
        let args = mcp["mcpServers"]["essaim"]["args"].to_string();
        assert!(args.contains("\"mcp\"") && args.contains("--socket") && !args.contains("--permission"), "{args}");
        assert!(mcp["mcpServers"]["essaim"]["env"]["ESSAIM_BUS_TOKEN"].is_string());
    }
}

/// Profil « protégé » : mêmes outils, mais les commandes tournent dans le bac à sable et ce qui en sort
/// passe par l'outil de permission d'essaim, donc par l'humain.
#[test]
fn profil_protege_bac_a_sable_et_outil_de_permission() {
    let b = bac("e2e-protege");
    profil(&b, "protected");
    let ent = ecrire_entreprise(&b, "mini", &entreprise_petite());
    let l = lancer(&b, &ent, BRIEF_COURT, "nominal", "dev");
    assert!(l.ok(), "{}", l.stderr);
    assert!(l.stderr.contains("profil : Protégé"), "{}", l.stderr);
    for a in &appels(&b) {
        assert_eq!(a.droit("--tools"), "Read,Grep,Glob,Edit,Write,Bash,WebSearch,WebFetch");
        assert_eq!(a.droit("--permission-mode"), "acceptEdits");
        assert_eq!(a.droit("--permission-prompt-tool"), "mcp__essaim__permission");
        assert_eq!(a.droit("--allowedTools"), "WebSearch,mcp__essaim__ask_human", "ni la recherche web ni une question à l'humain ne demandent d'autorisation");
        let st: serde_json::Value = serde_json::from_str(a.droit("--settings")).unwrap();
        assert_eq!(st["sandbox"]["enabled"], true);
        assert_eq!(st["sandbox"]["failIfUnavailable"], true);
        assert_eq!(st["permissions"]["blockReadsOutsideWorkingDirectories"], true);
        assert!(a.droit("--mcp-config").contains("--permission"), "le serveur MCP expose l'outil de permission");
    }
}

#[test]
fn entreprise_d_analyse_meme_parcours_sans_poste_de_realisation() {
    let (b, l, ap) = lance("e2e-analyse", "nominal", false, BRIEF_LONG);
    assert!(l.ok(), "{}", l.stderr);
    assert_eq!(ap.len(), 13, "{:?}", kinds(&ap));
    assert!(ap.iter().filter(|a| a.kind == "work").all(|a| a.prompt.contains("Produce your deliverable") && a.sys.contains("You analyse and advise")));
    let m = mission(&b);
    assert!(m.join("conception.md").is_file() && m.join("rapport.md").is_file() && m.join("reussie").is_file());
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

/// La revue ne valide pas le résultat et l'humain demande de relancer : une descente corrective, limitée
/// aux écarts, puis une nouvelle revue. Rien ne repart sans sa décision.
#[test]
fn revue_rejetee_l_humain_relance_et_la_mission_aboutit() {
    let b = bac("e2e-redo");
    let ent = ecrire_entreprise(&b, "acme", &entreprise_sept(true));
    let l = lancer_dialogue(&b, &ent, BRIEF_LONG, "review_redo", "dev1", "1\n");
    assert!(l.ok(), "{}", l.stderr);
    assert!(l.stderr.contains("revue de CEO") && l.stderr.contains("il manque un livrable") && l.stderr.contains("1. Relancer pour corriger ces écarts"), "{}", l.stderr);
    let ap = appels(&b);
    let ks = kinds(&ap);
    assert_eq!(ks.iter().filter(|k| **k == "review").count(), 2, "{ks:?}");
    assert_eq!(ap.len(), 13 + 11, "le flux de base, puis une descente corrective et sa revue : {ks:?}");
    // La revue rejetée est suivie d'une nouvelle délégation du boss, qui porte les écarts.
    assert_eq!(&ks[11..13], ["review", "delegate"]);
    assert!(ap[12].prompt.contains("il manque un livrable") && ap[12].prompt.contains("assign only what is needed"), "{}", ap[12].prompt);
    assert_eq!(ks.iter().filter(|k| **k == "report").count(), 4 + 3, "un seul rapport final du boss : {ks:?}");
    assert!(mission(&b).join("reussie").is_file());
}

/// Même revue rejetée, mais personne ne répond (`--yes`) : aucune relance automatique, la mission n'aboutit pas.
#[test]
fn revue_rejetee_sans_humain_rien_ne_repart() {
    let (b, l, ap) = lance("e2e-unmet", "review_unmet", true, BRIEF_LONG);
    assert_eq!(l.code, Some(1), "{}", l.stderr);
    assert!(l.stderr.contains("--yes : personne pour répondre"), "{}", l.stderr);
    assert!(l.stderr.contains("Mission non aboutie") && l.stderr.contains("critère X non atteint"), "{}", l.stderr);
    let ks = kinds(&ap);
    assert_eq!(ks.iter().filter(|k| **k == "review").count(), 1, "{ks:?}");
    assert_eq!(ap.len(), 13, "{ks:?}");
    assert!(!mission(&b).join("reussie").exists());
}

/// Un poste n'aboutit pas : aucun supérieur n'est appelé pour « corriger ». Sans humain, la mission s'arrête
/// sur ce qui manque, nommé.
#[test]
fn poste_en_echec_sans_humain_aucune_boucle_de_correction() {
    for mode in ["fail", "crash", "cut"] {
        let (b, l, ap) = lance(&format!("e2e-{mode}"), mode, true, BRIEF_LONG);
        assert_eq!(l.code, Some(1), "{mode} : {}", l.stderr);
        assert!(l.stderr.contains("Mission non aboutie"), "{mode} : {}", l.stderr);
        assert!(l.stderr.contains("poste bloqué : Backend Engineer"), "{mode} : la demande nomme le poste : {}", l.stderr);
        let ks = kinds(&ap);
        assert!(!ks.contains(&"review"), "{mode} : pas de revue d'un résultat que l'on sait incomplet : {ks:?}");
        // Ni le poste ni ses supérieurs ne rejouent quoi que ce soit : chaque poste interne délègue une fois.
        assert_eq!(ks.iter().filter(|k| **k == "delegate").count(), 4, "{mode} : {ks:?}");
        assert_eq!(ks.iter().filter(|k| **k == "work").count(), 3, "{mode} : le poste en échec n'est pas rejoué : {ks:?}");
        assert!(!mission(&b).join("reussie").exists(), "{mode}");
    }
}

/// Le même poste en échec, avec un humain : il lit ce qui bloque, répond, et le poste reprend SA session
/// avec cette réponse. La mission aboutit.
#[test]
fn poste_en_echec_l_humain_repond_et_le_poste_reprend_sa_session() {
    let b = bac("e2e-reponse");
    let ent = ecrire_entreprise(&b, "acme", &entreprise_sept(true));
    let l = lancer_dialogue(&b, &ent, BRIEF_LONG, "fail", "dev1", "le port est 8080\n");
    assert!(l.ok(), "{}", l.stderr);
    assert!(l.stderr.contains("poste bloqué : Backend Engineer") && l.stderr.contains("boum"), "{}", l.stderr);
    let ap = appels(&b);
    let echec = ap.iter().find(|a| a.kind == "work" && a.out == "boum").expect("l'appel en échec");
    let reprises: Vec<&Appel> = ap.iter().filter(|a| !a.resume.is_empty()).collect();
    assert_eq!(reprises.len(), 1, "une seule reprise de session dans toute la mission");
    let reprise = reprises[0];
    assert_eq!(reprise.resume, format!("fake-{}", echec.n), "la reprise porte sur la session du poste");
    assert!(reprise.prompt.contains("The human answers what stopped you:\nle port est 8080"), "{}", reprise.prompt);
    assert_eq!(ap.len(), 13 + 1, "un seul appel de plus : la reprise ({:?})", kinds(&ap));
    assert!(mission(&b).join("reussie").is_file());
}

#[test]
fn le_faux_processus_est_seul_appele_et_hors_reseau() {
    // Le lancement réussit sans `claude` dans le PATH : seul ESSAIM_CLAUDE_BIN est utilisé.
    let b = bac("e2e-horsreseau");
    let ent = ecrire_entreprise(&b, "mini", &entreprise_petite());
    let l = lancer(&b, &ent, BRIEF_COURT, "nominal", "dev");
    assert!(l.ok(), "{}", l.stderr);
    let ap = appels(&b);
    assert_eq!(kinds(&ap), ["plan", "delegate", "delegate", "work", "report", "review", "report"]);
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
    // L'appel refusé est rejoué : dev1 a bien produit son travail (13 appels + la tentative limitée).
    assert_eq!(ap.len(), 14, "{:?}", kinds(&ap));
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
    assert_eq!(appels(&b).len(), 13, "mission complète menée avec les agents personnels");
    assert!(mission(&b).join("reussie").is_file());
    // Les missions, elles, restent dans le projet.
    assert!(b.racine.join(".essaim/missions").is_dir());
    assert!(!home.join(".essaim").exists(), "le dossier personnel ne reçoit pas les missions");
}

// ------------------------------------------------------------------ profil de sécurité (`essaim profile`)

fn essaim_profile(config: &std::path::Path, args: &[&str], clavier: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_essaim"))
        .arg("profile")
        .args(args)
        .env("ESSAIM_CONFIG_DIR", config)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(clavier.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// C'est la question que pose l'installeur : les deux profils sont expliqués, la réponse est enregistrée
/// sans toucher aux autres réglages.
#[test]
fn le_profil_se_choisit_au_clavier_avec_son_explication() {
    let b = bac("e2e-profil");
    let config = config_dir(&b);
    std::fs::write(config.join("settings.toml"), "lang = \"fr\"\n").unwrap();
    let o = essaim_profile(&config, &[], "x\n1\n");
    let t = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(o.status.success(), "{t}");
    assert!(t.contains("1. Appareil dédié") && t.contains("2. Protégé"), "{t}");
    assert!(t.contains("SANS données personnelles") && t.contains("bac à sable"), "les deux profils sont expliqués : {t}");
    assert!(t.contains("Répondez 1 ou 2."), "une réponse illisible est redemandée : {t}");
    let reglages = std::fs::read_to_string(config.join("settings.toml")).unwrap();
    assert!(reglages.contains("profile = \"dedicated\"") && reglages.contains("lang = \"fr\""), "{reglages}");
    // Entrée seule garde le profil courant ; un argument répond d'avance (installeur `--profile`).
    essaim_profile(&config, &[], "\n");
    assert!(std::fs::read_to_string(config.join("settings.toml")).unwrap().contains("profile = \"dedicated\""));
    let o = essaim_profile(&config, &["protected"], "");
    assert!(o.status.success());
    assert!(std::fs::read_to_string(config.join("settings.toml")).unwrap().contains("profile = \"protected\""));
    assert!(!essaim_profile(&config, &["nimporte"], "").status.success(), "profil inconnu refusé");
}

/// Sans clavier (installation non interactive) : rien n'est imposé, le profil protégé s'applique par défaut.
#[test]
fn sans_clavier_le_profil_reste_protege() {
    let b = bac("e2e-profil-muet");
    let config = config_dir(&b);
    std::fs::remove_file(config.join("settings.toml")).unwrap();
    let o = essaim_profile(&config, &[], "");
    assert!(o.status.success());
    assert!(std::fs::read_to_string(config.join("settings.toml")).unwrap().contains("profile = \"protected\""));
}

// ------------------------------------------------------------------ serveur MCP (`essaim mcp`)

/// Le vrai binaire `essaim mcp`, tel que `claude` le lance : poignée de main MCP sur stdin/stdout, puis un
/// appel d'outil relayé au bus par la socket, avec le jeton, le nœud et le poste ; la réponse du bus revient
/// à l'agent comme résultat de l'outil.
#[test]
fn le_serveur_mcp_relaie_la_question_de_l_agent_jusqu_au_bus() {
    use std::io::{BufRead, BufReader, Write};
    let b = bac("e2e-mcp");
    // Chemin court : une socket Unix ne dépasse pas une centaine d'octets.
    let socket = std::env::temp_dir().join(format!("essaim-test-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let ecoute = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_essaim"))
        .args(["mcp", "--socket"])
        .arg(&socket)
        .args(["--node", "dev1/realisation/work", "--seat", "Backend Engineer"])
        .env("ESSAIM_BUS_TOKEN", "jeton-de-test")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut entree = child.stdin.take().unwrap();
    let mut sortie = BufReader::new(child.stdout.take().unwrap());
    let mut lire = || {
        let mut l = String::new();
        sortie.read_line(&mut l).unwrap();
        serde_json::from_str::<serde_json::Value>(&l).unwrap_or_else(|e| panic!("réponse illisible ({e}) : {l:?}"))
    };
    writeln!(entree, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}}}}}}"#).unwrap();
    assert_eq!(lire()["result"]["serverInfo"]["name"], "essaim");
    writeln!(entree, r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#).unwrap();
    writeln!(entree, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#).unwrap();
    let outils = lire();
    assert_eq!(outils["result"]["tools"].as_array().unwrap().len(), 1, "sans --permission : le seul outil est ask_human");
    assert_eq!(outils["result"]["tools"][0]["name"], "ask_human");
    writeln!(entree, r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"ask_human","arguments":{{"kind":"question","questions":[{{"text":"Quel port ?"}}]}}}}}}"#).unwrap();
    // Côté bus : la demande arrive avec de quoi l'attribuer et l'authentifier.
    let (flux, _) = ecoute.accept().unwrap();
    let mut demande = String::new();
    BufReader::new(flux.try_clone().unwrap()).read_line(&mut demande).unwrap();
    let d: serde_json::Value = serde_json::from_str(&demande).unwrap();
    assert_eq!((d["token"].as_str(), d["node"].as_str(), d["seat"].as_str(), d["tool"].as_str()), (Some("jeton-de-test"), Some("dev1/realisation/work"), Some("Backend Engineer"), Some("ask_human")));
    assert_eq!(d["args"]["questions"][0]["text"], "Quel port ?");
    writeln!(&flux, r#"{{"text":"Q: Quel port ?\nA: 8080","is_error":false}}"#).unwrap();
    let r = lire();
    assert_eq!(r["id"], 3);
    assert_eq!(r["result"]["content"][0]["text"], "Q: Quel port ?\nA: 8080");
    assert_eq!(r["result"]["isError"], false);
    // `claude` ferme l'entrée : le serveur s'arrête.
    drop(entree);
    assert!(child.wait().unwrap().success());
    let _ = std::fs::remove_file(&socket);
    drop(b);
}
