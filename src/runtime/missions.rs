//! Missions : `<racine>/.essaim/missions/<id>/{brief.md, conception.md, rapport.md}`.

use super::store::write_private;
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Mission {
    pub id: String,
    pub dir: PathBuf,
    pub title: String,
    pub has_conception: bool,
    pub has_report: bool,
    /// Conception menée à terme mais pas de rapport : un run interrompu ou refusé, reprenable à la réalisation.
    pub resumable: bool,
}

pub fn missions_dir(root: &Path) -> PathBuf {
    root.join(".essaim/missions")
}

/// Identifiant triable chronologiquement.
pub fn new_id(now_ms: u64) -> String {
    format!("{now_ms:013}-{}", &uuid::Uuid::new_v4().simple().to_string()[..4])
}

pub fn dir_of(root: &Path, id: &str) -> PathBuf {
    missions_dir(root).join(id)
}

pub fn brief_path(root: &Path, id: &str) -> PathBuf {
    dir_of(root, id).join("brief.md")
}

/// Identifiant de mission sûr pour un nom de dossier : lettres, chiffres, `-` et `_` (ni séparateur, ni `.`).
/// Vérifié avant toute jointure de chemin, y compris pour un `--mission` saisi par l'utilisateur.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Crée le dossier d'une mission (sans brief) et renvoie son chemin.
pub fn create(root: &Path, id: &str) -> Result<PathBuf> {
    anyhow::ensure!(is_valid_id(id), "identifiant de mission invalide");
    let dir = dir_of(root, id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Premier titre ou première ligne non vide du brief, sans marques Markdown.
pub fn title_of(brief: &str) -> String {
    let line = brief.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or("(brief vide)");
    let t: String = line.chars().take(60).collect();
    if line.chars().count() > 60 { format!("{t}…") } else { t }
}

/// Missions ayant un brief, de la plus récente à la plus ancienne.
pub fn list(root: &Path) -> Vec<Mission> {
    let Ok(rd) = std::fs::read_dir(missions_dir(root)) else { return vec![] };
    let mut v: Vec<Mission> = rd
        .flatten()
        .filter(|e| e.path().join("brief.md").is_file())
        .map(|e| {
            let dir = e.path();
            let brief = std::fs::read_to_string(dir.join("brief.md")).unwrap_or_default();
            Mission {
                id: e.file_name().to_string_lossy().into_owned(),
                title: title_of(&brief),
                has_conception: dir.join("conception.md").is_file(),
                has_report: dir.join("rapport.md").is_file(),
                resumable: !dir.join("rapport.md").is_file() && resumable_plan(&dir).is_some(),
                dir,
            }
        })
        .collect();
    v.sort_by(|a, b| b.id.cmp(&a.id));
    v
}

/// Marqueur « conception terminée sans échec ni annulation » : seule une conception marquée peut être reprise.
const DESIGN_DONE: &str = "conception.ok";

pub fn mark_conception_done(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(DESIGN_DONE), "")?;
    Ok(())
}

pub fn clear_conception_mark(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(DESIGN_DONE));
}

/// Plan consolidé d'une conception menée à terme, s'il existe : la mission peut alors reprendre à la réalisation.
pub fn resumable_plan(dir: &Path) -> Option<String> {
    if !dir.join(DESIGN_DONE).is_file() {
        return None;
    }
    std::fs::read_to_string(dir.join("conception.md")).ok().filter(|p| !p.trim().is_empty())
}

pub fn read_brief(m: &Mission) -> Result<String> {
    Ok(std::fs::read_to_string(m.dir.join("brief.md"))?)
}

pub fn write_output(dir: &Path, name: &str, text: &str) -> Result<()> {
    anyhow::ensure!(matches!(name, "conception.md" | "rapport.md" | "brief.md"), "nom de livrable inattendu : {name}");
    std::fs::create_dir_all(dir)?;
    // Livrables = texte du projet et de la mission : réservés à l'utilisateur (0600).
    write_private(&dir.join(name), text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("essaim-mis-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn identifiants_de_mission_sur() {
        assert!(is_valid_id(&new_id(1)));
        for bad in ["../x", "a/b", "", "a.b", "x\\y", "..", "/etc", "a b"] {
            assert!(!is_valid_id(bad), "{bad:?} doit être refusé");
        }
        let root = tmp();
        assert!(create(&root, "../evil").is_err());
        assert!(!root.join("evil").exists());
    }

    #[cfg(unix)]
    #[test]
    fn livrables_en_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = create(&tmp(), &new_id(2)).unwrap();
        write_output(&dir, "conception.md", "plan").unwrap();
        let mode = std::fs::metadata(dir.join("conception.md")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn creation_liste_et_tri() {
        let root = tmp();
        for (ms, brief) in [(1000u64, "# Premier\ncorps"), (3000, "\n\n## Dernier chantier\n"), (2000, "Sans titre markdown")] {
            let id = new_id(ms);
            let dir = create(&root, &id).unwrap();
            write_output(&dir, "brief.md", brief).unwrap();
        }
        create(&root, &new_id(9000)).unwrap(); // sans brief : ignorée
        let l = list(&root);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].title, "Dernier chantier");
        assert_eq!(l[1].title, "Sans titre markdown");
        assert_eq!(l[2].title, "Premier");
        assert!(!l[0].has_conception && !l[0].has_report);
    }

    #[test]
    fn livrables_detectes() {
        let root = tmp();
        let id = new_id(1);
        let dir = create(&root, &id).unwrap();
        write_output(&dir, "brief.md", "# M").unwrap();
        write_output(&dir, "conception.md", "plan").unwrap();
        let m = &list(&root)[0];
        assert!(m.has_conception && !m.has_report);
        write_output(&dir, "rapport.md", "fin").unwrap();
        assert!(list(&root)[0].has_report);
        assert_eq!(read_brief(&list(&root)[0]).unwrap(), "# M");
    }

    #[test]
    fn plan_reprenable_seulement_si_marque() {
        let root = tmp();
        let dir = create(&root, &new_id(1)).unwrap();
        write_output(&dir, "brief.md", "# M").unwrap();
        write_output(&dir, "conception.md", "plan").unwrap();
        assert!(resumable_plan(&dir).is_none() && !list(&root)[0].resumable, "plan non validé : pas reprenable");
        mark_conception_done(&dir).unwrap();
        assert_eq!(resumable_plan(&dir).as_deref(), Some("plan"));
        assert!(list(&root)[0].resumable);
        write_output(&dir, "rapport.md", "fin").unwrap();
        assert!(!list(&root)[0].resumable, "rapport présent : terminée");
        clear_conception_mark(&dir);
        assert!(resumable_plan(&dir).is_none());
    }

    #[test]
    fn garde_fous() {
        let root = tmp();
        assert!(create(&root, "../evil").is_err());
        assert!(create(&root, "").is_err());
        assert!(write_output(&root, "../x.md", "x").is_err());
        assert!(list(&tmp()).is_empty());
    }

    #[test]
    fn titres_longs_tronques() {
        let t = title_of(&"x".repeat(100));
        assert!(t.ends_with('…') && t.chars().count() == 61);
        assert_eq!(title_of("  \n  "), "(brief vide)");
    }
}
