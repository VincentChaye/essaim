mod model;
mod runtime;
mod tui;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use model::{ConfigDirs, agent, company, company::Company};
use runtime::claude_proc::ClaudeEvent;
use runtime::company_exec::{CompanyOptions, note_refus, run_mission};
use runtime::missions;
use runtime::pause;
use runtime::executor::{AskKind, HumanAnswer, HumanRequest, NodeStatus, RunEvent, claude_runner};
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
    /// Crée des agents et une entreprise d'exemple (sans écraser). Par défaut dans `~/.claude`,
    /// donc visibles depuis tous vos dossiers ; `--root <projet>` les crée dans ce projet seul.
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
    /// Ouvre le tableau de bord de consommation dans le navigateur : jetons dans le temps, par session,
    /// par dossier, par modèle et par poste, plus les terminaux `claude` ouverts. Page servie sur la
    /// boucle locale uniquement, avec une clé dans l'URL.
    #[command(visible_alias = "usage")]
    Conso {
        /// Port d'écoute sur 127.0.0.1 ; par défaut 8787, puis le port suivant s'il est pris.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Affiche seulement l'URL, sans ouvrir le navigateur.
        #[arg(long)]
        no_open: bool,
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Choisit le profil de sécurité des agents : `dedicated` (appareil sans données personnelles : tous les
    /// droits) ou `protected` (bac à sable, et demandes à l'humain pour tout ce qui en sort). Sans argument,
    /// explique les deux profils et pose la question.
    Profile {
        /// `dedicated` ou `protected`.
        choice: Option<String>,
    },
    /// Serveur MCP d'essaim pour un poste (lancé par `claude`, pas à la main) : relaie les demandes de
    /// l'agent à l'humain.
    #[command(hide = true)]
    Mcp {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long, default_value = "")]
        node: String,
        #[arg(long, default_value = "")]
        seat: String,
        /// Expose aussi l'outil de permission (profil protégé).
        #[arg(long)]
        permission: bool,
    },
}

/// Fichiers créés par `essaim init` : exemples haiku (poète, critique) ; les postes qui produisent le travail et ceux qui arbitrent sont en sonnet.
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
        "---\nname: manager\ndescription: Manager d'équipe — répartit le travail entre ses collaborateurs\nmodel: sonnet\n---\n\nTu manages une équipe. Tu découpes la tâche reçue en sous-tâches précises, indépendantes et sans chevauchement, et tu assembles leurs résultats.\n",
    ),
    (
        ".claude/agents/developer.md",
        "---\nname: developer\ndescription: Développeur — conçoit et réalise du code\nmodel: sonnet\n---\n\nTu es développeur. Tu travailles uniquement dans le périmètre qu'on t'assigne, tu fais court et correct, et tu signales clairement ce que tu as fait.\n",
    ),
    (
        ".claude/agents/analyste-secu.md",
        "---\nname: analyste-secu\ndescription: Analyste sécurité — revue de risques et recommandations\nmodel: sonnet\n---\n\nTu es analyste sécurité. Tu examines le sujet reçu, listes les risques par gravité et proposes des mesures concrètes et vérifiables.\n",
    ),
    (
        ".claude/agents/designer.md",
        "---\nname: designer\ndescription: Designer UX — parcours, écrans et composants\nmodel: sonnet\n---\n\nTu es designer UX. Tu proposes parcours, écrans et composants précis, avec leurs états et règles d'accessibilité.\n",
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
    /// Exécute une mission : plan du boss, point de contrôle, puis réalisation (délégation en cascade, rapports).
    Run {
        file: PathBuf,
        /// Brief de la mission : texte, ou @fichier pour le lire dans un fichier.
        #[arg(long, conflicts_with = "mission")]
        brief: Option<String>,
        /// Mission existante (.essaim/missions/<id>/brief.md).
        #[arg(long)]
        mission: Option<String>,
        /// Sans humain : confirme la mission de plus de 30 appels et le point de contrôle ; une question d'un
        /// agent prend l'option qu'il recommande, une demande d'action ou de permission reste sans réponse.
        #[arg(long)]
        yes: bool,
        /// Reprend une mission interrompue (avec --mission) : un plan déjà écrit n'est pas réécrit, on repart de lui.
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

/// Lit une ligne sur l'entrée standard, sans bloquer l'exécuteur. `None` : entrée fermée.
async fn read_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(n) if n > 0 => Some(line.trim().to_string()),
            _ => None,
        }
    })
    .await
    .ok()
    .flatten()
}

/// Présente une demande à l'humain sur le terminal et lit ses réponses. Un numéro choisit une option,
/// tout autre texte est une réponse libre, une ligne vide prend l'option recommandée s'il y en a une.
/// Avec `--yes` ou une entrée fermée, personne ne répond : voir `HumanRequest::unattended`.
async fn answer_on_terminal(req: &HumanRequest, auto_yes: bool) -> Vec<HumanAnswer> {
    let qui = match req.kind {
        AskKind::Question => "question de",
        AskKind::Action => "action demandée par",
        AskKind::Blocked => "poste bloqué :",
        AskKind::Permission => "autorisation demandée par",
        AskKind::Review => "revue de",
    };
    eprintln!("\n──────── {qui} {} ────────", req.seat);
    let repli = req.unattended();
    let mut out = vec![];
    for (n, q) in req.questions.iter().enumerate() {
        eprintln!("{}", q.text);
        for (i, o) in q.options.iter().enumerate() {
            eprintln!("  {}. {o}{}", i + 1, if q.recommended == Some(i) { "  (recommandé)" } else { "" });
        }
        if auto_yes {
            match &repli[n] {
                HumanAnswer::Option(i) => eprintln!("--yes : option recommandée retenue ({}).", q.options[*i]),
                _ => eprintln!("--yes : personne pour répondre, la demande reste sans réponse."),
            }
            out.push(repli[n].clone());
            continue;
        }
        eprint!("{} ", if q.options.is_empty() { "Votre réponse :" } else { "Numéro, ou réponse libre :" });
        out.push(match read_line().await {
            None => repli[n].clone(),
            Some(l) if l.is_empty() => q.recommended.map(HumanAnswer::Option).unwrap_or(HumanAnswer::Unavailable),
            Some(l) => match l.parse::<usize>() {
                Ok(i) if i >= 1 && i <= q.options.len() => HumanAnswer::Option(i - 1),
                _ => HumanAnswer::Text(l),
            },
        });
    }
    eprintln!("────────────────────────────────────────────────────────");
    out
}

/// Affiche l'avancement d'un run sur stderr (libellés hiérarchiques inclus). `live` suit le run pour le
/// tableau de bord : `essaim conso` montre l'organigramme et les jetons pendant la mission, même lancée ici.
fn spawn_printer(mut rx: mpsc::UnboundedReceiver<RunEvent>, auto_yes: bool, edit: Option<PlanEdit>, mut live: Option<runtime::live::Tracker>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut labels: std::collections::HashMap<String, (String, usize)> = Default::default();
        let name = |labels: &std::collections::HashMap<String, (String, usize)>, id: &str| match labels.get(id) {
            Some((l, d)) => format!("{}{l}", "  ".repeat(*d)),
            None => id.to_string(),
        };
        let (mut cached_total, mut volume_total) = (0u64, 0u64);
        while let Some(ev) = rx.recv().await {
            if let Some(t) = &mut live {
                t.on_event(&ev);
            }
            match ev {
                RunEvent::Label { id, label, depth, .. } => {
                    labels.insert(id, (label, depth));
                }
                RunEvent::Checkpoint { mut plan, reply } => {
                    let go = loop {
                        eprintln!("\n──────── Plan de la mission (écrit par le boss) ────────\n{plan}\n────────────────────────────────────────────────────────");
                        if auto_yes {
                            eprintln!("--yes : la réalisation démarre.");
                            break true;
                        }
                        eprint!("Lancer la réalisation (les postes vont modifier le projet) ? [o/N/e = modifier le plan avec le boss] ");
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
                RunEvent::HumanRequest(req) => {
                    // Demandeur déjà parti (agent arrêté pendant qu'une autre demande était à l'écran) : rien à demander.
                    if req.pending() {
                        let answers = answer_on_terminal(&req, auto_yes).await;
                        let _ = req.reply.send(answers);
                    }
                }
                RunEvent::NodeStarted { id } => eprintln!("▶ {}", name(&labels, &id)),
                RunEvent::Node { id, event: ClaudeEvent::ToolUse { name: tool, .. } } => {
                    eprintln!("  [{}] outil {tool}", name(&labels, &id).trim())
                }
                RunEvent::Node { event: ClaudeEvent::Result { volume, cached, .. }, .. } => {
                    volume_total += volume;
                    cached_total += cached;
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
                RunEvent::Paused { why, resets_at } => match why {
                    pause::Why::Limit => eprintln!(
                        "⏸ limite de l'abonnement atteinte — run en pause, reprise automatique {} (Ctrl+C pour arrêter : la mission se reprendra plus tard)",
                        pause::reset_label(resets_at, true)
                    ),
                    pause::Why::User => eprintln!("⏸ run en pause"),
                },
                RunEvent::Resumed => eprintln!("▶ reprise du run"),
                RunEvent::RunFinished { ok, tokens } => {
                    eprintln!("{} {} tokens au total", if ok { "terminé —" } else { "échec —" }, runtime::missions::with_cache(tokens, volume_total, cached_total, true))
                }
            }
        }
    })
}

/// `essaim profile` : explique les deux profils de sécurité, lit le choix (argument ou clavier) et l'enregistre.
/// Appelé par l'installeur, qui lui passe le terminal : c'est la question posée à l'installation.
async fn choose_profile(choice: Option<String>) -> Result<()> {
    use tui::settings::{self, Profile};
    let path = settings::path().context("dossier de configuration introuvable")?;
    let mut reglages = settings::load(&path);
    let lang = reglages.lang.unwrap_or_default();
    let profile = match choice {
        Some(c) => Profile::parse(&c).with_context(|| format!("profil inconnu : {c:?} (dedicated ou protected)"))?,
        None => {
            println!("{}\n", lang.t("Profil de sécurité d'essaim : ce que les agents ont le droit de faire sur cette machine.", "essaim security profile: what agents may do on this machine."));
            for (i, p) in Profile::ALL.iter().enumerate() {
                println!("{}. {} ({})\n   {}\n", i + 1, p.name(lang), p.key(), p.explain(lang));
            }
            let actuel = reglages.profile.unwrap_or_default();
            loop {
                print!("{} [1/2, {} = {}] ", lang.t("Votre choix", "Your choice"), lang.t("Entrée", "Enter"), actuel.name(lang));
                std::io::Write::flush(&mut std::io::stdout()).ok();
                match read_line().await {
                    None => {
                        println!();
                        break actuel; // pas de clavier : on garde le profil courant (protégé par défaut)
                    }
                    Some(l) if l.is_empty() => break actuel,
                    Some(l) => match Profile::parse(&l) {
                        Some(p) => break p,
                        None => println!("{}", lang.t("Répondez 1 ou 2.", "Answer 1 or 2.")),
                    },
                }
            }
        }
    };
    reglages.profile = Some(profile);
    settings::save(&path, &reglages)?;
    println!("{} : {} — {}", lang.t("Profil enregistré", "Profile saved"), profile.name(lang), path.display());
    if profile == Profile::Protected && cfg!(target_os = "linux") {
        let absent: Vec<&str> = [("bwrap", "bubblewrap"), ("socat", "socat")].iter().filter(|(bin, _)| !in_path(bin)).map(|(_, pkg)| *pkg).collect();
        if !absent.is_empty() {
            println!(
                "⚠ {} : {}. {} sudo apt-get install {} ({} sudo dnf install {}).",
                lang.t("Le bac à sable a besoin de", "The sandbox needs"),
                absent.join(", "),
                lang.t("Installez-les :", "Install them:"),
                absent.join(" "),
                lang.t("ou", "or"),
                absent.join(" ")
            );
        }
    }
    println!("{}", lang.t("Vous pouvez en changer à tout moment : `essaim profile`, ou F10 dans essaim.", "You can change it at any time: `essaim profile`, or F10 inside essaim."));
    Ok(())
}

/// Le programme `name` est-il dans le PATH ?
fn in_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        None => tui::run(std::env::current_dir()?).await,
        Some(Cmd::Init { root }) => {
            // Sans `--root` : dossier de l'utilisateur, pour que les exemples suivent partout.
            let base = match &root {
                Some(r) => r.join(".claude"),
                None => model::user_claude_dir().context("dossier personnel introuvable : précisez --root <projet>")?,
            };
            for (path, content) in EXAMPLES {
                let path = base.join(path.strip_prefix(".claude/").unwrap_or(path));
                if path.exists() {
                    println!("existe déjà : {}", path.display());
                    continue;
                }
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, content)?;
                println!("créé : {}", path.display());
            }
            if root.is_none() {
                println!("\nCes agents et cette entreprise sont à vous : essaim les retrouve depuis n'importe quel dossier.");
            }
            println!("\nLancez `essaim` : onglet Entreprise (organigramme d'exemple).");
            Ok(())
        }
        Some(Cmd::Company { cmd }) => {
            match cmd {
                CompanyCmd::Show { file } => {
                    let c = Company::load(&company::resolve(&ConfigDirs::new(std::env::current_dir()?), &file)?)?;
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
                    println!("Appels estimés (flux de base) : plan {} + réalisation {} = {}", e.conception, e.realisation, e.total());
                }
                CompanyCmd::Run { file, brief, mission, yes, resume, root } => {
                    let root = root.unwrap_or(std::env::current_dir()?).canonicalize().context("racine introuvable")?;
                    let dirs = ConfigDirs::new(&root);
                    let company = Company::load(&company::resolve(&dirs, &file)?)?;
                    let agents = agent::load_all(&dirs);
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
                        "{} — mission {mission_id} : environ {} appel(s) (plan {} + réalisation {}), plafond {}, {} en parallèle",
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
                    let reglages = tui::settings::load_default();
                    let lang = reglages.lang.unwrap_or_default();
                    if reglages.profile.is_none() {
                        eprintln!("profil de sécurité jamais choisi : « protégé » s'applique (bac à sable, demandes à l'humain). `essaim profile` pour choisir.");
                    }
                    let access = reglages.access();
                    eprintln!("profil : {}", access.profile.name(lang));
                    let edit = PlanEdit { company: company.clone(), agents: agents.clone(), root: root.clone(), mission_dir: mission_dir.clone(), lang };
                    let run_id = runtime::store::new_id(runtime::store::now_ms());
                    let titre = missions::list(&root).into_iter().find(|m| m.id == mission_id).map(|m| m.title).unwrap_or_default();
                    let live = runtime::live::Tracker::new(&root, &run_id, &company, &titre, runtime::store::now_ms());
                    let printer = spawn_printer(rx, yes, Some(edit), Some(live));
                    // Mission laissée en pause par la limite : la porte repart fermée, le run attend l'heure de reprise.
                    let gate = match missions::read_pause(&mission_dir) {
                        Some(p) if p.resets_at.is_none_or(|t| t > pause::now_s()) => {
                            eprintln!("⏸ mission en pause (limite de l'abonnement) — reprise {}", pause::reset_label(p.resets_at, true));
                            pause::Gate::closed(pause::Why::Limit, p.resets_at)
                        }
                        _ => pause::Gate::new(),
                    };
                    // Bus des demandes à l'humain : les agents y arrivent par l'outil `ask_human` de leur serveur MCP.
                    let bus = runtime::bus::Bus::start(tx.clone(), Some(mission_dir.join(missions::NOTES_DIR)), lang, gate.clone())?;
                    bus.trust_sandbox(&root);
                    let opts = CompanyOptions { base_dir: root, mission_dir: Some(mission_dir.clone()), lang, resume, pause: gate, access, bus: Some(bus.clone()) };
                    let res = run_mission(&company, &agents, &brief_text, &opts, claude_runner(std::time::Duration::from_secs(company.node_timeout_s), Some(bus.clone())), tx, cancel_rx).await;
                    // Le bus garde un émetteur d'événements : sans le lâcher, l'affichage attendrait indéfiniment.
                    drop((opts, bus));
                    let out = res?;
                    printer.await?;
                    if let Some(note) = note_refus(out.approved) {
                        eprintln!("{note}");
                    }
                    let blockers: Vec<_> = out.conception.blockers.iter().chain(out.realisation.iter().flat_map(|r| r.blockers.iter())).collect();
                    if !blockers.is_empty() {
                        eprintln!("⚠ Mission non aboutie. Ce qui reste à lever (demandes restées sans réponse comprises) :");
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
                    let dirs = ConfigDirs::new(root.unwrap_or(std::env::current_dir()?));
                    let c = Company::load(&company::resolve(&dirs, &file)?)?;
                    let names: Vec<String> = agent::load_all(&dirs).into_iter().map(|a| a.name).collect();
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
        Some(Cmd::Conso { port, no_open, root }) => {
            let root = root.unwrap_or(std::env::current_dir()?);
            let lang = tui::settings::path().map(|p| tui::settings::load(&p)).and_then(|s| s.lang).unwrap_or_default();
            let dash = runtime::web::start(root, lang, port).await?;
            println!("Tableau de bord : {}", dash.url);
            println!("La clé de l'URL n'est valable que pour ce lancement. Ctrl+C pour arrêter.");
            if !no_open {
                if let Err(e) = runtime::web::open_in_browser(&dash.url) {
                    eprintln!("⚠ navigateur non lancé ({e:#}) : ouvrez l'URL ci-dessus à la main");
                }
            }
            tokio::signal::ctrl_c().await.ok();
            eprintln!("\ntableau de bord arrêté.");
            Ok(())
        }
        Some(Cmd::Mcp { socket, node, seat, permission }) => runtime::mcp::run(socket, node, seat, permission),
        Some(Cmd::Profile { choice }) => choose_profile(choice).await,
        Some(Cmd::Agents { root }) => {
            let dirs = ConfigDirs::new(root.unwrap_or(std::env::current_dir()?));
            for a in agent::load_all(&dirs) {
                // Un agent global (dossier utilisateur) est disponible dans tous les projets : signalé par `~`.
                let ou = a.path.as_deref().filter(|p| dirs.is_global(p)).map_or("", |_| " ~");
                println!("{:<24} {}{ou}", a.name, a.description);
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
