//! Construction des prompts et de la carte de poste (fonctions pures, sans I/O).

use super::Phase;
use crate::model::company::Company;
use super::{Question, Section};
use crate::runtime::textutil::truncate_middle;
use crate::tf;
use crate::tui::settings::Lang;

/// Consigne de choix du modèle, ajoutée à tout prompt qui produit des affectations.
pub(super) const MODEL_ADVICE: &str = " Per assignment you may set `model`: \"haiku\" for mechanical or simple work (inventories, checks, small well-specified edits), \"sonnet\" for analysis or code needing judgment, \"opus\" only for hard architecture or subtle reasoning. Pick the cheapest level that does the task well; when in doubt for code that must be correct, \"sonnet\". On a leaf it is its own model; on a manager, the default for the leaves below. Seats marked `model pinned` keep their model; omit `model` to keep the default.";

/// Un résultat d'enfant tronqué au-delà de cette taille dans le prompt de rapport du parent.
pub(super) const MAX_CHILD_CHARS: usize = 6000;

/// La tâche confiée à un subordonné, rappelée dans les prompts de rapport, correction et revue : début+fin
/// au-delà de cette taille (le plan et la tâche d'origine ne sont jamais tronqués).
pub(super) const MAX_SUBTASK_CHARS: usize = 1500;

/// Le run a été mis en pause, l'agent s'est arrêté avec un bilan, et la pause est levée : il reprend sa
/// session (`--resume`, contexte conservé) là où il s'était arrêté. On lui rappelle sa tâche d'origine et
/// son propre bilan, parce que c'est lui qui a décidé où couper.
pub(super) fn resume_after_pause_prompt(task: &str, note: &str) -> String {
    let mut out = String::from(
        "The run was paused and you stopped on request. It has resumed: continue your task from where you left off.\n\n",
    );
    if !note.trim().is_empty() {
        out.push_str(&format!("The handover you wrote when you stopped:\n{}\n\n", truncate_middle(note.trim(), MAX_CHILD_CHARS)));
    }
    out.push_str("Here is the task you were given, unchanged:\n");
    out.push_str(task);
    out.push_str("\n\nDo not redo what you already completed. Finish the rest, then answer through the structured output as originally asked.");
    out
}

/// Suite ajoutée au prompt d'un nouvel essai : dit pourquoi le précédent a été écarté.
pub(super) fn retry_note(cause: &str) -> String {
    format!("\n\nYour previous answer was rejected: {}. Answer again strictly through the structured output, following its schema.", truncate_middle(cause, 300))
}

/// Seat card appended to the system prompt of every call. Written in English (cheaper in tokens);
/// the user's language tells the model what to write in.
/// Order matters for the prompt cache, which matches on the longest identical prefix: what is shared
/// by the whole company (phase, rules) comes first, what is specific to the seat comes last.
///
/// **Le brief ne va qu'au boss.** C'est tout l'intérêt d'une hiérarchie : le boss digère la demande et
/// n'en descend que la part utile, chaque supérieur affinant pour ses subordonnés. Un poste reçoit donc
/// pour seule référence la tâche écrite par son supérieur. Deux effets : le contexte d'un poste reste
/// petit (le brief est relu à chaque tour d'outil, pas une seule fois), et un poste ne peut pas sortir
/// de son périmètre en s'autorisant lui-même au nom de l'objectif général.
pub(super) fn role_card(lang: Lang, c: &Company, i: usize, brief: &str, phase: Phase, kids: &[usize], scope: &[String]) -> String {
    let s = &c.seats[i];
    // Phase et règles d'abord : elles sont identiques pour tous les postes, donc elles forment le plus
    // long préfixe commun possible, celui que le cache de prompt sert d'une session à l'autre.
    let mut out = String::new();
    match phase {
        Phase::Conception => out.push_str(
            "DESIGN PHASE: we think and prepare, we build NOTHING. Produce only analysis, specifications, decisions and recommendations for the mission (nothing is created or modified in the project yet; this is normal). Never report as a failure or a gap that the mission's result does not exist yet: it is set up later by the realisation team, after the plan is approved.\n",
        ),
        Phase::Realisation => out.push_str("REALISATION PHASE: the plan is approved, we now build in the project.\n"),
    }
    out.push_str(&format!(
        "\nRules: be concise and actionable; stay within your seat's remit; clearly flag what is missing or what really failed. Never claim a success you could not verify: when you cannot complete your task, say so (status blocked) with the exact cause and what is needed, instead of working around it. The user's language is {}: write everything you produce in it.\n",
        lang.name()
    ));
    // Le brief au boss seulement ; aux autres, une ligne qui dit que leur tâche est toute leur référence.
    // Volontairement courte : elle est dans le prompt système, donc relue à chaque tour d'outil.
    if c.root() == Some(i) {
        out.push_str(&format!("\nCompany mission (brief framed with the leader):\n{brief}\n"));
    } else {
        out.push_str(
            "\nYou do not see the company brief: your supervisor's task is your whole spec. Do not widen it; if something is missing, say so rather than guess.\n",
        );
    }
    let mut chain: Vec<String> = c.chain_of_command(i).iter().rev().map(|&p| c.seats[p].title.clone()).collect();
    chain.push(format!("{} (you)", s.title));
    out.push_str(&format!(
        "\nYou hold the seat \"{}\" ({}) in the company \"{}\", under the identity of the agent \"{}\".\nChain of command: {}.\nCurrent phase: {}.\n",
        s.title,
        c.level(i),
        c.name,
        s.agent,
        chain.join(" → "),
        phase.name()
    ));
    if !kids.is_empty() {
        out.push_str("Your direct subordinates (you delegate only to them):\n");
        for &k in kids {
            let ks = &c.seats[k];
            let expertise = ks.expertise.clone().unwrap_or_default();
            let tools = match (ks.builds, ks.allow_bash.is_empty()) {
                (false, _) => String::new(),
                (true, true) => " · builds".to_string(),
                (true, false) => format!(" · builds, shell: {}", ks.allow_bash.join(", ")),
            };
            let pinned = ks.model.as_ref().map(|m| format!(" · model pinned: {m}")).unwrap_or_default();
            out.push_str(&format!("- {} · {}{}{tools}{pinned}\n", ks.id, ks.title, if expertise.is_empty() { String::new() } else { format!(" · {expertise}") }));
        }
    }
    if !kids.is_empty() {
        out.push_str(&format!(
            "You coordinate: you never write or modify files yourself, but you can read the project and verify results with these shell commands: {}.\n",
            c.verify_commands(i).join(", ")
        ));
    }
    if let Some(m) = &s.helpers {
        out.push_str(&format!(
            "You have light assistants (subagent \"{}\", model {m}): hand them only mechanical, fully specified chores that take no decision (listing or counting files, running a command and returning its raw output, extracting lines, moving a block you point out); they never edit files. You stay responsible: decide yourself, give precise instructions, and verify what they return.\n",
            super::shared::HELPER
        ));
    }
    if phase == Phase::Realisation && !scope.is_empty() {
        out.push_str(&format!("File scope assigned to you: {}.\n", scope.join(", ")));
        if s.builds {
            out.push_str("You may only create or modify files within this scope (any other write will be refused).\n");
            if !s.allow_bash.is_empty() {
                out.push_str(&format!("Allowed shell commands: {}.\n", s.allow_bash.join(", ")));
            }
        }
    }
    out
}

pub(super) fn delegate_prompt(task: &str, phase: Phase, scope: Option<&[String]>) -> String {
    let mut out = format!(
        "Task received from your supervisor:\n{task}\n\nSplit it among your direct subordinates: involve only those who are useful, and give each a self-contained, precise task that does not overlap with the others.\n\nYour subordinates see neither the brief nor the task you received: what you write is ALL they get. So each task must stand alone — objective, the constraints that apply, how to tell it is done — and carry only what that subordinate needs. Short and complete keeps their work focused; vague makes them read half the project to guess."
    );
    if phase == Phase::Conception {
        out.push_str(" In design, give ANALYSIS or SPECIFICATION tasks (what will need doing and how), never execution tasks.");
    } else {
        // Une session longue coûte tours × contexte moyen, donc son coût croît avec le carré de sa
        // durée : six étapes dans une seule session coûtent plusieurs fois les mêmes six en six sessions.
        out.push_str(
            " One assignment = ONE step, small enough to end in a single coherent commit. If a subordinate's work needs several successive steps, give it SEVERAL assignments, one per step, in order: they run one after the other, each starting from a clean session. Never pack a sequence of steps into one assignment — a session that accumulates everything costs several times the same work split into steps.",
        );
    }
    if let Some(sc) = scope {
        out.push_str(&format!(
            "\n\nYour file scope: `{}`. For each subordinate, give a `scope`: a list of project-relative file patterns (e.g. `src/api/**`, `docs/guide.md`), ALL included in yours. Two subordinates whose scopes overlap run one after the other: prefer disjoint scopes.",
            sc.join("`, `")
        ));
    }
    out.push_str(" For each subordinate also give `sections`: the answer template you want back, 2 to 6 sections (`name`, `expect` = what it must contain, `max_chars` 100 to 1000). You decide what you need, so make it match your own plan for using the answer; keep it tight.");
    out.push_str(MODEL_ADVICE);
    out.push_str(" Answer only through the structured output.");
    out
}

/// Gabarit de réponse imposé par le délégant (vide si réponse libre).
fn sections_hint(sections: &[Section]) -> String {
    if sections.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nYour supervisor wants the answer in these sections (one structured field each). The size is a budget: condense, keep the substance, never pad — anything past it is cut, so put what matters first and last.\n",
    );
    for s in sections {
        out.push_str(&format!("- `{}` ({}, about {} chars): {}\n", s.key, s.title, s.max_chars, s.expect));
    }
    out
}

/// `step` : (numéro, total) quand le poste enchaîne plusieurs étapes — il ne voit que l'étape en cours,
/// dans une session neuve, et on lui dit où il en est pour qu'il ne refasse ni n'anticipe rien.
pub(super) fn work_prompt(task: &str, phase: Phase, sections: &[Section], step: Option<(usize, usize)>, git: bool) -> String {
    let end = " Answer only through the structured output: status \"done\" ONLY if the task is really complete (verified as far as your tools allow) with every section filled; otherwise \"blocked\" with `blockers` (kind, exact cause, what is needed). Never report \"done\" with a known problem. If you lack information or a decision only your supervisor can give, do not guess: status \"question\" with one short `question` (kind, text, optional options); you will be resumed with the answer. For a shell command you may not run, use kind \"permission\" with the exact `command`: your supervisor decides, and a command that modifies the system is allowed for one single use, never repeated or varied without asking again.";
    let (sec, rep) = (sections_hint(sections), if sections.is_empty() { "`report`" } else { "the sections" });
    // Étape d'une série : la session est neuve, donc le poste ignore ce qui précède. Il faut lui dire
    // qu'il ne doit ni refaire ni anticiper, sinon il relit le projet pour « vérifier » les étapes faites.
    let head = match step {
        Some((n, m)) => format!("Step {n} of {m} of what your supervisor assigned you. The earlier steps are already done: do not redo them, do not audit them, and do not start the later ones — this step alone is your task.\n\n"),
        None => String::new(),
    };
    match phase {
        Phase::Conception => format!("{head}Task:\n{task}{sec}\n\nProduce your deliverable as text (analysis, specification, recommendations) in {rep}. You do not have to modify the project; use \"blocked\" only if you cannot produce your analysis (missing access or information).{end}"),
        Phase::Realisation => {
            // Déplacer du code en réécrivant le fichier entier coûte plusieurs fois sa taille : un Edit
            // transporte l'ancien ET le nouveau texte, et tout cela est relu à chaque tour suivant.
            let moves = if git {
                " To relocate code, use `git mv` then narrow edits; never rewrite a whole file just to move code."
            } else {
                " Never rewrite a whole file just to move or re-indent code: make narrow, targeted edits."
            };
            let commit = if git && step.is_some() { " End this step with one commit covering exactly it." } else { "" };
            format!("{head}Task:\n{task}{sec}\n\nCarry it out in the project, only within your scope, verify it with the commands you are allowed (tests, build), then summarize precisely what you did in {rep}.{moves}{commit}\n\nIf a tool or command is refused, a test fails or a prerequisite is missing: first fix what is within your scope; if you cannot, STOP and report it as blocked. Do not silently work around it and do not continue as if it worked.{end}")
        }
    }
}

/// Bloc « tâche d'origine + résultats », identique octet pour octet dans le rapport et la revue : leur début
/// commun est ainsi servi par le cache de prompt. `results`: (title, id, assigned task, success, text).
fn results_block(task: &str, results: &[(String, String, String, bool, String)]) -> String {
    let mut out = format!("Original task your supervisor gave you:\n{task}\n\nResults from your subordinates:\n");
    for (title, id, sub_task, ok, text) in results {
        let t = truncate_middle(text, MAX_CHILD_CHARS);
        out.push_str(&format!(
            "\n### {title} ({id}){}\nAssigned task: {}\n{}\n",
            if *ok { "" } else { " — FAILED" },
            truncate_middle(sub_task, MAX_SUBTASK_CHARS),
            if *ok { t } else { format!("[FAILED: {t}]") }
        ));
    }
    out
}

pub(super) fn report_prompt(task: &str, results: &[(String, String, String, bool, String)], sections: &[Section]) -> String {
    let mut out = results_block(task, results);
    out.push_str(&sections_hint(sections));
    out.push_str("\nWrite your summary report for your supervisor: conclusions, decisions, points of attention, missing elements. Explicitly mention any FAILURE and its impact. Answer through the structured output: status \"done\" only if every part of the task is really complete and verified (a subordinate's problem that was fixed in a later result counts as resolved); otherwise \"blocked\" with the remaining `blockers`.");
    out
}

/// `failed`: (title, id, assigned task, ok = false, texte avec blocages).
pub(super) fn fix_prompt(task: &str, failed: &[(String, String, String, bool, String)], scope: Option<&[String]>, decision: Option<&str>) -> String {
    let mut out = format!("Original task your supervisor gave you:\n{task}\n\nThese subordinates could NOT complete their work:\n");
    for (title, id, sub_task, _, text) in failed {
        let t = truncate_middle(text, MAX_CHILD_CHARS);
        out.push_str(&format!("\n### {title} ({id})\nAssigned task: {}\n[{t}]\n", truncate_middle(sub_task, MAX_SUBTASK_CHARS)));
    }
    out.push_str("\nYou are responsible for getting this fixed, properly. Write a short correction plan in `plan` (root cause, then the steps). Then give corrective, self-contained tasks in `assignments` to the subordinate(s) best placed to fix it — they see neither the brief nor this message, so state the objective and how to tell it is fixed: possibly a different one than who failed (e.g. the one that has the missing tool or permission, or the specialist team). If nobody in your branch can fix it (a permission or tool nobody has, a prerequisite that does not exist, a decision that is not yours), assign nothing and explain precisely in `escalate` what is needed: your supervisor will take over. Do not play the problem down.");
    if let Some(sc) = scope {
        out.push_str(&format!("\n\nYour file scope: `{}`. Each assignment needs a `scope` (project-relative patterns, ALL included in yours).", sc.join("`, `")));
    }
    if let Some(d) = decision.filter(|d| !d.trim().is_empty()) {
        out.push_str(&format!("\n\nDecision from the user who requested the mission (follow it):\n{}", d.trim()));
    }
    out.push_str(MODEL_ADVICE);
    out.push_str("\n\nAnswer only through the structured output.");
    out
}

/// Revue du CEO : le résultat atteint-il l'objectif ? `results` : mêmes rangées que pour le rapport.
pub(super) fn review_prompt(task: &str, results: &[(String, String, String, bool, String)], scope: Option<&[String]>, rounds_left: u32) -> String {
    let mut out = results_block(task, results);
    out.push_str("\nYou are accountable for the result reaching the goal, as cleanly as possible. Review it against the success criteria of the brief: check it yourself with the commands you have (tests, build, git diff, grep, wc…) instead of trusting reports. Treat any subordinate statement of failure, partial result, missing deliverable or unmeasured criterion as unmet unless a later result clearly resolved it. You coordinate and verify, you never do the work yourself.\n- Goal fully reached and verified: `accepted` true, with a one-line `findings`.\n- Otherwise: `accepted` false, `findings` = exactly what is missing or wrong, and `assignments` = complementary or corrective tasks (even ones the user did not foresee) for the subordinates best placed to do them. If nobody in your branch can do it, assign nothing: `findings` explains what is needed and the user will be asked.");
    if rounds_left <= 1 {
        out.push_str(" This is your last review round: do not ask for more than what is needed to reach the goal.");
    }
    if let Some(sc) = scope {
        out.push_str(&format!("\n\nYour file scope: `{}`. Each assignment needs a `scope` (project-relative patterns, ALL included in yours).", sc.join("`, `")));
    }
    out.push_str(MODEL_ADVICE);
    out.push_str("\n\nAnswer only through the structured output.");
    out
}

/// Texte de l'escalade vers l'utilisateur : ce qui bloque, par poste, et pourquoi le CEO ne peut pas régler.
pub(super) fn escalation_summary(problems: &[(String, String)], reason: Option<&str>) -> String {
    let mut out = tf!("Le CEO n'a pas pu lever ces blocages :", "The CEO could not clear these blockers:").to_string();
    for (title, text) in problems {
        out.push_str(&format!("\n\n### {title}\n{text}"));
    }
    if let Some(r) = reason.filter(|r| !r.trim().is_empty()) {
        out.push_str(&format!("\n\n{}\n{}", tf!("Raison de la remontée :", "Why it was escalated:"), r.trim()));
    }
    out
}

/// Prompt d'un supérieur à qui un subordonné pose une question : réponse courte ou aveu d'ignorance.
pub(super) fn answer_prompt(who: &str, task_prompt: &str, q: &Question) -> String {
    let mut out = format!("Your subordinate \"{who}\" is working on this task and asks you a question.\n\nTheir task:\n{}\n\nQuestion ({}): {}\n", truncate_middle(task_prompt, 1500), q.kind, q.text);
    if !q.options.is_empty() {
        out.push_str(&format!("Options they suggest: {}\n", q.options.join(" | ")));
    }
    out.push_str("\nAnswer in a few lines, decisively: you may read the project to check. If the answer is not yours to give (it needs your own supervisor or the user), set `can_answer` false. Answer only through the structured output.");
    out
}

/// Demande de commande shell d'un subordonné : le supérieur accepte, refuse, ou renvoie à son propre supérieur.
pub(super) fn permission_prompt(who: &str, task_prompt: &str, cmd: &str, risky: bool, can_escalate: bool) -> String {
    let mut out = format!("Your subordinate \"{who}\" asks permission to run this shell command:\n\n    {cmd}\n\nTheir task:\n{}\n\n", truncate_middle(task_prompt, 1500));
    if risky {
        out.push_str("This command can MODIFY the system (files, history, processes, network or installs). If you approve, it is allowed for ONE single run of exactly this command; any other or repeated use will ask you again. Approve only if the task really needs it, there is no read-only way to get the same result, and you have checked what it touches.");
    } else {
        out.push_str("This command only reads, tests or builds. If you approve, they may run it (and variants of it) for the rest of the mission.");
    }
    out.push_str(if can_escalate { " Deny if it is unnecessary or unsafe. Use `escalate` only if the decision is not yours to take." } else { " You are the final authority: decide approve or deny." });
    out.push_str(" Answer only through the structured output.");
    out
}
