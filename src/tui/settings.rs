//! Réglages mémorisés entre deux lancements : `<config>/essaim/settings.toml`
//! (`ESSAIM_CONFIG_DIR` remplace le dossier, pour les tests).

use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::path::PathBuf;

thread_local! {
    static CURRENT: Cell<Lang> = const { Cell::new(Lang::Fr) };
}

/// Langue courante du fil de l'interface (posée par l'`App` avant de dessiner ou de traiter une touche).
pub fn set_current(l: Lang) {
    CURRENT.with(|c| c.set(l));
}

pub fn current() -> Lang {
    CURRENT.with(|c| c.get())
}

/// Texte fixe selon la langue courante.
pub fn tr(fr: &'static str, en: &'static str) -> &'static str {
    current().t(fr, en)
}

/// `format!` selon la langue courante : `tf!("fr {x}", "en {x}", x = 1)`.
#[macro_export]
macro_rules! tf {
    ($fr:literal, $en:literal $(, $($arg:tt)*)?) => {
        if $crate::tui::settings::current() == $crate::tui::settings::Lang::En { format!($en $(, $($arg)*)?) } else { format!($fr $(, $($arg)*)?) }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Fr,
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Fr, Lang::En];

    pub fn name(self) -> &'static str {
        match self {
            Lang::Fr => "Français",
            Lang::En => "English",
        }
    }

    /// Choisit entre deux textes selon la langue.
    pub fn t(self, fr: &'static str, en: &'static str) -> &'static str {
        match self {
            Lang::Fr => fr,
            Lang::En => en,
        }
    }
}

/// Profil de sécurité, choisi à l'installation : ce que les agents ont le droit de faire sur la machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// Appareil dédié, sans données personnelles : tous les droits, aucune demande, aucun bac à sable.
    Dedicated,
    /// Machine personnelle : les commandes tournent dans le bac à sable de Claude Code, et tout ce qui
    /// en sort (réseau, fichiers hors du projet) devient une demande à l'humain.
    #[default]
    Protected,
}

impl Profile {
    pub const ALL: [Profile; 2] = [Profile::Dedicated, Profile::Protected];

    pub fn key(self) -> &'static str {
        match self {
            Profile::Dedicated => "dedicated",
            Profile::Protected => "protected",
        }
    }

    pub fn parse(s: &str) -> Option<Profile> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dedicated" | "dedie" | "dédié" | "1" => Some(Profile::Dedicated),
            "protected" | "protege" | "protégé" | "2" => Some(Profile::Protected),
            _ => None,
        }
    }

    pub fn name(self, lang: Lang) -> &'static str {
        match self {
            Profile::Dedicated => lang.t("Appareil dédié", "Dedicated machine"),
            Profile::Protected => lang.t("Protégé", "Protected"),
        }
    }

    /// Explication affichée au moment du choix (installeur, premier lancement, F10).
    pub fn explain(self, lang: Lang) -> &'static str {
        match self {
            Profile::Dedicated => lang.t(
                "Pour une machine SANS données personnelles (machine virtuelle, poste réservé aux agents). Les agents ont tous les droits : toute commande, tout fichier, le réseau, sans jamais demander. Rien n'empêche un agent d'agir hors du dossier du projet. Les agents ne vous interrompent que pour une question ou une action qu'eux seuls ne peuvent pas faire.",
                "For a machine WITHOUT personal data (virtual machine, box reserved for agents). Agents have every right: any command, any file, the network, never asking. Nothing stops an agent from acting outside the project folder. Agents only interrupt you for a question or an action they cannot do themselves.",
            ),
            Profile::Protected => lang.t(
                "Pour votre machine personnelle. Les commandes des agents tournent dans le bac à sable de Claude Code, tenu par le système : écriture limitée au dossier du projet, pas de lecture de votre dossier personnel, réseau limité aux registres de paquets. Dans le projet, les agents travaillent librement. Tout ce qui sort de ce cadre (commande hors bac à sable, site web, fichier hors du projet) vous est demandé : vous recevez donc plus de demandes. Limite : le bac à sable couvre les commandes ; les fichiers et le web passent par vos réponses.",
                "For your personal machine. Agent commands run in Claude Code's sandbox, enforced by the system: writes limited to the project folder, no reading of your home folder, network limited to package registries. Inside the project, agents work freely. Anything beyond that (a command outside the sandbox, a web site, a file outside the project) is asked to you: so you get more requests. Limit: the sandbox covers commands; files and the web go through your answers.",
            ),
        }
    }
}

/// Dossiers d'outillage lisibles depuis le bac à sable (profil protégé) quand `sandbox_read` n'est pas réglé :
/// sans eux, `cargo`, `node` ou `python` installés dans le dossier personnel ne démarrent pas
/// (mesuré sur `claude` 2.1.322 : « operation not permitted: cargo »).
pub const DEFAULT_SANDBOX_READ: &[&str] = &["~/.cargo", "~/.rustup", "~/.nvm", "~/.npm", "~/.volta", "~/.bun", "~/.deno", "~/.pyenv", "~/.local/bin", "~/.local/share/uv", "~/go"];
/// Caches de paquets inscriptibles depuis le bac à sable.
pub const DEFAULT_SANDBOX_WRITE: &[&str] = &["~/.cargo/registry", "~/.cargo/git", "~/.npm/_cacache"];
/// Registres de paquets joignables depuis le bac à sable (tout autre domaine est demandé à l'humain).
pub const DEFAULT_SANDBOX_DOMAINS: &[&str] = &["crates.io", "*.crates.io", "registry.npmjs.org", "pypi.org", "files.pythonhosted.org"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    pub lang: Option<Lang>,
    /// Profil de sécurité ; absent = jamais choisi (la question est posée, et `protected` s'applique d'ici là).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    /// Profil protégé : dossiers hors projet lisibles par les commandes (absent = `DEFAULT_SANDBOX_READ`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_read: Option<Vec<String>>,
    /// Profil protégé : dossiers hors projet inscriptibles par les commandes (absent = `DEFAULT_SANDBOX_WRITE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_write: Option<Vec<String>>,
    /// Profil protégé : domaines joignables sans demande (absent = `DEFAULT_SANDBOX_DOMAINS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_domains: Option<Vec<String>>,
}

/// Droits effectifs des agents d'un run : le profil et, pour le profil protégé, les ouvertures du bac à sable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Access {
    pub profile: Profile,
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub domains: Vec<String>,
}

impl Access {
    /// Tous les droits, sans bac à sable.
    pub fn dedicated() -> Self {
        Access { profile: Profile::Dedicated, ..Default::default() }
    }

    /// Réglages passés à `claude --settings` : le bac à sable du profil protégé ; rien pour le profil dédié.
    /// `failIfUnavailable` : jamais de repli silencieux hors bac à sable.
    pub fn claude_settings(&self) -> Option<String> {
        (self.profile == Profile::Protected).then(|| {
            serde_json::json!({
                "sandbox": {
                    "enabled": true,
                    "failIfUnavailable": true,
                    "autoAllowBashIfSandboxed": true,
                    "filesystem": {"allowRead": self.read, "allowWrite": self.write},
                    "network": {"allowedDomains": self.domains},
                },
                "permissions": {"blockReadsOutsideWorkingDirectories": true},
            })
            .to_string()
        })
    }
}

impl Settings {
    /// Droits effectifs : profil choisi (`protected` tant qu'aucun choix n'est fait) et ouvertures du bac à sable.
    pub fn access(&self) -> Access {
        let list = |v: &Option<Vec<String>>, d: &[&str]| v.clone().unwrap_or_else(|| d.iter().map(|s| s.to_string()).collect());
        Access {
            profile: self.profile.unwrap_or_default(),
            read: list(&self.sandbox_read, DEFAULT_SANDBOX_READ),
            write: list(&self.sandbox_write, DEFAULT_SANDBOX_WRITE),
            domains: list(&self.sandbox_domains, DEFAULT_SANDBOX_DOMAINS),
        }
    }
}

/// Réglages enregistrés (vides si le fichier manque ou ne se lit pas).
pub fn load_default() -> Settings {
    path().map(|p| load(&p)).unwrap_or_default()
}

pub fn path() -> Option<PathBuf> {
    let dir = match std::env::var_os("ESSAIM_CONFIG_DIR") {
        Some(d) => PathBuf::from(d),
        None => directories::ProjectDirs::from("", "", "essaim")?.config_dir().to_path_buf(),
    };
    Some(dir.join("settings.toml"))
}

pub fn load(path: &PathBuf) -> Settings {
    std::fs::read_to_string(path).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(path: &PathBuf, s: &Settings) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, toml::to_string_pretty(s)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profil_absent_vaut_protege_et_se_relit() {
        let s: Settings = toml::from_str("lang = \"fr\"\n").unwrap();
        assert_eq!(s.profile, None, "un ancien settings.toml reste lisible");
        assert_eq!(s.access().profile, Profile::Protected, "jamais choisi = protégé");
        let s: Settings = toml::from_str("profile = \"dedicated\"\n").unwrap();
        assert_eq!(s.access().profile, Profile::Dedicated);
        assert_eq!(Profile::parse("Protégé"), Some(Profile::Protected));
        assert_eq!(Profile::parse("1"), Some(Profile::Dedicated));
        assert_eq!(Profile::parse("x"), None);
    }

    #[test]
    fn bac_a_sable_seulement_en_profil_protege() {
        assert!(Access::dedicated().claude_settings().is_none(), "profil dédié : aucun réglage de bac à sable");
        let a = Settings { sandbox_read: Some(vec!["~/outils".into()]), ..Default::default() }.access();
        let v: serde_json::Value = serde_json::from_str(&a.claude_settings().unwrap()).unwrap();
        assert_eq!(v["sandbox"]["enabled"], true);
        assert_eq!(v["sandbox"]["failIfUnavailable"], true, "jamais de repli silencieux hors bac à sable");
        assert_eq!(v["sandbox"]["filesystem"]["allowRead"], serde_json::json!(["~/outils"]), "liste de l'utilisateur prioritaire");
        assert_eq!(v["permissions"]["blockReadsOutsideWorkingDirectories"], true);
        assert!(v["sandbox"]["network"]["allowedDomains"].as_array().unwrap().iter().any(|d| d == "crates.io"));
    }
}
