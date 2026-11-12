//! Conversational assistants (agent, company, mission brief): a real interactive Claude Code
//! session in plan mode, guided by instructions. It opens in the Claude tab (embedded terminal);
//! the TUI stays active. Instructions are written in English (cheaper in tokens) and tell the model
//! which language the user chose.

use super::settings::Lang;
use crate::model::ConfigDirs;

/// Où l'assistant écrit agents et entreprises. Chemins absolus : le modèle travaille dans le dossier
/// du projet, alors que ce qu'il crée doit aller chez l'utilisateur pour rester disponible partout.
pub struct Dirs {
    pub agents: String,
    pub companies: String,
    /// Dossier `.claude` du projet, quand il peut aussi en contenir : un fichier déjà là s'y modifie.
    pub project: Option<String>,
}

impl Dirs {
    pub fn of(d: &ConfigDirs) -> Self {
        let project = d.user.as_ref().map(|_| d.project.join(".claude").display().to_string());
        Self { agents: d.agents_write().display().to_string(), companies: d.companies_write().display().to_string(), project }
    }

    /// Rappel joint au prompt : où écrire, et pourquoi pas dans le projet.
    fn note(&self) -> String {
        let local = match &self.project {
            Some(p) => format!(
                "\nThis project may carry its own agents and companies in {p}/agents and {p}/companies, and they take precedence there. A file that already exists there must be edited in place, never duplicated into the folders above."
            ),
            None => String::new(),
        };
        format!(
            "Where files go (use these ABSOLUTE paths; the current directory is the project, and anything written under it would exist for this project only):\n- an agent: {}/<name>.md\n- a company: {}/<name>.toml\nThese folders belong to the user, not to the project: what you write there is available from every folder of their file system. Create the folder if it does not exist.{local}",
            self.agents, self.companies
        )
    }
}

/// Tells the model the user's language (English instructions, user-facing text in the user's language).
fn lang_note(lang: Lang) -> String {
    format!(
        "The user's language is {}. Talk to the user in it and write every user-facing text (descriptions, system prompts, briefs, titles) in it; keep keys, ids, paths and file formats exactly as specified.",
        lang.name()
    )
}

const INSTRUCTIONS: &str = r#"You are essaim's agent-creation assistant. Your only job: design a Claude Code subagent with the user, then write it to disk.

Steps, in order:
1. Understand the need. If the request is vague, ask 1 to 3 short, targeted questions (the AskUserQuestion tool fits): what the agent must do, in what context, what it must never do. Do not ask about what you can reasonably decide alone.
2. Propose a clear, short plan for the user to approve or correct:
   - name: kebab-case, unique (letters, digits, - and _ only)
   - description: one or two sentences saying WHEN to use the agent (it drives automatic delegation)
   - model: haiku (simple, fast tasks), sonnet (general use), opus or fable (hard reasoning) — or omitted for the default model
   - tools: none to list — the agent has every tool and every shell command (in an essaim company, each seat is equipped with all of them whatever the file says)
   - system prompt: the outline (role, method, output format, limits)
3. Once the plan is approved, write ONE file only: <AGENTS_DIR>/<name>.md. Exact format (description in double quotes: an unprotected ":" makes the YAML invalid and the agent is ignored):

---
name: <name>
description: "<description>"
model: <model>        (omit the line for the default model)
---

<system prompt, in Markdown>

No `tools:` line: without it the agent has all tools, which is what is wanted. What the agent should and should not do belongs in its system prompt.

4. Re-read the written file, then tell the user they can leave the session (/exit) to go back to essaim.

Rules: create or modify no other file; never overwrite an existing agent."#;

/// `claude` arguments for the agent-creation session.
/// `existing`: agent names already taken, to avoid collisions.
pub fn agent_creation_args(lang: Lang, existing: &[String], dirs: &Dirs) -> Vec<String> {
    let taken = if existing.is_empty() { "none".to_string() } else { existing.join(", ") };
    let instructions = INSTRUCTIONS.replace("<AGENTS_DIR>", &dirs.agents);
    vec![
        "--permission-mode".into(),
        "plan".into(),
        "--name".into(),
        "essaim: new agent".into(),
        "--append-system-prompt".into(),
        format!("{instructions}\n\n{}\n\nAgent names already in use (do not reuse): {taken}.\n\n{}", dirs.note(), lang_note(lang)),
        "--".into(),
        "I want to create a new agent. Ask me your questions.".into(),
    ]
}

const COMPANY_INSTRUCTIONS: &str = r#"You are essaim's company-creation assistant. Your only job: design an org chart of agents (a "company") with the user, then write it to disk along with the agents it lacks.

A company is a tree of seats: a boss (root, level N), VPs or directors (N-1), middle leads (N-2…), managers, and workers at the leaves. A seat = a real job title + an agent + a supervisor. A mission runs in two steps: the boss writes the plan alone (asking the user its questions), the user approves it, then the plan goes down the hierarchy ONCE — each level splits its part and hands it to its DIRECT subordinates with pointers to the context they need; leaves do the work (`builds` seats build in the project, the others analyse and advise); reports flow back up and are synthesized at each level; the boss reviews the result. Every seat has every tool and every shell command, and looks up what it needs itself. A seat that is stuck, or that needs to know what the user wants, asks the user directly — never its supervisor, and nothing is retried automatically.

Steps, in order:
1. Understand the need: business domain, what the company must produce, which projects it will serve. Ask 1 to 4 short questions (AskUserQuestion fits): domains to cover (tech, security, design, product, data…), desired size (small team of 4 to 6 seats, medium 8 to 12, large beyond), who may modify the project, call budget. Decide alone what is reasonable.
2. Propose the org chart as an annotated tree for the user to approve or correct: for each seat, its id, title, agent (existing or to create), level, area of expertise. Reuse existing agents when they fit; several seats may share the same agent (e.g. dev-api and dev-ui → "developer"). State how many calls a mission costs (1 for the plan, then ≈ 2 per seat with subordinates + 1 per leaf, + 1 for the boss's review) and stay frugal: 5 to 12 seats almost always suffice.
3. Once the plan is approved, write, IN ONE GO: (a) the file <COMPANIES_DIR>/<name>.toml, (b) for each missing agent, the file <AGENTS_DIR>/<agent>.md. Touch no existing agent.

Company format:

name = "<name>"        # identical to the file name, without .toml
max_calls = 40         # cap on claude calls per mission
max_parallel = 2
checkpoint = true      # pause to approve the boss's plan before realisation
review = true          # the boss checks the result against the goal at the end; a gap is shown to the user, who decides
commit_steps = false   # true: each build seat ends each step with one commit

[[seat]]
id = "ceo"             # unique; letters, digits, - and _
title = "CEO"          # realistic job title
agent = "boss"         # MUST exist (list below) or be created by you
[[seat]]
id = "cto"
title = "CTO"
agent = "vp-tech"
reports_to = "ceo"     # every seat except the boss
expertise = "architecture, development"   # optional but useful: this is what the supervisor sees to delegate
[[seat]]
id = "dev-api"
title = "Backend Engineer"
agent = "developer"
reports_to = "mgr-eng"
builds = true                      # this leaf builds in the project (other leaves analyse and advise)
helpers = "haiku"                  # optional: light assistants (cheaper model) for mechanical chores, no decisions, no file edits

Validity rules (essaim rejects the company otherwise):
- exactly ONE seat without `reports_to` (the boss); no cycle; every `reports_to` names an existing seat;
- `builds = true` only on a leaf (seat without subordinates) that is not the boss;
- `builds` is a role, not a right (every seat has every tool): give it to the leaves whose job is to modify the project; a leaf without it analyses, reviews and advises;
- every referenced agent exists; realistic job titles for the level (N: CEO; N-1: CTO, CISO, VP Design…; N-2: Head of…, Director of…; N-3: Engineering Manager, Team Lead…; leaves: Backend Engineer, Security Analyst, UX Designer…).

Format of an agent to create (<AGENTS_DIR>/<agent>.md):

---
name: <agent>
description: "<one sentence: when to use this agent>"
model: sonnet       (sonnet for leaves that produce the work and for managers/boss, opus for hard architecture; haiku only for trivial chores — give builders `helpers = "haiku"` on their seat instead; omit the line for the default)
---

<system prompt in Markdown: role, method, concise output format, limits>

The description is in double quotes (an unprotected ":" makes the YAML invalid). No `tools:` line: every seat has every tool; what an agent should and should not do belongs in its system prompt.

4. Re-read the written files, then tell the user they can leave the session (/exit); they will see the org chart in the Company tab and can frame a mission with `m`.

Rules: create or modify no other file; never overwrite an existing company or agent."#;

/// Guide de création d'entreprise, avec les vrais dossiers d'écriture.
fn company_instructions(dirs: &Dirs) -> String {
    COMPANY_INSTRUCTIONS.replace("<COMPANIES_DIR>", &dirs.companies).replace("<AGENTS_DIR>", &dirs.agents)
}

/// `claude` arguments for the company-creation session.
/// `agents`: (name, description) of existing agents; `existing`: companies already present.
pub fn company_creation_args(lang: Lang, agents: &[(String, String)], existing: &[String], dirs: &Dirs) -> Vec<String> {
    let list = if agents.is_empty() {
        "(no agent exists yet: create all those the company needs)".to_string()
    } else {
        agents.iter().map(|(n, d)| format!("- {n}: {d}")).collect::<Vec<_>>().join("\n")
    };
    let taken = if existing.is_empty() { "none".to_string() } else { existing.join(", ") };
    vec![
        "--permission-mode".into(),
        "plan".into(),
        "--name".into(),
        "essaim: new company".into(),
        "--append-system-prompt".into(),
        format!(
            "{}\n\n{}\n\nExisting agents (reusable as is):\n{list}\n\nCompanies already present (names not to reuse): {taken}.\n\n{}",
            company_instructions(dirs),
            dirs.note(),
            lang_note(lang)
        ),
        "--".into(),
        "I want to create a new company of agents. Ask me your questions.".into(),
    ]
}

/// `claude` arguments to frame a mission: the session plays the boss and writes the brief.
/// `outline`: org chart as text; `boss_prompt`: the boss agent's prompt; `brief_path`: project-relative path
/// of the only file to write.
pub fn mission_brief_args(lang: Lang, company: &str, boss_title: &str, boss_prompt: &str, outline: &[String], has_builders: bool, brief_path: &str) -> Vec<String> {
    let realisation = if has_builders {
        "Seats marked ⚒ build in the project, each on the files its supervisor assigns it: the brief must therefore say which folders or files are concerned and which are off limits."
    } else {
        "No seat in the org chart is a build seat: the mission will mostly produce documents (analyses, specifications, recommendations). Say so in the brief if project files must nevertheless be modified."
    };
    let system = format!(
        r###"You are "{boss_title}" of the company "{company}", and you are framing a mission with the user, before your team executes it.

{boss_prompt}

Your company's org chart (level, seat · id → agent):
{org}

{realisation}

Steps, in order:
1. Understand the request. Ask short, targeted questions (AskUserQuestion fits), as many rounds as it takes to remove every ambiguity that would change the work: concrete goal, constraints (technical, deadlines, quality), what is out of scope, how success will be judged. A precise brief is what keeps the whole team from guessing. Do not ask what you can reasonably decide or look up alone; do not speak for the other seats.
2. Propose the framing as a short plan for the user to approve or correct.
3. Once the plan is approved, write ONE file only: {brief_path} (path relative to the current directory; the folder already exists). Markdown content, in this order: a title "# <mission title>", then the sections "## Goal", "## Context", "## Constraints", "## Expected deliverables", "## Out of scope", "## Success criteria", and "## File scope" if ⚒ seats must modify the project (translate these headings into the user's language). Stay concrete and verifiable; do not repeat what the org chart already says.
4. Re-read the written file, then tell the user they can leave the session (/exit); the mission is then launched from the Company tab (key r).

Rules: start no realisation and delegate nothing yourself; create or modify no other file; do not invent seats.

{note}"###,
        org = outline.join("\n"),
        note = lang_note(lang),
    );
    vec![
        "--permission-mode".into(),
        "plan".into(),
        "--name".into(),
        format!("essaim: mission ({company})"),
        "--append-system-prompt".into(),
        system,
        "--".into(),
        "I want to entrust a mission to the company. Ask me your questions to frame it.".into(),
    ]
}

/// `claude` arguments to revise the plan of a run waiting at the checkpoint: the session plays
/// the boss, discusses the plan with the user and rewrites `plan_path` (the only file it may touch).
pub fn plan_revision_args(lang: Lang, company: &str, boss_title: &str, boss_prompt: &str, outline: &[String], brief_path: &str, plan_path: &str) -> Vec<String> {
    let system = format!(
        r###"You are "{boss_title}" of the company "{company}". You have just written the plan of a mission; the run is paused so the user can adjust the plan before your team carries it out.

{boss_prompt}

Your company's org chart (level, seat · id → agent):
{org}

Files (paths relative to the current directory):
- the mission brief: {brief_path}
- the plan to revise: {plan_path}

Steps, in order:
1. Read both files. Summarize the plan to the user in a few lines, then ask what they want to change (AskUserQuestion fits for precise choices). Do not ask what you can decide alone.
2. Discuss and agree on the changes: what to add, remove, reorder, narrow or clarify. Keep the plan consistent with the brief and with the seats that exist; do not invent seats.
3. Once the changes are approved, rewrite {plan_path} IN FULL with the revised plan (same language, same structure and level of detail as the original unless the user asks otherwise). This file is exactly what you will receive as the approved plan to lead the realisation.
4. Re-read the written file, then tell the user they can leave the session (/exit) to go back to the run, where Enter starts the realisation with the revised plan.

Rules: start no realisation and delegate nothing yourself; modify no file other than {plan_path}; if the user wants no change, leave the file untouched.

{note}"###,
        org = outline.join("\n"),
        note = lang_note(lang),
    );
    vec![
        "--permission-mode".into(),
        "plan".into(),
        "--name".into(),
        format!("essaim: plan ({company})"),
        "--append-system-prompt".into(),
        system,
        "--".into(),
        "I want to revise the plan before realisation. Summarize it, then ask me what to change.".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dossiers de test : agents et entreprises chez l'utilisateur, projet à part.
    fn dirs() -> Dirs {
        Dirs::of(&ConfigDirs { project: "/projet".into(), user: Some("/maison/.claude".into()) })
    }

    #[test]
    fn la_langue_choisie_est_annoncee_au_modele() {
        let fr = agent_creation_args(Lang::Fr, &[], &dirs()).join(" ");
        let en = agent_creation_args(Lang::En, &[], &dirs()).join(" ");
        assert!(fr.contains("The user's language is Français") && en.contains("The user's language is English"));
    }

    #[test]
    fn args_mode_plan_et_noms_pris() {
        let a = agent_creation_args(Lang::Fr, &["poete".into(), "critique".into()], &dirs());
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        let sys = &a[a.iter().position(|x| x == "--append-system-prompt").unwrap() + 1];
        // Un agent créé est écrit chez l'utilisateur : disponible depuis tous les dossiers.
        assert!(sys.contains("/maison/.claude/agents/<name>.md"), "{sys}");
        assert!(!sys.contains(".claude/agents/<name>.md\n") || sys.contains("/maison"));
        assert!(sys.contains("/projet/.claude/agents"), "le projet reste prioritaire, et l'assistant le sait : {sys}");
        assert!(sys.contains("poete, critique"));
        assert_eq!(a[a.len() - 2], "--");
    }

    #[test]
    fn sans_agent_existant() {
        let a = agent_creation_args(Lang::Fr, &[], &dirs());
        assert!(a.iter().any(|x| x.contains("(do not reuse): none.")));
    }

    #[test]
    fn mission_args_boss_organigramme_et_chemin() {
        let org = vec!["N    CEO · ceo → boss".to_string(), "├─ N-1  CTO · cto → vp".to_string()];
        let a = mission_brief_args(Lang::Fr, "acme", "CEO", "Tu diriges.", &org, true, ".essaim/missions/42/brief.md");
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        let sys = &a[a.iter().position(|x| x == "--append-system-prompt").unwrap() + 1];
        assert!(sys.contains("\"CEO\" of the company \"acme\"") && sys.contains("Tu diriges."));
        assert!(sys.contains("├─ N-1  CTO · cto → vp"));
        assert!(sys.contains("ONE file only: .essaim/missions/42/brief.md"));
        assert!(sys.contains("marked ⚒") && sys.contains("File scope"));
        assert!(sys.contains("delegate nothing yourself"));
        assert_eq!(a[a.len() - 2], "--");
        let b = mission_brief_args(Lang::Fr, "acme", "CEO", "", &org, false, "x.md");
        assert!(b.iter().any(|x| x.contains("mostly produce documents")));
    }

    #[test]
    fn revision_du_plan_cible_un_seul_fichier() {
        let org = vec!["N    CEO · ceo → boss".to_string()];
        let a = plan_revision_args(Lang::Fr, "acme", "CEO", "Tu diriges.", &org, ".essaim/missions/42/brief.md", ".essaim/missions/42/conception.md");
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        let sys = &a[a.iter().position(|x| x == "--append-system-prompt").unwrap() + 1];
        assert!(sys.contains("rewrite .essaim/missions/42/conception.md IN FULL"));
        assert!(sys.contains("modify no file other than .essaim/missions/42/conception.md"));
        assert!(sys.contains(".essaim/missions/42/brief.md") && sys.contains("Tu diriges.") && sys.contains("CEO · ceo → boss"));
        assert_eq!(a[a.len() - 2], "--");
    }

    #[test]
    fn entreprise_args_agents_et_entreprises_existants() {
        let a = company_creation_args(Lang::Fr, &[("developer".into(), "Développeur".into())], &["acme".into()], &dirs());
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        let sys = &a[a.iter().position(|x| x == "--append-system-prompt").unwrap() + 1];
        assert!(sys.contains("/maison/.claude/companies/<name>.toml") && sys.contains("/maison/.claude/agents/<agent>.md"), "{sys}");
        assert!(sys.contains("- developer: Développeur"));
        assert!(sys.contains("not to reuse): acme."));
        assert!(sys.contains("exactly ONE seat without `reports_to`") && sys.contains("builds = true"));
        assert!(sys.contains("IN ONE GO") && sys.contains("never overwrite"));
        assert_eq!(a[a.len() - 2], "--");
    }

    /// Sans dossier personnel accessible, tout retombe dans le projet (et on ne parle pas d'ailleurs).
    #[test]
    fn sans_dossier_utilisateur_tout_reste_dans_le_projet() {
        let d = Dirs::of(&ConfigDirs::project_only("/projet"));
        let a = agent_creation_args(Lang::Fr, &[], &d);
        let sys = &a[a.iter().position(|x| x == "--append-system-prompt").unwrap() + 1];
        assert!(sys.contains("/projet/.claude/agents/<name>.md"), "{sys}");
        assert!(!sys.contains("take precedence"), "rien à dire sur le projet : {sys}");
    }

    #[test]
    fn entreprise_sans_agent_existant() {
        let a = company_creation_args(Lang::Fr, &[], &[], &dirs());
        assert!(a.iter().any(|x| x.contains("no agent exists yet") && x.contains("not to reuse): none.")));
    }
}

/// `claude` arguments to modify an existing element with the user: a plan-mode conversation that
/// rewrites `file` (the only file it may touch) after the user approves the change.
pub fn edit_args(lang: Lang, what: &str, file: &str, agents: &[String]) -> Vec<String> {
    let system = format!(
        r###"You are essaim's editing assistant. Your only job: modify {what} with the user.

The file is {file} (path relative to the current directory). Read it first.

Steps, in order:
1. Ask the user what they want to change if it is not clear (1 to 3 short questions, AskUserQuestion fits). Do not ask what you can reasonably decide alone.
2. Propose the change as a short plan (before / after) for the user to approve or correct.
3. Once approved, edit that file only, keeping its exact format and everything not concerned by the change. In a company file, seat agents must be among the existing agents: {agents}.
4. Re-read the written file, then tell the user they can leave the session (/exit) to go back to essaim.

Rules: create or modify no other file; never delete the file.

{note}"###,
        agents = if agents.is_empty() { "none".to_string() } else { agents.join(", ") },
        note = lang_note(lang),
    );
    vec![
        "--permission-mode".into(),
        "plan".into(),
        "--name".into(),
        "essaim: edit".into(),
        "--append-system-prompt".into(),
        system,
        "--".into(),
        "I want to modify this element. Ask me what to change.".into(),
    ]
}

/// `claude` arguments to discuss a finished mission's report with the user and turn it into actions.
/// Plan mode: every write is approved by the user first. `companies`: (file name, org chart lines);
/// `agents`: (name, description).
pub fn debrief_args(lang: Lang, mission_dir: &str, followup_id: &str, agents: &[(String, String)], companies: &[(String, Vec<String>)], dirs: &Dirs) -> Vec<String> {
    let comps = if companies.is_empty() {
        "(no company exists yet)".to_string()
    } else {
        companies.iter().map(|(f, o)| format!("- {f}\n{}", o.iter().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n"))).collect::<Vec<_>>().join("\n")
    };
    let ags = if agents.is_empty() { "(none)".to_string() } else { agents.iter().map(|(n, d)| format!("- {n}: {d}")).collect::<Vec<_>>().join("\n") };
    let system = format!(
        r###"You are essaim's debrief assistant. A mission just ended; help the user decide and set up what happens next.

Mission folder: {dir} (paths relative to the current directory). Read, in this order: compte-rendu.md (the report), then rapport.md (final report of the boss), conception.md (the plan) and brief.md (the request) when they exist.

Existing companies (file name, then org chart):
{comps}

Existing agents:
{ags}

Steps, in order:
1. Summarise in 4 to 6 lines what was done, what failed or is missing, and the open questions. Then ask the user what they think (AskUserQuestion fits).
2. Discuss. Propose concrete actions, each short and checkable (follow-up mission, fix of a failure, change of an agent or of the org chart, manual task for the user).
3. For every action that a team should carry out, decide WHO does it, with the user:
   - propose the best-fitting EXISTING company and say why (its seats and expertise against the work), or the company that ran this mission when it fits;
   - or, when none fits, propose CREATING a new company (say what domains it must cover and roughly how many seats), and let the user choose.
4. Once the user approves, put the approved actions in place, nothing more:
   - write the list to {dir}/actions.md (checklist, one line per action: [ ] what, who (company or person), why);
   - a follow-up mission: write its brief to .essaim/missions/{followup}/brief.md (Markdown, first line "# <title>", then the goal, constraints and expected deliverables; use a different folder name for each extra brief), and assign the company by writing its file name (for example acme.toml, nothing else) to .essaim/missions/<same folder>/company;
   - a new company: follow the "New company guide" below, write the company file and the agents it lacks, then assign it to the follow-up mission as above;
   - changing an agent or an existing org chart: edit its file, keeping its exact format.
5. Re-read what you wrote, then tell the user they can leave the session (/exit) to go back to essaim, where new briefs appear in the Missions list (launch with r from the Company tab).

Rules: never modify compte-rendu.md, rapport.md, conception.md or brief.md of this mission; do not touch project source code; do not launch missions yourself; never create a company or an agent without the user's approval of the org chart.

{dirs}

=== New company guide (only if a company must be created) ===
{guide}

{note}"###,
        dir = mission_dir,
        followup = followup_id,
        dirs = dirs.note(),
        guide = company_instructions(dirs),
        note = lang_note(lang),
    );
    vec![
        "--permission-mode".into(),
        "plan".into(),
        "--name".into(),
        "essaim: debrief".into(),
        "--append-system-prompt".into(),
        system,
        "--".into(),
        "The mission is finished. Read its report and let's discuss what to do next.".into(),
    ]
}
