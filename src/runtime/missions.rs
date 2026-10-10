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
    /// Entreprise (fichier) qui a lancé la mission ; `None` tant qu'aucune ne l'a lancée.
    pub company: Option<String>,
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
                company: std::fs::read_to_string(dir.join(COMPANY_FILE)).ok().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()),
                dir,
            }
        })
        .collect();
    v.sort_by(|a, b| b.id.cmp(&a.id));
    v
}

/// Résumé concis d'une mission, tiré de ses fichiers : but, fichiers cités, résultat.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Summary {
    pub goal: Vec<String>,
    pub files: Vec<String>,
    pub outcome: Vec<String>,
}

/// Lignes (puces nettoyées) de la première section dont le titre contient un des `titles`.
fn section(md: &str, titles: &[&str], max: usize) -> Vec<String> {
    let mut on = false;
    let mut out = vec![];
    for l in md.lines() {
        let t = l.trim();
        if t.starts_with('#') {
            let h = t.trim_start_matches('#').trim().to_lowercase();
            if on && !out.is_empty() {
                break;
            }
            on = titles.iter().any(|x| h.contains(x));
            continue;
        }
        if on && !t.is_empty() {
            let c = t.trim_start_matches(['-', '*', ' ']).replace("**", "").replace('`', "");
            out.push(c.chars().take(110).collect());
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Chemins de fichiers cités dans le texte (`src/x.rs`, `Cargo.toml:12`…), sans doublon.
fn files_in(md: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for w in md.split(|c: char| c.is_whitespace() || "`()[],;«»\"'*".contains(c)) {
        let w = w.trim_end_matches(['.', ':']);
        let w = match w.rsplit_once(':') {
            Some((a, b)) if !b.is_empty() && b.chars().all(|c| c.is_ascii_digit() || c == '-') => a,
            _ => w,
        };
        let ext = w.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        let known = matches!(ext, "rs" | "md" | "toml" | "sh" | "json" | "yml" | "yaml" | "ts" | "tsx" | "js" | "py" | "css" | "html");
        if known && w.len() > 3 && !w.contains("://") && !w.contains('*') && !w.starts_with("..") && !out.iter().any(|f| f == w) {
            out.push(w.to_string());
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

pub fn summarize(dir: &Path) -> Summary {
    let read = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap_or_default();
    let (brief, plan, report) = (read("brief.md"), read("conception.md"), read("rapport.md"));
    let mut goal = section(&brief, &["objectif", "but"], 1);
    if goal.is_empty() {
        goal = brief.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).take(1).map(|l| l.chars().take(110).collect()).collect();
    }
    let mut files = files_in(&report, 4);
    if files.is_empty() {
        files = files_in(&plan, 4);
    }
    let outcome = section(&report, &["conclusion", "synthèse", "résumé", "résultat"], 2);
    Summary { goal, files, outcome }
}

const COMPANY_FILE: &str = "company";

/// Rattache la mission à l'entreprise qui la réalise.
pub fn set_company(dir: &Path, company_file: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    write_private(&dir.join(COMPANY_FILE), company_file.as_bytes())?;
    Ok(())
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
    anyhow::ensure!(matches!(name, "conception.md" | "rapport.md" | "brief.md" | "compte-rendu.md"), "nom de livrable inattendu : {name}");
    std::fs::create_dir_all(dir)?;
    // Livrables = texte du projet et de la mission : réservés à l'utilisateur (0600).
    write_private(&dir.join(name), text.as_bytes())?;
    Ok(())
}

/// Une ligne du déroulé d'une mission (un appel `claude`).
pub struct StepLine {
    pub label: String,
    pub agent: String,
    pub status: String,
    pub tokens: u64,
}

/// Données d'un compte rendu de fin de mission.
pub struct ReportData<'a> {
    pub title: &'a str,
    pub company: &'a str,
    pub date: &'a str,
    pub ok: bool,
    pub realised: bool,
    pub tokens: u64,
    pub steps: Vec<StepLine>,
    /// Points d'attention : (libellé, message) des appels échoués ou ignorés.
    pub issues: Vec<(String, String)>,
    /// Rapport final du boss (rapport.md, sinon plan de conception).
    pub final_report: &'a str,
}

/// Compte rendu Markdown de fin de mission (`compte-rendu.md`), lisible dans l'onglet Entreprise.
pub fn compte_rendu(d: &ReportData, fr: bool) -> String {
    let t = |a: &'static str, b: &'static str| if fr { a } else { b };
    let verdict = match (d.ok, d.realised) {
        (true, true) => t("✔ terminée", "✔ completed"),
        (true, false) => t("✔ conception terminée, réalisation non lancée", "✔ design completed, realisation not started"),
        (false, _) => t("✖ terminée avec des échecs ou interrompue", "✖ finished with failures or interrupted"),
    };
    let mut s = format!("# {} — {}\n\n", t("Compte rendu", "Report"), d.title);
    s.push_str(&format!("- **{}** : {}\n- **{}** : {}\n- **{}** : {}\n- **Tokens** : {}\n- **{}** : {}\n\n", t("Entreprise", "Company"), d.company, t("Date", "Date"), d.date, t("Résultat", "Result"), verdict, d.tokens, t("Appels", "Calls"), d.steps.len()));
    s.push_str(&format!("## {}\n\n", t("Ce qui a été fait", "What was done")));
    if d.final_report.trim().is_empty() {
        s.push_str(t("_Aucun rapport final n'a été produit._\n\n", "_No final report was produced._\n\n"));
    } else {
        s.push_str(d.final_report.trim());
        s.push_str("\n\n");
    }
    s.push_str(&format!("## {}\n\n", t("Déroulé", "Timeline")));
    if d.steps.is_empty() {
        s.push_str(t("_Aucun appel._\n\n", "_No calls._\n\n"));
    } else {
        s.push_str(&format!("| {} | Agent | {} | Tokens |\n|---|---|---|---:|\n", t("Étape", "Step"), t("Résultat", "Result")));
        for l in &d.steps {
            s.push_str(&format!("| {} | {} | {} | {} |\n", l.label.replace('|', "/"), l.agent, l.status, l.tokens));
        }
        s.push('\n');
    }
    if !d.issues.is_empty() {
        s.push_str(&format!("## {}\n\n", t("Points d'attention", "Points of attention")));
        for (label, msg) in &d.issues {
            let first = msg.lines().next().unwrap_or("").trim();
            s.push_str(&format!("- **{label}** — {first}\n"));
        }
        s.push('\n');
    }
    s.push_str(&format!("## {}\n\n`brief.md` · `conception.md`{} · `compte-rendu.md`\n", t("Fichiers de la mission", "Mission files"), if d.realised { " · `rapport.md`" } else { "" }));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compte_rendu_resume_la_mission() {
        let d = ReportData {
            title: "Site", company: "Acme", date: "01/01 10:00", ok: false, realised: true, tokens: 1234,
            steps: vec![StepLine { label: "CEO · reports".into(), agent: "boss".into(), status: "✔".into(), tokens: 10 }],
            issues: vec![("Dev · builds".into(), "boum\nsuite".into())],
            final_report: "Tout est fait.",
        };
        let md = compte_rendu(&d, true);
        assert!(md.starts_with("# Compte rendu — Site"));
        assert!(md.contains("Tout est fait.") && md.contains("| CEO · reports | boss | ✔ | 10 |"));
        assert!(md.contains("**Dev · builds** — boum") && !md.contains("suite"));
        assert!(md.contains("`rapport.md`") && md.contains("**Tokens** : 1234"));
    }

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

#[cfg(test)]
mod tests_company {
    use super::*;

    #[test]
    fn mission_rattachee_a_son_entreprise() {
        let root = std::env::temp_dir().join(format!("essaim-mis-{}", uuid::Uuid::new_v4()));
        let dir = create(&root, "m1").unwrap();
        write_output(&dir, "brief.md", "# Test").unwrap();
        assert_eq!(list(&root)[0].company, None);
        set_company(&dir, "a.toml").unwrap();
        assert_eq!(list(&root)[0].company.as_deref(), Some("a.toml"));
    }
}
