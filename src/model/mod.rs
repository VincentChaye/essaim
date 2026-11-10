use std::io::Read;
use std::path::{Path, PathBuf};

pub mod agent;
pub mod company;
pub mod ident;
pub mod roles;

/// Dossier `.claude` de l'utilisateur (`~/.claude`), s'il est localisable.
pub fn user_claude_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().join(".claude"))
}

/// Où essaim lit et écrit agents et entreprises : le projet courant, et le dossier `.claude` de
/// l'utilisateur. Ce dernier suit l'utilisateur partout : ce qu'on y crée reste disponible depuis
/// n'importe quel dossier du système de fichiers, donc toujours dans l'outil.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigDirs {
    /// Racine du projet courant : à nom de fichier égal, `<projet>/.claude/…` a le dernier mot.
    pub project: PathBuf,
    /// Dossier `.claude` de l'utilisateur ; `None` = introuvable (tout reste alors dans le projet).
    pub user: Option<PathBuf>,
}

impl ConfigDirs {
    /// Projet + dossier de l'utilisateur : la configuration complète, telle que l'outil la voit.
    pub fn new(project: impl Into<PathBuf>) -> Self {
        Self { project: project.into(), user: user_claude_dir() }
    }

    /// Projet seul : pour les tests, qui ne doivent pas dépendre du vrai `~/.claude`. En vrai, un
    /// dossier personnel introuvable donne déjà le même résultat (`user: None`).
    #[cfg(test)]
    pub fn project_only(project: impl Into<PathBuf>) -> Self {
        Self { project: project.into(), user: None }
    }

    /// Dossiers à lire, par priorité décroissante : le projet, puis l'utilisateur.
    fn read(&self, sub: &str) -> Vec<PathBuf> {
        let mut v = vec![self.project.join(".claude").join(sub)];
        v.extend(self.user.as_ref().map(|u| u.join(sub)));
        v
    }

    /// Dossier où créer du neuf : celui de l'utilisateur dès qu'il est connu (création globale par défaut).
    fn write(&self, sub: &str) -> PathBuf {
        self.user.clone().unwrap_or_else(|| self.project.join(".claude")).join(sub)
    }

    pub fn agents(&self) -> Vec<PathBuf> {
        self.read("agents")
    }

    pub fn agents_write(&self) -> PathBuf {
        self.write("agents")
    }

    pub fn companies(&self) -> Vec<PathBuf> {
        self.read("companies")
    }

    pub fn companies_write(&self) -> PathBuf {
        self.write("companies")
    }

    /// Vrai pour un fichier du dossier de l'utilisateur : il est global, visible depuis tous les dossiers.
    pub fn is_global(&self, path: &Path) -> bool {
        self.user.as_ref().is_some_and(|u| path.starts_with(u))
    }
}

/// Chemin raccourci pour l'affichage : le dossier personnel devient `~`.
pub fn display_short(path: &Path) -> String {
    let home = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf());
    match home.and_then(|h| path.strip_prefix(h).ok().map(|r| format!("~/{}", r.display()))) {
        Some(s) => s,
        None => path.display().to_string(),
    }
}

/// Taille maximale d'un fichier de configuration du projet lu (agent, entreprise) : 1 Mio.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// Lit un fichier de configuration du projet, borné à `MAX_CONFIG_BYTES` : un fichier démesuré (YAML, TOML)
/// ne doit pas saturer la mémoire. La borne s'applique à la lecture elle-même, pas seulement à la taille annoncée.
pub fn read_config(path: &Path) -> std::io::Result<String> {
    let mut s = String::new();
    std::fs::File::open(path)?.take(MAX_CONFIG_BYTES + 1).read_to_string(&mut s)?;
    if s.len() as u64 > MAX_CONFIG_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("fichier trop volumineux (plus de 1 Mio) : {}", path.display())));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dossiers_projet_puis_utilisateur_et_creation_globale() {
        let d = ConfigDirs { project: PathBuf::from("/p"), user: Some(PathBuf::from("/u/.claude")) };
        assert_eq!(d.agents(), vec![PathBuf::from("/p/.claude/agents"), PathBuf::from("/u/.claude/agents")]);
        assert_eq!(d.companies(), vec![PathBuf::from("/p/.claude/companies"), PathBuf::from("/u/.claude/companies")]);
        // Le neuf est écrit côté utilisateur : disponible depuis tout le système de fichiers.
        assert_eq!(d.agents_write(), PathBuf::from("/u/.claude/agents"));
        assert_eq!(d.companies_write(), PathBuf::from("/u/.claude/companies"));
        assert!(d.is_global(Path::new("/u/.claude/agents/x.md")) && !d.is_global(Path::new("/p/.claude/agents/x.md")));
        assert!(!ConfigDirs::project_only("/p").is_global(Path::new("/p/.claude/agents/x.md")), "sans dossier utilisateur, rien n'est global");
        // Sans dossier utilisateur : tout reste dans le projet.
        let p = ConfigDirs::project_only("/p");
        assert_eq!(p.agents(), vec![PathBuf::from("/p/.claude/agents")]);
        assert_eq!(p.agents_write(), PathBuf::from("/p/.claude/agents"));
    }

    #[test]
    fn config_bornee_a_1_mio() {
        let p = std::env::temp_dir().join(format!("essaim-cfg-{}.toml", uuid::Uuid::new_v4()));
        std::fs::write(&p, "x".repeat(MAX_CONFIG_BYTES as usize + 10)).unwrap();
        assert!(read_config(&p).is_err(), "au-delà de 1 Mio : refusé");
        std::fs::write(&p, "name = \"ok\"").unwrap();
        assert_eq!(read_config(&p).unwrap(), "name = \"ok\"");
        let _ = std::fs::remove_file(&p);
    }
}
