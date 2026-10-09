use crate::tf;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Définition d'un agent, stockée dans un fichier `.claude/agents/<name>.md`
/// (frontmatter YAML + corps = prompt système).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub prompt: String,
}

#[derive(Serialize, Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// Format Claude Code : liste séparée par des virgules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tools: Option<String>,
}

impl AgentDef {
    pub fn parse(src: &str) -> Result<Self> {
        let rest = src
            .strip_prefix("---\n")
            .with_context(|| tf!("frontmatter manquant (le fichier doit commencer par ---)", "missing frontmatter (the file must start with ---)"))?;
        let (yaml, body) = rest
            .split_once("\n---")
            .with_context(|| tf!("frontmatter non terminé (--- de fermeture introuvable)", "unterminated frontmatter (closing --- not found)"))?;
        let body = body.strip_prefix('\n').unwrap_or(body);
        let fm: Frontmatter = serde_norway::from_str(yaml).with_context(|| tf!("frontmatter YAML invalide", "invalid YAML frontmatter"))?;
        let tools = fm.tools.map(|t| {
            t.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        });
        Ok(Self {
            name: fm.name,
            description: fm.description,
            model: fm.model,
            tools,
            prompt: body.trim_matches('\n').to_string(),
        })
    }

    pub fn render(&self) -> Result<String> {
        let fm = Frontmatter {
            name: self.name.clone(),
            description: self.description.clone(),
            model: self.model.clone(),
            tools: self.tools.as_ref().map(|t| t.join(", ")),
        };
        let yaml = serde_norway::to_string(&fm)?;
        Ok(format!("---\n{}---\n\n{}\n", yaml, self.prompt))
    }

    /// Entrée JSON pour `claude --agents '{ "<name>": {...} }'`.
    pub fn to_cli_json(&self) -> serde_json::Value {
        let mut o = serde_json::json!({
            "description": self.description,
            "prompt": self.prompt,
        });
        if let Some(m) = &self.model {
            o["model"] = m.clone().into();
        }
        if let Some(t) = &self.tools {
            o["tools"] = t.clone().into();
        }
        o
    }
}

/// Dossiers d'agents : projet (`<racine>/.claude/agents`) puis utilisateur (`~/.claude/agents`).
pub fn agent_dirs(project_root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![project_root.join(".claude/agents")];
    if let Some(home) = directories::UserDirs::new() {
        dirs.push(home.home_dir().join(".claude/agents"));
    }
    dirs
}

/// Charge tous les agents ; le projet prend le pas sur l'utilisateur en cas de doublon.
pub fn load_all(project_root: &Path) -> Vec<AgentDef> {
    let mut out: Vec<AgentDef> = Vec::new();
    for dir in agent_dirs(project_root) {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for p in files {
            if p.extension().is_some_and(|e| e == "md") {
                if let Ok(src) = super::read_config(&p) {
                    if let Ok(a) = AgentDef::parse(&src) {
                        // Le nom vient du frontmatter (fichier du projet) : même règle qu'à l'écriture, il devient un argument CLI.
                        if is_valid_name(&a.name) && !out.iter().any(|x| x.name == a.name) {
                            out.push(a);
                        }
                    }
                }
            }
        }
    }
    out
}

/// Nom d'agent sûr pour un nom de fichier : lettres, chiffres, `-` et `_` uniquement
/// (donc ni séparateur de chemin, ni `..`, ni point, ni vide).
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn save(project_root: &Path, agent: &AgentDef) -> Result<PathBuf> {
    if !is_valid_name(&agent.name) {
        bail!("{}", tf!("nom d'agent invalide : {:?} (lettres, chiffres, - et _ uniquement)", "invalid agent name: {:?} (letters, digits, - and _ only)", agent.name));
    }
    let dir = project_root.join(".claude/agents");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", agent.name));
    std::fs::write(&path, agent.render()?)?;
    Ok(path)
}

/// Fichiers `.claude/agents/*.md` du projet qui ne se lisent pas (nom du fichier, erreur).
pub fn invalid_files(project_root: &Path) -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir(project_root.join(".claude/agents")) else { return vec![] };
    let mut v: Vec<(String, String)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .filter_map(|p| {
            let src = super::read_config(&p).ok()?;
            let err = AgentDef::parse(&src).err()?;
            Some((p.file_name()?.to_string_lossy().into_owned(), format!("{err:#}")))
        })
        .collect();
    v.sort();
    v
}

/// Supprime le fichier d'un agent du projet. Renvoie `false` s'il n'existe pas côté projet.
pub fn delete(project_root: &Path, name: &str) -> Result<bool> {
    // Le nom vient aussi du frontmatter d'un fichier (non validé au chargement) : jamais de chemin construit tel quel.
    if !is_valid_name(name) {
        bail!("{}", tf!("nom d'agent invalide : {name:?} (lettres, chiffres, - et _ uniquement)", "invalid agent name: {name:?} (letters, digits, - and _ only)"));
    }
    let path = project_root.join(".claude/agents").join(format!("{name}.md"));
    if path.exists() {
        std::fs::remove_file(path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let a = AgentDef {
            name: "code-reviewer".into(),
            description: "Relit le code".into(),
            model: Some("sonnet".into()),
            tools: Some(vec!["Read".into(), "Grep".into()]),
            prompt: "Tu es un relecteur.\n\nSois bref.".into(),
        };
        let b = AgentDef::parse(&a.render().unwrap()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parse_sans_options() {
        let a = AgentDef::parse("---\nname: x\ndescription: d\n---\nBonjour\n").unwrap();
        assert_eq!(a.model, None);
        assert_eq!(a.tools, None);
        assert_eq!(a.prompt, "Bonjour");
    }

    #[test]
    fn detecte_les_fichiers_invalides() {
        let root = std::env::temp_dir().join(format!("essaim-inv-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".claude/agents")).unwrap();
        std::fs::write(root.join(".claude/agents/ok.md"), "---\nname: ok\ndescription: d\n---\np\n").unwrap();
        std::fs::write(root.join(".claude/agents/casse.md"), "pas de frontmatter").unwrap();
        let inv = invalid_files(&root);
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].0, "casse.md");
    }

    #[test]
    fn suppression_refuse_les_noms_dangereux() {
        let root = std::env::temp_dir().join(format!("essaim-del-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".claude/agents")).unwrap();
        std::fs::write(root.join("victime.md"), "ne pas toucher").unwrap();
        std::fs::write(root.join(".claude/agents/ok.md"), "---\nname: ok\ndescription: d\n---\np\n").unwrap();
        for bad in ["../victime", "..", "a/b", "a\\b", "/tmp/x", "", "x.md", "x.y"] {
            assert!(delete(&root, bad).is_err(), "{bad:?} doit être refusé");
        }
        assert!(root.join("victime.md").exists(), "rien hors de .claude/agents n'est supprimé");
        assert!(delete(&root, "ok").unwrap());
        assert!(!root.join(".claude/agents/ok.md").exists());
        assert!(!delete(&root, "ok").unwrap(), "absent : false, sans erreur");
    }

    #[test]
    fn rejette_sans_frontmatter() {
        assert!(AgentDef::parse("pas de frontmatter").is_err());
    }

    #[test]
    fn charge_seulement_les_noms_valides() {
        let root = std::env::temp_dir().join(format!("essaim-noms-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".claude/agents")).unwrap();
        std::fs::write(root.join(".claude/agents/ok.md"), "---\nname: ok\ndescription: d\n---\np\n").unwrap();
        std::fs::write(root.join(".claude/agents/bad.md"), "---\nname: \"../x y\"\ndescription: d\n---\np\n").unwrap();
        let names: Vec<String> = load_all(&root).into_iter().map(|a| a.name).collect();
        assert!(names.contains(&"ok".to_string()), "{names:?}");
        assert!(!names.iter().any(|n| n.contains('/') || n.contains(' ')), "nom non valide chargé : {names:?}");
    }
}
