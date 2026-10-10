//! Mesures de référence (étape A, avant correctifs) : tests `#[ignore]` chronométrés et compteur d'allocations.
//!
//! Lancer : `cargo test --release --test perf_baseline -- --ignored --nocapture --test-threads=1`
//!
//! Le crate est un binaire sans `lib.rs` : les modules mesurés sont inclus par `#[path]` (aucune modification de `src/`).
//! Fixtures : `tests/fixtures/perf/` (voir son README) ; les volumes sont générés de façon déterministe.
//! Le compteur d'allocations (`#[global_allocator]`) n'existe que dans ce binaire de test.

#![allow(dead_code)]

#[path = "../src/runtime/claude_proc.rs"]
mod claude_proc;
#[path = "../src/runtime/scope.rs"]
mod scope;
#[path = "../src/model/ident.rs"]
mod ident;

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// Allocateur système qui compte les allocations (appels) et les octets demandés.
struct Compteur;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static OCTETS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: délègue toutes les opérations à l'allocateur système ; seuls des compteurs atomiques sont ajoutés.
unsafe impl GlobalAlloc for Compteur {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        OCTETS.fetch_add(l.size(), Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, nouvelle_taille: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        OCTETS.fetch_add(nouvelle_taille, Relaxed);
        unsafe { System.realloc(p, l, nouvelle_taille) }
    }
}

#[global_allocator]
static GLOBAL: Compteur = Compteur;

/// Exécute `f` une fois à blanc, puis `n` fois chronométrées. Affiche médiane, min, max et allocations par exécution.
fn mesure(id: &str, description: &str, n: usize, mut f: impl FnMut()) {
    assert!(n >= 1);
    f();
    let (a0, o0) = (ALLOCATIONS.load(Relaxed), OCTETS.load(Relaxed));
    let mut durees: Vec<Duration> = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        f();
        durees.push(t.elapsed());
    }
    let allocs = (ALLOCATIONS.load(Relaxed) - a0) / n;
    let octets = (OCTETS.load(Relaxed) - o0) / n;
    durees.sort();
    eprintln!(
        "[{id}] {description} | n={n} médiane={:?} min={:?} max={:?} | allocs/exéc={allocs} octets/exéc={octets}",
        durees[n / 2],
        durees[0],
        durees[n - 1]
    );
}

/// Contexte de machine, relevé à chaque campagne (la marque du CPU n'est pas relevée ici : voir le rapport).
#[test]
#[ignore = "contexte de mesure"]
fn contexte_machine() {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    eprintln!("[machine] os={} arch={} cpus={cpus}", std::env::consts::OS, std::env::consts::ARCH);
}

/// Échantillon NDJSON versionné (`tests/fixtures/perf/stream-sample.jsonl`).
const FIXTURE_FLUX: &str = include_str!("fixtures/perf/stream-sample.jsonl");

/// Flux de `n` lignes construit en cyclant l'échantillon (déterministe).
fn flux(n: usize) -> Vec<String> {
    let modele: Vec<&str> = FIXTURE_FLUX.lines().filter(|l| !l.trim().is_empty()).collect();
    (0..n).map(|i| modele[i % modele.len()].to_string()).collect()
}

/// P1 : analyse NDJSON du flux `claude -p --output-format stream-json` (chemin chaud : une ligne par événement).
#[test]
#[ignore = "mesure de référence P1"]
fn p1_parse_line_flux_5000_lignes() {
    let lignes = flux(5_000);
    mesure("P1", "claude_proc::parse_line × 5000 lignes", 15, || {
        for l in &lignes {
            std::hint::black_box(claude_proc::parse_line(l).ok());
        }
    });
}

/// P2 : masquage des secrets appliqué à la sortie affichée ou persistée (≈ 1 Mio, secrets factices).
#[test]
#[ignore = "mesure de référence P2"]
fn p2_redact_secrets_1_mio() {
    let texte = "résultat ok ANTHROPIC_API_KEY=abc123 sk-ant-api03-XYZ123 fin\n".repeat(16_000);
    mesure("P2", "claude_proc::redact_secrets sur ≈1 Mio", 15, || {
        std::hint::black_box(claude_proc::redact_secrets(&texte));
    });
}

/// P3 : regroupement des périmètres en lots sans chevauchement (placement glouton, O(n × lots × k²)).
#[test]
#[ignore = "mesure de référence P3"]
fn p3_batches_400_perimetres() {
    let perimetres: Vec<Vec<String>> = (0..400).map(|i| vec![format!("src/m{}/**", i % 40), format!("docs/f{i}.md")]).collect();
    mesure("P3", "scope::batches sur 400 périmètres", 15, || {
        std::hint::black_box(scope::batches(&perimetres));
    });
}

/// P4 : écriture atomique (temporaire, `sync_all`, `rename`) : coût E/S dominé par `fsync`.
#[test]
#[ignore = "mesure de référence P4"]
fn p4_write_atomic_20_ecritures() {
    let dir = std::env::temp_dir().join(format!("essaim-perf-p4-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fichier = dir.join("donnee.toml");
    let contenu = "cle = \"valeur\"\n".repeat(300);
    mesure("P4", "ident::write_atomic × 20 (≈4 Kio, fsync)", 10, || {
        for _ in 0..20 {
            ident::write_atomic(&fichier, &contenu).unwrap();
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// Dossier projet avec `n` agents `.claude/agents/agent-NNNN.md` (déterministe).
fn projet_agents(n: usize) -> PathBuf {
    let racine = std::env::temp_dir().join(format!("essaim-perf-p5-{n}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&racine);
    let agents = racine.join(".claude/agents");
    std::fs::create_dir_all(&agents).unwrap();
    for i in 0..n {
        let md = format!("---\nname: agent-{i:04}\ndescription: Agent de perf {i}\nmodel: haiku\n---\n\nTu es l'agent {i}.\n");
        std::fs::write(agents.join(format!("agent-{i:04}.md")), md).unwrap();
    }
    racine
}

/// P5 : processus `essaim agents --root` sur 300 agents (temps mur ; les allocations ne concernent que ce processus de test).
#[test]
#[ignore = "mesure de référence P5 (processus CLI)"]
fn p5_cli_agents_300_fichiers() {
    let racine = projet_agents(300);
    let bin = env!("CARGO_BIN_EXE_essaim");
    mesure("P5", "essaim agents --root (300 agents, processus)", 7, || {
        let sortie = std::process::Command::new(bin).arg("agents").arg("--root").arg(&racine).output().unwrap();
        assert!(sortie.status.success(), "{}", String::from_utf8_lossy(&sortie.stderr));
    });
    let _ = std::fs::remove_dir_all(&racine);
}

/// P6 : analyse d'une ligne `assistant` géante (≈ 1 Mio de texte).
#[test]
#[ignore = "mesure de référence P6"]
fn p6_parse_line_ligne_geante_1_mio() {
    let texte = "mot ".repeat(262_144);
    let ligne = serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": texte}]}}).to_string();
    mesure("P6", "claude_proc::parse_line d'une ligne assistant ≈1 Mio", 15, || {
        std::hint::black_box(claude_proc::parse_line(&ligne).ok());
    });
}

/// Fichiers `.rs` de `dir`, récursivement.
fn collecte_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collecte_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Clé d'une fenêtre : ses lignes normalisées jointes.
fn cle(fenetre: &[(usize, String)]) -> String {
    fenetre.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n")
}

/// R (redondance) : fenêtres de 6 lignes normalisées (trim, sans commentaires ni lignes de moins de 12 caractères)
/// présentes au moins deux fois dans `src/` (tests inclus). Commande : `cargo test --test perf_baseline redondance -- --ignored --nocapture`.
#[test]
#[ignore = "mesure R : lignes dupliquées dans src/"]
fn redondance_fenetres_dupliquees_dans_src() {
    const FENETRE: usize = 6;
    const MIN_CARACTERES: usize = 12;
    let mut fichiers = vec![];
    collecte_rs(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut fichiers);
    fichiers.sort();
    let sequences: Vec<Vec<(usize, String)>> = fichiers
        .iter()
        .map(|f| {
            std::fs::read_to_string(f)
                .unwrap_or_default()
                .lines()
                .enumerate()
                .map(|(i, l)| (i + 1, l.trim().to_string()))
                .filter(|(_, l)| l.chars().count() >= MIN_CARACTERES && !l.starts_with("//"))
                .collect()
        })
        .collect();
    let total: usize = sequences.iter().map(Vec::len).sum();
    let mut compte: HashMap<String, usize> = HashMap::new();
    for seq in &sequences {
        for w in seq.windows(FENETRE) {
            *compte.entry(cle(w)).or_insert(0) += 1;
        }
    }
    let doublons: Vec<usize> = compte.values().copied().filter(|&c| c >= 2).collect();
    let groupes = doublons.len();
    let en_trop: usize = doublons.iter().map(|c| c - 1).sum();
    let mut lignes_concernees = 0usize;
    for seq in &sequences {
        let mut marque = vec![false; seq.len()];
        for (k, w) in seq.windows(FENETRE).enumerate() {
            if compte[&cle(w)] >= 2 {
                marque[k..k + FENETRE].iter_mut().for_each(|m| *m = true);
            }
        }
        lignes_concernees += marque.iter().filter(|&&m| m).count();
    }
    let nb_fichiers = fichiers.len();
    eprintln!(
        "[R] fichiers={nb_fichiers} lignes_normalisees={total} fenetres={FENETRE} groupes_dupliques={groupes} occurrences_en_trop={en_trop} lignes_concernees={lignes_concernees}"
    );
}
