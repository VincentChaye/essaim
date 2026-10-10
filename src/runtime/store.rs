use super::claude_proc::redact_secrets;
use super::executor::NodeStatus;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Trace persistée d'un nœud.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: String,
    pub agent: String,
    /// Libellé lisible et niveau d'indentation (runs d'entreprise) ; vides pour un pipeline.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub depth: usize,
    /// None = jamais terminé (run interrompu).
    pub status: Option<NodeStatus>,
    #[serde(default)]
    pub tokens: u64,
    pub log: Vec<String>,
}

/// Trace persistée d'un run : `<racine>/.essaim/runs/<id>/run.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub pipeline: String,
    pub started_ms: u64,
    /// None = en cours ou interrompu.
    pub finished: Option<bool>,
    #[serde(default)]
    pub tokens: u64,
    pub nodes: Vec<NodeRecord>,
}

impl RunRecord {
    pub fn started_label(&self) -> String {
        use chrono::TimeZone;
        chrono::Local
            .timestamp_millis_opt(self.started_ms as i64)
            .single()
            .map(|t| t.format("%d/%m %H:%M").to_string())
            .unwrap_or_else(|| "?".into())
    }
}

pub fn runs_dir(root: &Path) -> PathBuf {
    root.join(".essaim/runs")
}

/// Identifiant triable chronologiquement.
pub fn new_id(now_ms: u64) -> String {
    format!("{now_ms:013}-{}", &uuid::Uuid::new_v4().simple().to_string()[..4])
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Écrit un fichier réservé à l'utilisateur (0600 sous Unix, existant ou non) : journaux, livrables et
/// traces peuvent contenir du texte du projet.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        opts.mode(0o600);
        let mut f = opts.open(path)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        opts.open(path)?.write_all(bytes)
    }
}

/// Copie d'une trace dont les secrets connus sont masqués (journaux de nœud, statuts d'erreur).
fn redacted(rec: &RunRecord) -> RunRecord {
    let mut r = rec.clone();
    for n in &mut r.nodes {
        for l in &mut n.log {
            *l = redact_secrets(l);
        }
        if let Some(NodeStatus::Failed(t) | NodeStatus::Skipped(t)) = n.status.as_mut() {
            *t = redact_secrets(t);
        }
    }
    r
}

/// Écriture atomique (fichier temporaire puis renommage), en 0600 et sans secret connu.
pub fn save(root: &Path, rec: &RunRecord) -> Result<()> {
    let dir = runs_dir(root).join(&rec.id);
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("run.json.tmp");
    write_private(&tmp, &serde_json::to_vec_pretty(&redacted(rec))?)?;
    std::fs::rename(tmp, dir.join("run.json"))?;
    Ok(())
}

/// Tous les runs lisibles, du plus récent au plus ancien.
pub fn load_all(root: &Path) -> Vec<RunRecord> {
    let Ok(rd) = std::fs::read_dir(runs_dir(root)) else { return vec![] };
    let mut v: Vec<RunRecord> = rd
        .flatten()
        .filter_map(|e| std::fs::read(e.path().join("run.json")).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect();
    v.sort_by(|a, b| b.id.cmp(&a.id));
    v
}

pub fn delete(root: &Path, id: &str) -> Result<()> {
    // Vide = le dossier `runs` entier : refusé.
    anyhow::ensure!(!id.is_empty() && !id.contains(['/', '\\', '.']), "identifiant de run invalide");
    std::fs::remove_dir_all(runs_dir(root).join(id))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, ok: Option<bool>) -> RunRecord {
        RunRecord {
            id: id.into(),
            pipeline: "p".into(),
            started_ms: 1_791_460_000_000,
            finished: ok,
            tokens: 0,
            nodes: vec![NodeRecord { id: "a".into(), agent: "x".into(), label: String::new(), depth: 0, status: Some(NodeStatus::Failed("boum".into())), tokens: 0, log: vec!["l1".into()] }],
        }
    }

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("essaim-store-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn aller_retour_et_tri() {
        let root = tmp();
        save(&root, &rec(&new_id(1000), Some(true))).unwrap();
        save(&root, &rec(&new_id(3000), None)).unwrap();
        save(&root, &rec(&new_id(2000), Some(false))).unwrap();
        let all = load_all(&root);
        assert_eq!(all.len(), 3);
        assert!(all[0].id.starts_with("0000000003000"), "le plus récent d'abord");
        assert_eq!(all[2].nodes[0].status, Some(NodeStatus::Failed("boum".into())));
        assert!(!root.join(".essaim/runs").join(&all[0].id).join("run.json.tmp").exists());
    }

    #[test]
    fn ignore_les_fichiers_corrompus() {
        let root = tmp();
        save(&root, &rec(&new_id(1), Some(true))).unwrap();
        let bad = runs_dir(&root).join("casse");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("run.json"), "{pas du json").unwrap();
        assert_eq!(load_all(&root).len(), 1);
        assert!(load_all(&tmp()).is_empty());
    }

    #[test]
    fn suppression_et_garde_chemin() {
        let root = tmp();
        let r = rec(&new_id(5), Some(true));
        save(&root, &r).unwrap();
        assert!(delete(&root, "../x").is_err());
        assert!(delete(&root, "").is_err(), "un id vide ne doit pas effacer tout le dossier des runs");
        assert_eq!(load_all(&root).len(), 1);
        delete(&root, &r.id).unwrap();
        assert!(load_all(&root).is_empty());
    }

    #[test]
    fn libelle_de_date() {
        assert_ne!(rec("a", None).started_label(), "?");
    }

    #[test]
    fn trace_sans_secret_et_en_0600() {
        let root = tmp();
        let mut r = rec(&new_id(9), Some(false));
        r.nodes[0].log = vec!["✖ clé sk-ant-api03-SECRETVALUE refusée".into()];
        r.nodes[0].status = Some(NodeStatus::Failed("ANTHROPIC_API_KEY=SECRETVALUE2".into()));
        save(&root, &r).unwrap();
        let path = runs_dir(&root).join(&r.id).join("run.json");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("SECRETVALUE"), "secret persisté");
        let back = load_all(&root);
        assert!(back[0].nodes[0].log[0].contains("refusée"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
