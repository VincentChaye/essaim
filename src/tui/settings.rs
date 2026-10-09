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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    pub lang: Option<Lang>,
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
