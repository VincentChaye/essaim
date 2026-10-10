//! Identifiants, chemins sûrs et écriture atomique partagés par les opérations CRUD (agents, entreprises, runs).

use std::fmt;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

/// Longueur maximale d'un identifiant (règle D2).
pub const ID_MAX_LEN: usize = 64;

/// Noms réservés (périphériques Windows) refusés comme identifiant.
const NOMS_RESERVES: &[&str] = &[
    "con", "nul", "aux", "prn", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7",
    "lpt8", "lpt9",
];

/// Erreur commune des opérations CRUD. Chaque message est en français et porte l'identifiant ou le chemin concerné.
#[derive(Debug)]
pub enum CrudError {
    /// L'élément demandé n'existe pas.
    Introuvable(String),
    /// Entrée utilisateur ou donnée non valide (identifiant, chemin, contenu).
    Invalide(String),
    /// Un élément du même nom existe déjà.
    Existe(String),
    /// Opération refusée par une règle métier (référence active, lien symbolique, boss, etc.).
    Refuse(String),
    /// Erreur d'entrée/sortie sur un chemin donné.
    Io { chemin: String, source: io::Error },
}

impl CrudError {
    /// Construit une erreur d'E/S rattachée à un chemin.
    pub fn io(chemin: &Path, source: io::Error) -> Self {
        CrudError::Io {
            chemin: chemin.display().to_string(),
            source,
        }
    }
}

impl fmt::Display for CrudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrudError::Introuvable(s) => write!(f, "introuvable : {s}"),
            CrudError::Invalide(s) => write!(f, "{s}"),
            CrudError::Existe(s) => write!(f, "{s} existe déjà"),
            CrudError::Refuse(s) => write!(f, "opération refusée : {s}"),
            CrudError::Io { chemin, source } => write!(f, "erreur d'E/S sur {chemin} : {source}"),
        }
    }
}

impl std::error::Error for CrudError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CrudError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Valide un identifiant (règle D2) : `[a-z0-9_-]`, 1 à 64 caractères, sans tiret ni point initial, hors noms réservés.
pub fn validate_id(id: &str) -> Result<(), CrudError> {
    let invalide = |raison: &str| CrudError::Invalide(format!("identifiant « {id} » invalide : {raison}"));
    if id.is_empty() {
        return Err(CrudError::Invalide("identifiant vide".to_string()));
    }
    if id.len() > ID_MAX_LEN {
        return Err(invalide(&format!("plus de {ID_MAX_LEN} caractères")));
    }
    if id.starts_with('-') {
        return Err(invalide("ne doit pas commencer par un tiret"));
    }
    if id.starts_with('.') {
        return Err(invalide("ne doit pas commencer par un point"));
    }
    if let Some(c) = id.chars().find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_')) {
        return Err(invalide(&format!("caractère « {c} » interdit (autorisés : a-z, 0-9, _ et -)")));
    }
    if NOMS_RESERVES.contains(&id) {
        return Err(invalide("nom réservé"));
    }
    Ok(())
}

/// Indique si un nom existant ne respecte pas la règle D2. Ces noms restent lisibles mais doivent être signalés.
pub fn nom_herite(id: &str) -> bool {
    validate_id(id).is_err()
}

/// Vérifie qu'un identifiant n'est pas déjà pris, sans tenir compte de la casse.
pub fn verifier_unicite<'a>(id: &str, existants: impl IntoIterator<Item = &'a str>) -> Result<(), CrudError> {
    let cle = id.to_lowercase();
    if existants.into_iter().any(|e| e.to_lowercase() == cle) {
        return Err(CrudError::Existe(format!("l'identifiant « {id} »")));
    }
    Ok(())
}

/// Joint un chemin relatif à `base` en refusant tout chemin vide, absolu, ou contenant `..`, `.` ou un préfixe.
pub fn safe_join(base: &Path, relatif: &str) -> Result<PathBuf, CrudError> {
    if relatif.is_empty() {
        return Err(CrudError::Invalide("chemin vide".to_string()));
    }
    let rel = Path::new(relatif);
    if rel.is_absolute() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(CrudError::Invalide(format!("chemin « {relatif} » non autorisé (chemin absolu ou traversée de dossier)")));
    }
    Ok(base.join(rel))
}

/// Refuse un chemin qui est un lien symbolique. Un chemin absent est accepté.
pub fn verifier_pas_lien(chemin: &Path) -> Result<(), CrudError> {
    match std::fs::symlink_metadata(chemin) {
        Ok(m) if m.file_type().is_symlink() => Err(CrudError::Refuse(format!("« {} » est un lien symbolique", chemin.display()))),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CrudError::io(chemin, e)),
    }
}

/// Écrit `contenu` dans `chemin` de façon atomique : fichier temporaire du même dossier, `sync_all`, puis `rename`.
/// Refuse une cible qui est un lien symbolique.
pub fn write_atomic(chemin: &Path, contenu: &str) -> Result<(), CrudError> {
    verifier_pas_lien(chemin)?;
    let nom = chemin
        .file_name()
        .ok_or_else(|| CrudError::Invalide(format!("chemin sans nom de fichier : {}", chemin.display())))?;
    let dossier = match chemin.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let tmp = dossier.join(format!(".{}.{}.tmp", nom.to_string_lossy(), uuid::Uuid::new_v4()));
    let resultat = (|| -> io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(contenu.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, chemin)
    })();
    if let Err(e) = resultat {
        // Nettoyage best effort du temporaire : l'erreur d'origine est celle remontée.
        std::fs::remove_file(&tmp).ok();
        return Err(CrudError::io(chemin, e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dossier_temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("essaim-ident-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn id_valides_acceptes() {
        let longueur_max = "a".repeat(ID_MAX_LEN);
        for id in ["a", "agent-1", "mon_agent", "x9", longueur_max.as_str()] {
            assert!(validate_id(id).is_ok(), "accepté : {id}");
        }
    }

    #[test]
    fn id_invalides_refuses() {
        assert!(validate_id("").is_err(), "vide");
        assert!(validate_id(&"a".repeat(ID_MAX_LEN + 1)).is_err(), "trop long");
        assert!(validate_id("-abc").is_err(), "tiret initial");
        assert!(validate_id(".caché").is_err(), "point initial");
        assert!(validate_id("Maj").is_err(), "majuscule");
        assert!(validate_id("a b").is_err(), "espace");
        assert!(validate_id("a/b").is_err(), "séparateur");
        assert!(validate_id("con").is_err(), "nom réservé");
        assert!(validate_id("lpt1").is_err(), "nom réservé");
    }

    #[test]
    fn nom_herite_signale_sans_bloquer_la_lecture() {
        assert!(nom_herite("Ancien Nom"));
        assert!(!nom_herite("nouveau-nom"));
    }

    #[test]
    fn unicite_insensible_a_la_casse() {
        assert!(verifier_unicite("Dev", ["dev", "autre"]).is_err());
        assert!(verifier_unicite("neuf", ["dev", "autre"]).is_ok());
    }

    #[test]
    fn safe_join_refuse_traversee_et_absolu() {
        let base = Path::new("/base");
        assert!(safe_join(base, "../etc").is_err(), "traversée");
        assert!(safe_join(base, "a/../../b").is_err(), "traversée imbriquée");
        assert!(safe_join(base, "/etc/passwd").is_err(), "absolu");
        assert!(safe_join(base, "").is_err(), "vide");
        assert!(safe_join(base, "./a").is_err(), "point courant");
        assert_eq!(safe_join(base, "agent.md").unwrap(), base.join("agent.md"));
    }

    #[test]
    fn write_atomic_ecrit_et_remplace() {
        let d = dossier_temp();
        let f = d.join("donnee.toml");
        write_atomic(&f, "a = 1\n").unwrap();
        write_atomic(&f, "a = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a = 2\n");
        let restes: Vec<_> = std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert_eq!(restes.len(), 1, "aucun temporaire résiduel");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_refuse_lien_symbolique() {
        let d = dossier_temp();
        let cible = d.join("cible.txt");
        std::fs::write(&cible, "intact").unwrap();
        let lien = d.join("lien.txt");
        std::os::unix::fs::symlink(&cible, &lien).unwrap();
        assert!(write_atomic(&lien, "piégé").is_err(), "lien refusé");
        assert_eq!(std::fs::read_to_string(&cible).unwrap(), "intact");
        let _ = std::fs::remove_dir_all(&d);
    }
}
