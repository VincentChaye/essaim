use super::ConfigDirs;
use crate::tf;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Définition d'un agent, stockée dans un fichier `.claude/agents/<name>.md`
/// (frontmatter YAML + corps = prompt système).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub prompt: String,
    /// Fichier d'où l'agent a été lu (`None` : définition en mémoire, pas encore écrite).
    pub path: Option<PathBuf>,
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
            path: None,
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

/// Charge tous les agents, du projet puis de l'utilisateur ; le projet prend le pas en cas de doublon.
pub fn load_all(dirs: &ConfigDirs) -> Vec<AgentDef> {
    let mut out: Vec<AgentDef> = Vec::new();
    for p in md_files(&dirs.agents()) {
        if let Ok(src) = super::read_config(&p) {
            if let Ok(mut a) = AgentDef::parse(&src) {
                // Le nom vient du frontmatter : même règle qu'à l'écriture, il devient un argument CLI.
                if is_valid_name(&a.name) && !out.iter().any(|x| x.name == a.name) {
                    a.path = Some(p);
                    out.push(a);
                }
            }
        }
    }
    out
}

/// Fichiers `.md` des dossiers donnés, triés dans chaque dossier (ordre des dossiers conservé).
fn md_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = vec![];
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md")).collect();
        files.sort();
        out.extend(files);
    }
    out
}

/// Fichier d'un agent : celui qui existe déjà (projet, puis utilisateur), sinon celui à créer —
/// côté utilisateur, pour qu'un agent créé soit disponible depuis tout le système de fichiers.
pub fn path_of(dirs: &ConfigDirs, name: &str) -> Result<PathBuf> {
    check_name(name)?;
    let file = format!("{name}.md");
    match dirs.agents().into_iter().map(|d| d.join(&file)).find(|p| p.exists()) {
        Some(p) => Ok(p),
        None => Ok(dirs.agents_write().join(file)),
    }
}

/// Nom d'agent sûr pour un nom de fichier : lettres, chiffres, `-` et `_` uniquement
/// (donc ni séparateur de chemin, ni `..`, ni point, ni vide).
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Écrit l'agent : dans son fichier s'il en a déjà un, sinon dans le dossier de l'utilisateur
/// (un agent créé vaut pour tous les projets). Renvoie le fichier écrit.
pub fn save(dirs: &ConfigDirs, agent: &AgentDef) -> Result<PathBuf> {
    let path = path_of(dirs, &agent.name)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, agent.render()?)?;
    Ok(path)
}

/// Fichiers `*.md` (projet et utilisateur) qui ne se lisent pas : (fichier, erreur).
pub fn invalid_files(dirs: &ConfigDirs) -> Vec<(PathBuf, String)> {
    md_files(&dirs.agents())
        .into_iter()
        .filter_map(|p| {
            let src = super::read_config(&p).ok()?;
            let err = AgentDef::parse(&src).err()?;
            Some((p, format!("{err:#}")))
        })
        .collect()
}

/// Supprime le fichier d'un agent, côté projet ou côté utilisateur (le premier trouvé, dans cet
/// ordre : un même nom présent des deux côtés demande deux suppressions, comme deux agents).
/// Renvoie le fichier supprimé, `None` s'il n'y en avait aucun.
pub fn delete(dirs: &ConfigDirs, name: &str) -> Result<Option<PathBuf>> {
    // Le nom vient aussi du frontmatter d'un fichier (non validé au chargement) : jamais de chemin construit tel quel.
    check_name(name)?;
    let file = format!("{name}.md");
    let Some(path) = dirs.agents().into_iter().map(|d| d.join(&file)).find(|p| p.exists()) else { return Ok(None) };
    std::fs::remove_file(&path)?;
    Ok(Some(path))
}

fn check_name(name: &str) -> Result<()> {
    if !is_valid_name(name) {
        bail!("{}", tf!("nom d'agent invalide : {name:?} (lettres, chiffres, - et _ uniquement)", "invalid agent name: {name:?} (letters, digits, - and _ only)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Projet jetable et dossier utilisateur jetable : jamais le vrai `~/.claude`.
    fn dirs_test(nom: &str) -> (ConfigDirs, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("essaim-{nom}-{}", uuid::Uuid::new_v4()));
        let d = ConfigDirs { project: base.join("projet"), user: Some(base.join("maison/.claude")) };
        std::fs::create_dir_all(d.project.join(".claude/agents")).unwrap();
        std::fs::create_dir_all(d.user.clone().unwrap().join("agents")).unwrap();
        (d, base)
    }

    #[test]
    fn roundtrip() {
        let a = AgentDef {
            name: "code-reviewer".into(),
            description: "Relit le code".into(),
            model: Some("sonnet".into()),
            tools: Some(vec!["Read".into(), "Grep".into()]),
            prompt: "Tu es un relecteur.\n\nSois bref.".into(),
            path: None,
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
    fn detecte_les_fichiers_invalides_des_deux_cotes() {
        let (d, _base) = dirs_test("inv");
        std::fs::write(d.project.join(".claude/agents/ok.md"), "---\nname: ok\ndescription: d\n---\np\n").unwrap();
        std::fs::write(d.project.join(".claude/agents/casse.md"), "pas de frontmatter").unwrap();
        std::fs::write(d.user.clone().unwrap().join("agents/casse-global.md"), "pas de frontmatter non plus").unwrap();
        let inv = invalid_files(&d);
        let noms: Vec<String> = inv.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(noms, vec!["casse.md", "casse-global.md"]);
    }

    #[test]
    fn suppression_refuse_les_noms_dangereux() {
        let (d, _base) = dirs_test("del");
        std::fs::write(d.project.join("victime.md"), "ne pas toucher").unwrap();
        std::fs::write(d.project.join(".claude/agents/ok.md"), "---\nname: ok\ndescription: d\n---\np\n").unwrap();
        for bad in ["../victime", "..", "a/b", "a\\b", "/tmp/x", "", "x.md", "x.y"] {
            assert!(delete(&d, bad).is_err(), "{bad:?} doit être refusé");
        }
        assert!(d.project.join("victime.md").exists(), "rien hors de .claude/agents n'est supprimé");
        assert!(delete(&d, "ok").unwrap().is_some());
        assert!(!d.project.join(".claude/agents/ok.md").exists());
        assert!(delete(&d, "ok").unwrap().is_none(), "absent : None, sans erreur");
    }

    /// Un agent créé est global : il est écrit dans `~/.claude/agents` et se charge depuis n'importe
    /// quel projet. Un agent déjà présent dans le projet, lui, reste dans le projet et le supplante.
    #[test]
    fn creation_globale_et_priorite_du_projet() {
        let (d, _base) = dirs_test("global");
        let a = AgentDef { name: "relecteur".into(), description: "relit".into(), prompt: "p".into(), ..Default::default() };
        let ecrit = save(&d, &a).unwrap();
        assert_eq!(ecrit, d.user.clone().unwrap().join("agents/relecteur.md"));
        assert!(d.is_global(&ecrit));

        // Vu depuis un autre projet, sans aucun fichier local.
        let ailleurs = ConfigDirs { project: d.project.join("autre-projet"), user: d.user.clone() };
        let charges = load_all(&ailleurs);
        assert_eq!(charges.len(), 1);
        assert_eq!(charges[0].name, "relecteur");
        assert_eq!(charges[0].path.as_deref(), Some(ecrit.as_path()));

        // Même nom côté projet : c'est lui qui est chargé, et c'est lui que `save` réécrit.
        let local = d.project.join(".claude/agents/relecteur.md");
        std::fs::write(&local, "---\nname: relecteur\ndescription: version du projet\n---\np\n").unwrap();
        let charges = load_all(&d);
        assert_eq!(charges.len(), 1, "un seul agent par nom : {charges:?}");
        assert_eq!(charges[0].description, "version du projet");
        assert_eq!(save(&d, &a).unwrap(), local);
        assert!(std::fs::read_to_string(&ecrit).unwrap().contains("relit"), "le fichier global est intact");
    }

    #[test]
    fn rejette_sans_frontmatter() {
        assert!(AgentDef::parse("pas de frontmatter").is_err());
    }

    #[test]
    fn charge_seulement_les_noms_valides() {
        let (d, _base) = dirs_test("noms");
        std::fs::write(d.project.join(".claude/agents/ok.md"), "---\nname: ok\ndescription: d\n---\np\n").unwrap();
        std::fs::write(d.project.join(".claude/agents/bad.md"), "---\nname: \"../x y\"\ndescription: d\n---\np\n").unwrap();
        let names: Vec<String> = load_all(&d).into_iter().map(|a| a.name).collect();
        assert!(names.contains(&"ok".to_string()), "{names:?}");
        assert!(!names.iter().any(|n| n.contains('/') || n.contains(' ')), "nom non valide chargé : {names:?}");
    }
}
