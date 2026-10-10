//! Helpers d'E/S fichiers communs (F4). HYPOTHÈSE : contrat dérivé des groupes de duplication mesurés.
//!
//! Ajout seulement : aucun site d'appel existant n'est modifié ici (adoption à l'étape 4).

use std::io;
use std::path::Path;

/// Contenu du fichier s'il existe et n'est pas vide ni composé d'espaces seulement.
/// Équivalent à `read_to_string(p).ok().filter(|t| !t.trim().is_empty())`.
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn read_nonblank(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().filter(|t| !t.trim().is_empty())
}

/// Contenu du fichier, ou chaîne vide si la lecture échoue.
/// Équivalent à `read_to_string(p).unwrap_or_default()`.
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn read_or_default(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Écrit `contents` dans `path` après avoir créé les dossiers parents manquants.
/// Sans panique si `path` n'a pas de parent non vide (nom seul).
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn write_creating_parents(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("essaim-fsutil-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn read_nonblank_filtre_absent_et_blanc() {
        let d = scratch();
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.md");
        assert_eq!(read_nonblank(&f), None, "fichier absent");
        std::fs::write(&f, "  \n\t ").unwrap();
        assert_eq!(read_nonblank(&f), None, "contenu blanc");
        std::fs::write(&f, "  texte  ").unwrap();
        assert_eq!(read_nonblank(&f).as_deref(), Some("  texte  "), "contenu conservé tel quel");
    }

    #[test]
    fn read_or_default_absent_donne_vide() {
        let d = scratch();
        assert_eq!(read_or_default(&d.join("absent.md")), "");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ok.md"), "contenu").unwrap();
        assert_eq!(read_or_default(&d.join("ok.md")), "contenu");
    }

    #[test]
    fn write_creating_parents_cree_les_dossiers_et_remplace() {
        let f = scratch().join("x/y/z.md");
        write_creating_parents(&f, "premier").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "premier");
        write_creating_parents(&f, b"second".as_slice()).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "second");
    }
}
