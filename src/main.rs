mod model;
mod runtime;
mod tui;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use model::{agent, company::Company};
use runtime::claude_proc::ClaudeEvent;
use runtime::company_exec::{CompanyOptions, note_refus, run_mission};
use runtime::missions;
use runtime::executor::{EscalationReply, NodeStatus, RunEvent, claude_runner};
use std::path::PathBuf;
use tokio::sync::{mpsc, watch};

#[derive(Parser)]
#[command(name = "essaim", about = "Structures multi-agents (entreprises) pour Claude Code")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Crée des agents et une entreprise d'exemple dans le dossier courant (sans écraser).
    Init {
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Organigramme d'une entreprise d'agents.
    Company {
        #[command(subcommand)]
        cmd: CompanyCmd,
    },
    /// Liste les agents disponibles.
    Agents {
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

/// Fichiers créés par `essaim init` : modèles haiku, sauf le dirigeant et les VP (sonnet).
const EXAMPLES: &[(&str, &str)] = &[
    (
        ".claude/agents/poete.md",
        "---\nname: poete\ndescription: Écrit un haïku\nmodel: haiku\n---\n\nTu écris un haïku en français, rien d'autre.\n",
    ),
    (
        ".claude/agents/critique.md",
        "---\nname: critique\ndescription: Note un texte de 1 à 10\nmodel: haiku\n---\n\nTu notes le texte reçu de 1 à 10, en une phrase courte.\n",
    ),
    (
        ".claude/agents/boss.md",
        "---\nname: boss\ndescription: Dirigeant — cadre une mission, la découpe par domaine et arbitre\nmodel: sonnet\n---\n\nTu es le dirigeant de l'entreprise. Tu cadres la mission, la découpes par domaine (technique, sécurité, design…), choisis les bons responsables et arbitres leurs conclusions. Tu es synthétique et décisionnaire.\n",
    ),
    (
        ".claude/agents/vp-tech.md",
        "---\nname: vp-tech\ndescription: Directeur technique — architecture et livraison\nmodel: sonnet\n---\n\nTu diriges la technique. Tu spécialises la part de mission qui te revient, tu confies le travail aux bons managers et tu garantis la cohérence d'architecture.\n",
    ),
    (
        ".claude/agents/vp-secu.md",
        "---\nname: vp-secu\ndescription: Responsable sécurité — risques, menaces et conformité\nmodel: sonnet\n---\n\nTu diriges la sécurité. Tu identifies les risques et exigences de sécurité de la part de mission qui te revient et tu fais vérifier leur couverture.\n",
    ),
    (
        ".claude/agents/vp-design.md",
        "---\nname: vp-design\ndescription: Directeur du design — expérience utilisateur et interface\nmodel: sonnet\n---\n\nTu diriges le design. Tu transformes la part de mission qui te revient en exigences d'expérience et d'interface claires.\n",
    ),
    (
        ".claude/agents/manager.md",
        "---\nname: manager\ndescription: Manager d'équipe — répartit le travail entre ses collaborateurs\nmodel: haiku\n---\n\nTu manages une équipe. Tu découpes la tâche reçue en sous-tâches précises, indépendantes et sans chevauchement, et tu assembles leurs résultats.\n",
    ),
    (
        ".claude/agents/developer.md",
        "---\nname: developer\ndescription: Développeur — conçoit et réalise du code\nmodel: haiku\n---\n\nTu es développeur. Tu travailles uniquement dans le périmètre qu'on t'assigne, tu fais court et correct, et tu signales clairement ce que tu as fait.\n",
    ),
    (
        ".claude/agents/analyste-secu.md",
        "---\nname: analyste-secu\ndescription: Analyste sécurité — revue de risques et recommandations\nmodel: haiku\n---\n\nTu es analyste sécurité. Tu examines le sujet reçu, listes les risques par gravité et proposes des mesures concrètes et vérifiables.\n",
    ),
    (
        ".claude/agents/designer.md",
        "---\nname: designer\ndescription: Designer UX — parcours, écrans et composants\nmodel: haiku\n---\n\nTu es designer UX. Tu proposes parcours, écrans et composants précis, avec leurs états et règles d'accessibilité.\n",
    ),
    (
        ".claude/companies/acme-studio.toml",
        r#"name = "acme-studio"
max_calls = 40
max_parallel = 2
checkpoint = true

[[seat]]
id = "ceo"
title = "CEO"
agent = "boss"

[[seat]]
id = "cto"
title = "CTO"
agent = "vp-tech"
reports_to = "ceo"
expertise = "architecture, développement, livraison"

[[seat]]
id = "ciso"
title = "CISO"
agent = "vp-secu"
reports_to = "ceo"
expertise = "sécurité, risques, conformité"

[[seat]]
id = "vp-design"
title = "VP Design"
agent = "vp-design"
reports_to = "ceo"
expertise = "expérience utilisateur, interface"

[[seat]]
id = "mgr-eng"
title = "Engineering Manager"
agent = "manager"
reports_to = "cto"

[[seat]]
id = "dev-api"
title = "Backend Engineer"
agent = "developer"
reports_to = "mgr-eng"
builds = true

[[seat]]
id = "dev-ui"
title = "Frontend Engineer"
agent = "developer"
reports_to = "mgr-eng"
builds = true

[[seat]]
id = "sec-analyst"
title = "Security Analyst"
agent = "analyste-secu"
reports_to = "ciso"

[[seat]]
id = "ux"
title = "UX Designer"
agent = "designer"
reports_to = "vp-design"
"#,
    ),
];

#[derive(Subcommand)]
enum CompanyCmd {
    /// Affiche l'organigramme (niveaux, postes, agents) et l'estimation d'appels.
    Show { file: PathBuf },
    /// Exécute une mission : conception (délégation en cascade, rapports), point de contrôle, réalisation.
    Run {
        file: PathBuf,
        /// Brief de la mission : texte, ou @fichier pour le lire dans un fichier.
        #[arg(long, conflicts_with = "mission")]
        brief: Option<String>,
        /// Mission existante (.essaim/missions/<id>/brief.md).
        #[arg(long)]
        mission: Option<String>,
        /// Confirme sans demander : mission de plus de 30 appels et point de contrôle.
        #[arg(long)]
        yes: bool,
        /// Reprend une mission interrompue (avec --mission) : une conception terminée n'est pas rejouée, on repart de son plan.
        #[arg(long, requires = "mission")]
        resume: bool,
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Valide l'organigramme (hiérarchie, agents, postes de réalisation) sans rien exécuter.
    Check {
        file: PathBuf,
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

/// De quoi ouvrir une conversation avec le boss pour réviser le plan au point de contrôle.
struct PlanEdit {
    company: Company,
    agents: Vec<agent::AgentDef>,
    root: PathBuf,
    mission_dir: PathBuf,
    lang: tui::settings::Lang,
}

impl PlanEdit {
    /// Session `claude` interactive (mode plan) qui réécrit `conception.md` ; renvoie le plan relu.
    async fn revise(&self) -> Option<String> {
        let boss = self.company.root()?;
        let seat = &self.company.seats[boss];
        let prompt = self.agents.iter().find(|a| a.name == seat.agent).map(|a| a.prompt.clone()).unwrap_or_default();
        let rel = |name: &str| self.mission_dir.strip_prefix(&self.root).unwrap_or(&self.mission_dir).join(name).display().to_string();
        let args = tui::assistant::plan_revision_args(self.lang, &self.company.name, &seat.title, &prompt, &self.company.outline(), &rel("brief.md"), &rel("conception.md"));
        eprintln!("\n── conversation avec le boss : /exit pour revenir au point de contrôle ──");
        match runtime::claude_proc::claude_command().args(&args).current_dir(&self.root).status().await {
            Ok(st) if st.success() => {}
            Ok(st) => eprintln!("⚠ la session s'est terminée avec {st}"),
            Err(e) => {
                eprintln!("⚠ impossible de lancer claude : {e}");
                return None;
            }
        }
        std::fs::read_to_string(self.mission_dir.join("conception.md")).ok().filter(|t| !t.trim().is_empty())
    }
}

impl PlanEdit {
    /// Session `claude` interactive avec le boss d'une mission bloquée : il écrit sa décision dans `resolution.md`.
    async fn discuss(&self, blockage: &str) {
        let Some(boss) = self.company.root() else { return };
        let seat = &self.company.seats[boss];
        let prompt = self.agents.iter().find(|a| a.name == seat.agent).map(|a| a.prompt.clone()).unwrap_or_default();
        let rel = |name: &str| self.mission_dir.strip_prefix(&self.root).unwrap_or(&self.mission_dir).join(name).display().to_string();
        let args = tui::assistant::escalation_args(self.lang, &self.company.name, &seat.title, &prompt, &self.company.outline(), &rel("brief.md"), &rel(missions::RESOLUTION_FILE), blockage);
        eprintln!("\n── discussion avec le boss : /exit pour revenir au blocage ──");
        match runtime::claude_proc::claude_command().args(&args).current_dir(&self.root).status().await {
            Ok(st) if st.success() => {}
            Ok(st) => eprintln!("⚠ la session s'est terminée avec {st}"),
            Err(e) => eprintln!("⚠ impossible de lancer claude : {e}"),
        }
    }
}

/// Affiche l'avancement d'un run sur stderr (libellés hiérarchiques inclus).
fn spawn_printer(mut rx: mpsc::UnboundedReceiver<RunEvent>, auto_yes: bool, edit: Option<PlanEdit>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut labels: std::collections::HashMap<String, (String, usize)> = Default::default();
        let name = |labels: &std::collections::HashMap<String, (String, usize)>, id: &str| match labels.get(id) {
            Some((l, d)) => format!("{}{l}", "  ".repeat(*d)),
            None => id.to_string(),
        };
        while let Some(ev) = rx.recv().await {
            match ev {
                RunEvent::Label { id, label, depth, .. } => {
                    labels.insert(id, (label, depth));
                }
                RunEvent::Checkpoint { mut plan, reply } => {
                    let go = loop {
                        eprintln!("\n──────── Plan consolidé (fin de la conception) ────────\n{plan}\n────────────────────────────────────────────────────────");
                        if auto_yes {
                            eprintln!("--yes : la réalisation démarre.");
                            break true;
                        }
                        eprint!("Lancer la réalisation (les postes ⚒ vont modifier le projet) ? [o/N/e = modifier le plan avec le boss] ");
                        let answer = tokio::task::spawn_blocking(|| {
                            let mut line = String::new();
                            let _ = std::io::stdin().read_line(&mut line);
                            line.trim().to_lowercase()
                        })
                        .await
                        .unwrap_or_default();
                        match (answer.as_str(), &edit) {
                            ("e" | "edit" | "modifier", Some(ed)) => match ed.revise().await {
                                Some(new_plan) if new_plan != plan => {
                                    eprintln!("plan révisé.");
                                    plan = new_plan;
                                }
                                Some(_) => eprintln!("plan inchangé."),
                                None => eprintln!("⚠ conception.md illisible ou vide : plan d'origine conservé."),
                            },
                            (_, _) => break matches!(answer.as_str(), "o" | "oui" | "y" | "yes"),
                        }
                    };
                    let _ = reply.send(go);
                }
                RunEvent::Escalation { summary, reply } => {
                    let go = loop {
                        eprintln!("\n──────── Mission bloquée : le boss n'a pas pu la débloquer ────────\n{summary}\n────────────────────────────────────────────────────────");
                        if auto_yes {
                            eprintln!("--yes : jamais de poursuite automatique sur un blocage, la mission s'arrête.");
                            break EscalationReply::Stop;
                        }
                        eprint!("[r = relancer la correction / e = en discuter avec le boss / N = arrêter] ");
                        let answer = tokio::task::spawn_blocking(|| {
                            let mut line = String::new();
                            let _ = std::io::stdin().read_line(&mut line);
                            line.trim().to_lowercase()
                        })
                        .await
                        .unwrap_or_default();
                        match (answer.as_str(), &edit) {
                            ("e" | "discuter", Some(ed)) => ed.discuss(&summary).await,
                            ("r" | "relancer", _) => break EscalationReply::Retry,
                            _ => break EscalationReply::Stop,
                        }
                    };
                    let _ = reply.send(go);
                }
                RunEvent::NodeStarted { id } => eprintln!("▶ {}", name(&labels, &id)),
                RunEvent::Node { id, event: ClaudeEvent::ToolUse { name: tool, .. } } => {
                    eprintln!("  [{}] outil {tool}", name(&labels, &id).trim())
                }
                RunEvent::Node { .. } => {}
                RunEvent::NodeFinished { id, status, tokens } => {
                    let n = name(&labels, &id);
                    match status {
                        NodeStatus::Succeeded => eprintln!("✔ {n} ({tokens} tokens)"),
                        NodeStatus::Failed(e) => eprintln!("✖ {n} : {e}"),
                        NodeStatus::Blocked(b) => eprintln!("⚠ {n} bloqué : {}", b.replace('\n', " ; ")),
                        NodeStatus::Skipped(r) => eprintln!("⊘ {n} : {r}"),
                        NodeStatus::Cancelled => eprintln!("✖ {n} : annulé"),
                    }
                }
                RunEvent::RunFinished { ok, tokens } => {
                    eprintln!("{} {tokens} tokens au total", if ok { "terminé —" } else { "échec —" })
                }
            }
        }
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        None => tui::run(std::env::current_dir()?).await,
        Some(Cmd::Init { root }) => {
            let root = root.unwrap_or(std::env::current_dir()?);
            for (path, content) in EXAMPLES {
                let path = root.join(path);
                if path.exists() {
                    println!("existe déjà : {}", path.display());
                    continue;
                }
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, content)?;
                println!("créé : {}", path.display());
            }
            println!("\nLancez `essaim` : onglet Entreprise (organigramme d'exemple).");
            Ok(())
        }
        Some(Cmd::Company { cmd }) => {
            match cmd {
                CompanyCmd::Show { file } => {
                    let c = Company::load(&file)?;
                    let e = c.estimate_calls();
                    println!(
                        "{} — {} poste(s) · max {} appels · parallélisme {} · point de contrôle : {}",
                        c.name,
                        c.seats.len(),
                        c.max_calls,
                        c.max_parallel,
                        if c.checkpoint { "oui" } else { "non" }
                    );
                    for l in c.outline() {
                        println!("{l}");
                    }
                    println!("Appels estimés (majorant) : conception {} + réalisation {} = {}", e.conception, e.realisation, e.total());
                }
                CompanyCmd::Run { file, brief, mission, yes, resume, root } => {
                    let root = root.unwrap_or(std::env::current_dir()?).canonicalize().context("racine introuvable")?;
                    let company = Company::load(&file)?;
                    let agents = agent::load_all(&root);
                    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
                    company.validate(&names)?;
                    // Brief : mission existante, ou nouveau dossier de mission créé à partir du texte.
                    let (mission_id, brief_text) = match (mission, brief) {
                        (Some(id), _) => {
                            anyhow::ensure!(missions::is_valid_id(&id), "identifiant de mission invalide : {id:?}");
                            let text = std::fs::read_to_string(missions::brief_path(&root, &id)).with_context(|| format!("mission {id} introuvable (brief.md)"))?;
                            (id, text)
                        }
                        (None, Some(b)) => {
                            let text = match b.strip_prefix('@') {
                                Some(path) => std::fs::read_to_string(path).with_context(|| format!("lecture du brief {path}"))?,
                                None => b,
                            };
                            let id = missions::new_id(runtime::store::now_ms());
                            let dir = missions::create(&root, &id)?;
                            missions::write_output(&dir, "brief.md", &text)?;
                            (id, text)
                        }
                        (None, None) => anyhow::bail!("indiquez --brief \"…\" ou --mission <id>"),
                    };
                    if let Some(m) = missions::list(&root).into_iter().find(|m| m.id == mission_id).filter(|m| !m.waiting_for.is_empty()) {
                        let names: Vec<String> = m.waiting_for.iter().map(|id| format!("{id} ({})", missions::list(&root).into_iter().find(|x| &x.id == id).map(|x| x.title).unwrap_or_default())).collect();
                        anyhow::bail!("mission {mission_id} en attente : réussir d'abord {}", names.join(", "));
                    }
                    if resume && missions::resumable_plan(&missions::dir_of(&root, &mission_id)).is_none() {
                        anyhow::bail!("mission {mission_id} non reprenable : aucune conception menée à terme (relancez sans --resume)");
                    }
                    let e = company.estimate_calls();
                    eprintln!(
                        "{} — mission {mission_id} : jusqu'à {} appel(s) (conception {} + réalisation {}), plafond {}, {} en parallèle",
                        company.name, e.total(), e.conception, e.realisation, company.max_calls, company.max_parallel
                    );
                    if e.total() > 30 && !yes {
                        anyhow::bail!("mission estimée à plus de 30 appels : relancez avec --yes pour confirmer");
                    }
                    let (tx, rx) = mpsc::unbounded_channel();
                    let (cancel_tx, cancel_rx) = watch::channel(false);
                    tokio::spawn(async move {
                        if tokio::signal::ctrl_c().await.is_ok() {
                            eprintln!("\nannulation…");
                            let _ = cancel_tx.send(true);
                        }
                    });
                    let mission_dir = missions::dir_of(&root, &mission_id);
                    let lang = crate::tui::settings::path().map(|p| crate::tui::settings::load(&p)).and_then(|s| s.lang).unwrap_or_default();
                    let edit = PlanEdit { company: company.clone(), agents: agents.clone(), root: root.clone(), mission_dir: mission_dir.clone(), lang };
                    let printer = spawn_printer(rx, yes, Some(edit));
                    let opts = CompanyOptions { base_dir: root, mission_dir: Some(mission_dir.clone()), lang, resume };
                    let out = run_mission(&company, &agents, &brief_text, &opts, claude_runner(), tx, cancel_rx).await?;
                    printer.await?;
                    if let Some(note) = note_refus(out.approved) {
                        eprintln!("{note}");
                    }
                    let blockers: Vec<_> = out.conception.blockers.iter().chain(out.realisation.iter().flat_map(|r| r.blockers.iter())).collect();
                    if !blockers.is_empty() {
                        eprintln!("⚠ Mission bloquée, blocages non levés :");
                        for b in blockers {
                            eprintln!("  - {}", b.line());
                        }
                    }
                    eprintln!("Livrables : {}", mission_dir.display());
                    if out.ok {
                        let all = missions::list(&PathBuf::from(&mission_dir).ancestors().nth(3).map(PathBuf::from).unwrap_or_default());
                        for n in missions::unlocked_by(&all, &mission_id) {
                            eprintln!("➜ mission débloquée : {} — essaim company run <entreprise> --mission {}", n.title, n.id);
                        }
                    }
                    println!("{}", out.report);
                    if !out.ok {
                        std::process::exit(1);
                    }
                }
                CompanyCmd::Check { file, root } => {
                    let root = root.unwrap_or(std::env::current_dir()?);
                    let c = Company::load(&file)?;
                    let names: Vec<String> = agent::load_all(&root).into_iter().map(|a| a.name).collect();
                    c.validate(&names)?;
                    let e = c.estimate_calls();
                    println!("✔ {} : {} poste(s), valide — jusqu'à {} appel(s) par mission", c.name, c.seats.len(), e.total());
                    if e.total() > c.max_calls {
                        println!("⚠ l'estimation dépasse max_calls ({}) : une mission pourrait être interrompue", c.max_calls);
                    }
                }
            }
            Ok(())
        }
        Some(Cmd::Agents { root }) => {
            let root = root.unwrap_or(std::env::current_dir()?);
            for a in agent::load_all(&root) {
                println!("{:<24} {}", a.name, a.description);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn la_revision_cli_lance_claude_en_mode_plan_et_relit_le_plan() {
        let root = std::env::temp_dir().join(format!("essaim-cli-{}", uuid::Uuid::new_v4()));
        let dir = root.join(".essaim/missions/42");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("conception.md"), "PLAN V1").unwrap();
        // Faux claude : écrit le plan révisé et garde ses arguments.
        let fake = root.join("fake-claude.sh");
        std::fs::write(&fake, format!("#!/bin/sh\necho \"$@\" > '{0}/args.txt'\nprintf 'PLAN V2' > '{0}/.essaim/missions/42/conception.md'\n", root.display())).unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        unsafe { std::env::set_var("ESSAIM_CLAUDE_BIN", &fake) };
        let company = Company::parse("name = \"acme\"\n[[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"a\"\n").unwrap();
        let edit = PlanEdit { company, agents: vec![], root: root.clone(), mission_dir: dir, lang: tui::settings::Lang::Fr };
        assert_eq!(edit.revise().await.as_deref(), Some("PLAN V2"));
        let args = std::fs::read_to_string(root.join("args.txt")).unwrap();
        assert!(args.contains("--permission-mode plan") && args.contains(".essaim/missions/42/conception.md"), "{args}");
    }
}
