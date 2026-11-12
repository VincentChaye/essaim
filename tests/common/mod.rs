//! Outillage commun des tests de bout en bout et du harnais de mesure : faux `claude` (script shell),
//! bac à sable de projet, lancement du binaire `essaim company run` et relecture des appels enregistrés.
//!
//! Aucun appel réseau, aucune horloge : le faux processus répond instantanément et de façon déterministe.
//! Le processus `claude` est substitué via `ESSAIM_CLAUDE_BIN` (seule porte d'entrée prévue par le code).
#![allow(dead_code)]
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Jetons synthétiques d'un volume en octets : `ceil(octets / 4)`. Formule unique, voir `tests/mesure/README.md`.
pub fn jetons(octets: usize) -> u64 {
    octets.div_ceil(4) as u64
}

/// Faux `claude -p --output-format stream-json`. Enregistre chaque appel dans `$FAKE_DIR/calls/<n>/` puis répond
/// par une sortie structurée selon le type d'appel (déduit du schéma JSON reçu). Pilotage par `FAKE_MODE`
/// (liste séparée par des virgules) et `FAKE_TARGET` (poste visé par les pannes).
const FAKE_SH: &str = r##"#!/bin/sh
D="$FAKE_DIR"
schema=""; sys=""; model=""; resume=""; prompt=""; agents=""; droits=""
while [ $# -gt 0 ]; do
  case "$1" in
    --json-schema) schema="$2"; shift 2 ;;
    --append-system-prompt) sys="$2"; shift 2 ;;
    --model) model="$2"; shift 2 ;;
    --resume) resume="$2"; shift 2 ;;
    --agents) agents="$2"; shift 2 ;;
    # Droits de l'appel : gardés tels quels, une option par ligne, pour les tests de profil.
    --permission-mode|--tools|--allowedTools|--mcp-config|--permission-prompt-tool|--settings) droits="$droits$1 $2
"; shift 2 ;;
    --output-format|--input-format|--agent|--max-budget-usd|--session-id|--setting-sources) shift 2 ;;
    *) shift ;;
  esac
done

# Le prompt arrive sur stdin (`--input-format stream-json`), une ligne NDJSON `{"type":"user",...}`.
# On extrait le champ `text`, puis `printf %b` rend les échappements JSON (\n, \t, \\) ; restent les
# guillemets échappés, que `sed` remet. `printf %b` est POSIX, contrairement à `\n` en remplacement de sed.
esc=$(head -1 | sed -n 's/.*"text":"\(.*\)"}].*/\1/p')
prompt=$(printf %b "$esc" | sed 's/\\"/"/g')

i=1
while ! mkdir "$D/calls/$i" 2>/dev/null; do i=$((i+1)); done
C="$D/calls/$i"
printf %s "$prompt" > "$C/prompt"
printf %s "$sys" > "$C/sys"
printf %s "$schema" > "$C/schema"
printf %s "$agents" > "$C/agents"
printf %s "$model" > "$C/model"
printf %s "$resume" > "$C/resume"
printf %s "$droits" > "$C/droits"

case "$schema" in
  *'"accepted"'*) kind=review ;;
  *'"assignments"'*) kind=delegate ;;
  *) case "$prompt" in
       "Original task your supervisor"*) kind=report ;;
       "Write the plan of the mission"*) kind=plan ;;
       *) kind=work ;;
     esac ;;
esac
printf %s "$kind" > "$C/kind"

who=$(printf %s "$prompt" | sed -n 's/.*\[\[\([a-z0-9-]*\)\]\].*/\1/p' | head -1)
who=${who:-ceo}
mode=",${FAKE_MODE:-nominal},"
target="${FAKE_TARGET:-dev1}"

FILL="Analyse détaillée du sujet : l'architecture proposée sépare clairement les responsabilités, les risques principaux sont identifiés avec leur gravité et une mesure vérifiable est associée à chacun. Les décisions prises sont justifiées, les points ouverts sont listés avec la personne qui doit les trancher, et les critères d'acceptation sont repris tels que formulés dans le brief. "
TASK="Étudier la part de la mission qui vous revient, produire une analyse structurée avec décisions, risques et critères d'acceptation vérifiables, sans chevaucher le travail des autres postes."

emit() {
  printf %s "$1" > "$C/out"
  inb=$(printf %s "$prompt$sys$schema$agents" | wc -c | tr -d ' ')
  outb=$(printf %s "$1" | wc -c | tr -d ' ')
  printf '{"type":"system","subtype":"init","session_id":"fake-%s","model":"fake","apiKeySource":"none"}\n' "$i"
  printf '{"type":"result","subtype":"success","is_error":false,"result":"ok","duration_ms":1,"session_id":"fake-%s","usage":{"input_tokens":%s,"output_tokens":%s,"cache_read_input_tokens":0},"structured_output":%s,"permission_denials":[]}\n' "$i" "$(( (inb+3)/4 ))" "$(( (outb+3)/4 ))" "$1"
  exit 0
}

# Subordonnés directs : lus dans la fiche de poste (« - id · titre »), le schéma de délégation ne les nomme pas.
kids=$(printf '%s\n' "$sys" | sed -n '/^Your direct subordinates/,/^[^-]/s/^- \([a-z0-9-]*\) · .*/\1/p' | tr '\n' ' ')
parent=$(printf %s "$prompt" | sed -n 's/.*Your file scope: `\([^`]*\)`.*/\1/p' | head -1)
dir=${parent%/\*\*}
case "$dir" in ''|'**') dir=src ;; esac

items() {
  out=""; sep=""
  for k in $kids; do
    out="$out$sep{\"to\":\"$k\",\"task\":\"$1 $k [[$k]] : $TASK\",\"context\":[\"src/$k/mod.rs:1-40\"],\"scope\":[\"$dir/$k/**\"]}"
    sep=","
  done
  printf %s "$out"
}

body() {
  b="Rapport de $who : $FILL"
  case "$mode" in *,big,*) n=0; while [ $n -lt 50 ]; do b="$b$FILL"; n=$((n+1)); done ;; esac
  printf %s "$b"
}

case "$kind" in
  delegate) emit "{\"assignments\":[$(items 'Tâche pour')]}" ;;
  review)
    c=0; [ -f "$D/review_n" ] && c=$(cat "$D/review_n"); c=$((c+1)); printf %s "$c" > "$D/review_n"
    case "$mode" in
      *,review_redo,*) [ "$c" -eq 1 ] && emit "{\"accepted\":false,\"findings\":\"il manque un livrable\"}" ;;
      *,review_unmet,*) emit "{\"accepted\":false,\"findings\":\"critère X non atteint\"}" ;;
    esac
    emit '{"accepted":true,"findings":"ok"}' ;;
  plan) emit "{\"status\":\"done\",\"report\":\"Plan de $who : $FILL\",\"blockers\":[]}" ;;
  work)
    if [ "$who" = "$target" ]; then
      case "$mode" in
        *,fail,*)
          printf %s "boum" > "$C/out"
          printf '{"type":"system","subtype":"init","session_id":"fake-%s","model":"fake","apiKeySource":"none"}\n' "$i"
          printf '{"type":"result","is_error":true,"result":"boum","usage":{"input_tokens":1,"output_tokens":1}}\n'
          exit 0 ;;
        *,crash,*) echo "panic: le faux claude plante" >&2; exit 3 ;;
        *,limit,*)
          # Limite de l'abonnement au premier passage : réinitialisation annoncée dans 1 s.
          if [ ! -f "$D/limit_done" ]; then
            : > "$D/limit_done"
            printf '{"type":"system","subtype":"init","session_id":"fake-%s","model":"fake","apiKeySource":"none"}\n' "$i"
            printf '{"type":"result","is_error":true,"result":"Claude AI usage limit reached|%s","usage":{"input_tokens":1,"output_tokens":1}}\n' "$(( $(date +%s) + 1 ))"
            exit 0
          fi ;;
        *,cut,*)
          printf '{"type":"system","subtype":"init","session_id":"fake-%s","model":"fake","apiKeySource":"none"}\n' "$i"
          printf '{"type":"result","is_error":false,"result":"{\\"stat'
          exit 0 ;;
      esac
    fi
    emit "{\"status\":\"done\",\"report\":\"$(body)\",\"blockers\":[]}" ;;
  report) emit "{\"status\":\"done\",\"report\":\"$(body)\",\"blockers\":[]}" ;;
esac
"##;

/// Bac à sable : projet jetable (agents d'exemple créés par `essaim init`) + dossier d'enregistrement du faux `claude`.
pub struct Bac {
    pub racine: PathBuf,
    pub fake_dir: PathBuf,
    pub fake_bin: PathBuf,
}

/// Dossier de configuration d'essaim du bac (`ESSAIM_CONFIG_DIR`) : les tests ne lisent jamais les réglages de la machine.
pub fn config_dir(b: &Bac) -> PathBuf {
    b.racine.parent().unwrap().join("config")
}

/// Fixe le profil de sécurité du bac (`dedicated` ou `protected`).
pub fn profil(b: &Bac, profil: &str) {
    std::fs::create_dir_all(config_dir(b)).unwrap();
    std::fs::write(config_dir(b).join("settings.toml"), format!("lang = \"fr\"\nprofile = \"{profil}\"\n")).unwrap();
}

/// Crée (en le vidant) le bac `nom` sous `CARGO_TARGET_TMPDIR`. Profil de sécurité : « appareil dédié ». Un nom par test ET par processus de test (pid) :
/// deux `cargo test` concurrents sur le même dépôt ne se vident plus mutuellement leurs dossiers d'appels.
pub fn bac(nom: &str) -> Bac {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("essaim-e2e-{nom}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let racine = base.join("projet");
    let fake_dir = base.join("fake");
    std::fs::create_dir_all(&racine).unwrap();
    std::fs::create_dir_all(fake_dir.join("calls")).unwrap();
    std::fs::create_dir_all(base.join("home")).unwrap();
    let fake_bin = base.join("fake-claude.sh");
    std::fs::write(&fake_bin, FAKE_SH).unwrap();
    std::fs::set_permissions(&fake_bin, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755)).unwrap();
    let b = Bac { racine, fake_dir, fake_bin };
    profil(&b, "dedicated");
    let init = Command::new(env!("CARGO_BIN_EXE_essaim")).args(["init", "--root"]).arg(&b.racine).env("HOME", b.racine.parent().unwrap().join("home")).output().unwrap();
    assert!(init.status.success(), "essaim init a échoué : {}", String::from_utf8_lossy(&init.stderr));
    b
}

/// Résultat d'un lancement de `essaim company run`.
pub struct Lancement {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duree: Duration,
}

impl Lancement {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// Écrit l'entreprise `nom` dans le projet et renvoie son chemin.
pub fn ecrire_entreprise(b: &Bac, nom: &str, toml: &str) -> PathBuf {
    let p = b.racine.join(format!(".claude/companies/{nom}.toml"));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, toml).unwrap();
    p
}

/// Lance `essaim company run <entreprise> --brief @fichier --yes` avec le faux `claude` : aucun humain ne répond.
/// `mode` : valeur de `FAKE_MODE` ; `cible` : poste visé par les pannes.
pub fn lancer(b: &Bac, entreprise: &Path, brief: &str, mode: &str, cible: &str) -> Lancement {
    lancer_avec(b, entreprise, brief, mode, cible, None)
}

/// Comme `lancer`, mais sans `--yes` : `reponses` est ce que l'humain tape sur le terminal, une ligne par
/// demande (un numéro d'option, ou une réponse libre).
pub fn lancer_dialogue(b: &Bac, entreprise: &Path, brief: &str, mode: &str, cible: &str, reponses: &str) -> Lancement {
    lancer_avec(b, entreprise, brief, mode, cible, Some(reponses))
}

fn lancer_avec(b: &Bac, entreprise: &Path, brief: &str, mode: &str, cible: &str, reponses: Option<&str>) -> Lancement {
    use std::io::Write;
    let brief_path = b.racine.parent().unwrap().join("brief.md");
    std::fs::write(&brief_path, brief).unwrap();
    let t = Instant::now();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_essaim"));
    cmd.args(["company", "run"]).arg(entreprise).arg("--brief").arg(format!("@{}", brief_path.display()));
    if reponses.is_none() {
        cmd.arg("--yes");
    }
    let mut child = cmd
        .arg("--root")
        .arg(&b.racine)
        .current_dir(&b.racine)
        .env("ESSAIM_CLAUDE_BIN", &b.fake_bin)
        .env("ESSAIM_CONFIG_DIR", config_dir(b))
        .env("FAKE_DIR", &b.fake_dir)
        .env("FAKE_MODE", mode)
        .env("FAKE_TARGET", cible)
        .env("HOME", b.racine.parent().unwrap().join("home"))
        .stdin(if reponses.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(r) = reponses {
        // Tout est écrit d'avance puis l'entrée est fermée : essaim lit une ligne par demande, dans l'ordre.
        child.stdin.take().unwrap().write_all(r.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    Lancement { code: out.status.code(), stdout: String::from_utf8_lossy(&out.stdout).into_owned(), stderr: String::from_utf8_lossy(&out.stderr).into_owned(), duree: t.elapsed() }
}

/// Un appel enregistré par le faux `claude`.
#[derive(Debug, Clone)]
pub struct Appel {
    pub n: usize,
    pub kind: String,
    pub prompt: String,
    pub sys: String,
    pub schema: String,
    pub agents: String,
    pub model: String,
    pub out: String,
    /// Session reprise (`--resume`), vide pour un appel neuf.
    pub resume: String,
    /// Options de droits reçues, une par ligne : `--permission-mode`, `--tools`, `--mcp-config`, `--settings`…
    pub droits: String,
}

impl Appel {
    /// Valeur d'une option de droits (`--tools`, `--permission-mode`…), vide si elle n'a pas été passée.
    pub fn droit(&self, option: &str) -> &str {
        self.droits.lines().find_map(|l| l.strip_prefix(option)?.strip_prefix(' ')).unwrap_or("")
    }

    /// Octets envoyés au modèle : prompt + fiche de poste + schéma + sous-agents.
    pub fn octets_entree(&self) -> usize {
        self.prompt.len() + self.sys.len() + self.schema.len() + self.agents.len()
    }
}

/// Appels enregistrés, dans l'ordre d'arrivée.
pub fn appels(b: &Bac) -> Vec<Appel> {
    let calls = b.fake_dir.join("calls");
    let mut ns: Vec<usize> = std::fs::read_dir(&calls).unwrap().flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect();
    ns.sort_unstable();
    ns.into_iter()
        .map(|n| {
            let lire = |f: &str| std::fs::read_to_string(calls.join(n.to_string()).join(f)).unwrap_or_default();
            Appel { n, kind: lire("kind"), prompt: lire("prompt"), sys: lire("sys"), schema: lire("schema"), agents: lire("agents"), model: lire("model"), out: lire("out"), resume: lire("resume"), droits: lire("droits") }
        })
        .collect()
}

/// Mesure d'une mission, calculée uniquement à partir des appels enregistrés.
#[derive(Debug, Clone, PartialEq)]
pub struct Mesure {
    pub appels: u64,
    pub entree: u64,
    pub sortie: u64,
    /// Jetons d'entrée servis par le cache synthétique : préfixe commun de la fiche de poste avec l'appel précédent.
    pub cache: u64,
    pub par_modele: BTreeMap<String, u64>,
}

impl Mesure {
    pub fn part_cache(&self) -> f64 {
        if self.entree == 0 { 0.0 } else { self.cache as f64 / self.entree as f64 }
    }
}

fn prefixe_commun(a: &str, b: &str) -> usize {
    a.as_bytes().iter().zip(b.as_bytes()).take_while(|(x, y)| x == y).count()
}

/// Jetons d'entrée = ceil(octets/4) ; jetons de sortie = ceil(octets de la sortie structurée/4) ;
/// cache = ceil(préfixe commun de la fiche de poste avec celle de l'appel précédent / 4), borné à l'entrée.
pub fn mesurer(appels: &[Appel]) -> Mesure {
    let mut m = Mesure { appels: appels.len() as u64, entree: 0, sortie: 0, cache: 0, par_modele: BTreeMap::new() };
    let mut prec = "";
    for a in appels {
        let entree = jetons(a.octets_entree());
        m.entree += entree;
        m.sortie += jetons(a.out.len());
        m.cache += jetons(prefixe_commun(prec, &a.sys)).min(entree);
        *m.par_modele.entry(if a.model.is_empty() { "defaut".into() } else { a.model.clone() }).or_insert(0) += 1;
        prec = &a.sys;
    }
    m
}

// ------------------------------------------------------------------ entreprises et briefs types

/// Brief court (M1).
pub const BRIEF_COURT: &str = "Concevoir un module de journalisation structurée pour le service de facturation. Critères : format JSON, niveaux configurables, aucun secret écrit dans les journaux.";

/// Brief détaillé avec critères (M2, M3, M4).
pub const BRIEF_LONG: &str = "Mission : ajouter l'export CSV des factures au service de facturation.\n\nContexte : les équipes comptables exportent aujourd'hui les factures à la main. L'export doit être fiable, sécurisé et simple à utiliser depuis l'interface existante.\n\nCritères de succès :\n1. L'export produit un fichier CSV encodé en UTF-8 avec en-têtes, une ligne par facture.\n2. Aucune donnée personnelle n'est exportée sans le droit correspondant ; les accès sont journalisés.\n3. Les montants conservent deux décimales et le séparateur est configurable.\n4. L'écran d'export affiche la progression et un message d'erreur clair en cas d'échec.\n5. Des tests automatisés couvrent le cas nominal, le fichier vide et l'erreur d'écriture.\n\nContraintes : pas de nouvelle dépendance, pas de changement du format de stockage, livraison en un seul lot.";

/// Entreprise à 3 postes (M1) : CEO → manager → développeur.
pub fn entreprise_petite() -> String {
    "name = \"mini\"\nmax_calls = 40\nmax_parallel = 1\ncheckpoint = false\n\n[[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"boss\"\n\n[[seat]]\nid = \"lead\"\ntitle = \"Tech Lead\"\nagent = \"manager\"\nreports_to = \"ceo\"\nexpertise = \"architecture et livraison\"\n\n[[seat]]\nid = \"dev\"\ntitle = \"Backend Engineer\"\nagent = \"developer\"\nreports_to = \"lead\"\n".into()
}

/// Entreprise à 7 postes (M2, M3, M4). `builds` : dev1 et dev2 réalisent dans le projet (⚒) ; sinon tous analysent.
pub fn entreprise_sept(builds: bool) -> String {
    let b = |_: bool| if builds { "builds = true\n".to_string() } else { String::new() };
    format!(
        "name = \"acme\"\nmax_calls = 80\nmax_parallel = 1\ncheckpoint = false\n\n\
[[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"boss\"\n\n\
[[seat]]\nid = \"cto\"\ntitle = \"CTO\"\nagent = \"vp-tech\"\nreports_to = \"ceo\"\nexpertise = \"technique\"\n\n\
[[seat]]\nid = \"ciso\"\ntitle = \"CISO\"\nagent = \"vp-secu\"\nreports_to = \"ceo\"\n\n\
[[seat]]\nid = \"mgr\"\ntitle = \"Engineering Manager\"\nagent = \"manager\"\nreports_to = \"cto\"\n\n\
[[seat]]\nid = \"dev1\"\ntitle = \"Backend Engineer\"\nagent = \"developer\"\nreports_to = \"mgr\"\n{}\n\
[[seat]]\nid = \"dev2\"\ntitle = \"Frontend Engineer\"\nagent = \"developer\"\nreports_to = \"mgr\"\n{}\n\
[[seat]]\nid = \"sec\"\ntitle = \"Security Analyst\"\nagent = \"analyste-secu\"\nreports_to = \"ciso\"\n",
        b(true),
        b(false)
    )
}

/// Scénario de mesure : nom, entreprise, brief, mode du faux `claude`.
pub struct Scenario {
    pub id: &'static str,
    pub description: &'static str,
    pub entreprise: String,
    pub brief: &'static str,
    pub mode: &'static str,
    /// Réponses de l'humain sur le terminal (`None` : `--yes`, personne ne répond).
    pub reponses: Option<&'static str>,
}

/// Missions types M1 à M4 (max_parallel = 1 partout : ordre des appels déterministe).
pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario { id: "M1", description: "3 postes, brief court", entreprise: entreprise_petite(), brief: BRIEF_COURT, mode: "nominal", reponses: None },
        Scenario { id: "M2", description: "7 postes d'analyse, brief détaillé", entreprise: entreprise_sept(false), brief: BRIEF_LONG, mode: "nominal", reponses: None },
        Scenario { id: "M3", description: "7 postes dont 2 de réalisation", entreprise: entreprise_sept(true), brief: BRIEF_LONG, mode: "nominal", reponses: None },
        // La revue rejette le résultat ; l'humain répond « 1 » (relancer pour corriger) : une descente corrective, puis une revue acceptée.
        Scenario { id: "M4", description: "7 postes, revue rejetée, relance demandée par l'humain, sorties volumineuses (troncature)", entreprise: entreprise_sept(false), brief: BRIEF_LONG, mode: "review_redo,big", reponses: Some("1\n") },
    ]
}
