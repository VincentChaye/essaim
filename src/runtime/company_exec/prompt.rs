//! Construction des prompts et de la carte de poste (fonctions pures, sans I/O).

use super::Phase;
use crate::model::company::Company;
use crate::tf;
use crate::tui::settings::Lang;

/// Consigne de choix du modèle, ajoutée à tout prompt qui produit des affectations.
pub(super) const MODEL_ADVICE: &str = " For each assignment you may set `model`, the cost/quality level of whoever does it: \"haiku\" for purely mechanical or simple work (inventories, running checks, small well-specified edits), \"sonnet\" for real analysis or code that needs judgment, \"opus\" only for hard architecture or subtle reasoning. Choose the cheapest level that will do the task well, based on what the task really demands; when in doubt for code that must be correct, use \"sonnet\". On a leaf it is its own model; on a manager it is the default for the leaves below (who can be refined further down). Seats marked `model pinned` keep their fixed model; leave `model` out to keep the default.";

/// Un résultat d'enfant tronqué au-delà de cette taille dans le prompt de rapport du parent.
pub(super) const MAX_CHILD_CHARS: usize = 6000;

/// Seat card appended to the system prompt of every call. Written in English (cheaper in tokens);
/// the user's language tells the model what to write in.
pub(super) fn role_card(lang: Lang, c: &Company, i: usize, brief: &str, phase: Phase, kids: &[usize], scope: &[String]) -> String {
    let s = &c.seats[i];
    let mut chain: Vec<String> = c.chain_of_command(i).iter().rev().map(|&p| c.seats[p].title.clone()).collect();
    chain.push(format!("{} (you)", s.title));
    let mut out = format!(
        "You hold the seat \"{}\" ({}) in the company \"{}\", under the identity of the agent \"{}\".\nChain of command: {}.\nCurrent phase: {}.\n",
        s.title,
        c.level(i),
        c.name,
        s.agent,
        chain.join(" → "),
        phase.name()
    );
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
    out.push_str(&format!("\nCompany mission (brief framed with the leader):\n{brief}\n"));
    match phase {
        Phase::Conception => out.push_str(
            "\nDESIGN PHASE: we think and prepare, we build NOTHING. Produce only analysis, specifications, decisions and recommendations for the mission (nothing is created or modified in the project yet; this is normal). Never report as a failure or a gap that the mission's result does not exist yet: it is set up later by the realisation team, after the plan is approved.\n",
        ),
        Phase::Realisation => out.push_str("\nREALISATION PHASE: the plan is approved, we now build in the project.\n"),
    }
    out.push_str(&format!(
        "\nRules: be concise and actionable; stay within your seat's remit; do not repeat the brief; clearly flag what is missing or what really failed. Never claim a success you could not verify: when you cannot complete your task, say so (status blocked) with the exact cause and what is needed, instead of working around it. The user's language is {}: write everything you produce in it.",
        lang.name()
    ));
    out
}

pub(super) fn delegate_prompt(task: &str, phase: Phase, scope: Option<&[String]>) -> String {
    let mut out = format!(
        "Task received from your supervisor:\n{task}\n\nSplit it among your direct subordinates: involve only those who are useful, and give each a self-contained, precise task that does not overlap with the others."
    );
    if phase == Phase::Conception {
        out.push_str(" In design, give ANALYSIS or SPECIFICATION tasks (what will need doing and how), never execution tasks.");
    }
    if let Some(sc) = scope {
        out.push_str(&format!(
            "\n\nYour file scope: `{}`. For each subordinate, give a `scope`: a list of project-relative file patterns (e.g. `src/api/**`, `docs/guide.md`), ALL included in yours. Two subordinates whose scopes overlap run one after the other: prefer disjoint scopes.",
            sc.join("`, `")
        ));
    }
    out.push_str(MODEL_ADVICE);
    out.push_str(" Answer only through the structured output.");
    out
}

pub(super) fn work_prompt(task: &str, phase: Phase) -> String {
    let end = " Answer only through the structured output: status \"done\" ONLY if the task is really complete (and verified as far as your tools allow); otherwise \"blocked\" with `blockers` (kind, exact cause, what is needed). Never report \"done\" with a known problem.";
    match phase {
        Phase::Conception => format!("Task:\n{task}\n\nProduce your deliverable as text (analysis, specification, recommendations) in `report`. You do not have to modify the project; use \"blocked\" only if you cannot produce your analysis (missing access or information).{end}"),
        Phase::Realisation => format!("Task:\n{task}\n\nCarry it out in the project, only within your scope, verify it with the commands you are allowed (tests, build), then summarize precisely what you did in `report`.\n\nIf a tool or command is refused, a test fails or a prerequisite is missing: first fix what is within your scope; if you cannot, STOP and report it as blocked. Do not silently work around it and do not continue as if it worked.{end}"),
    }
}

/// `results`: (title, id, assigned task, success, text).
pub(super) fn report_prompt(task: &str, results: &[(String, String, String, bool, String)]) -> String {
    let mut out = format!("Original task your supervisor gave you:\n{task}\n\nResults from your subordinates:\n");
    for (title, id, sub_task, ok, text) in results {
        let mut t = text.clone();
        if t.chars().count() > MAX_CHILD_CHARS {
            t = t.chars().take(MAX_CHILD_CHARS).collect::<String>() + "\n[… truncated]";
        }
        out.push_str(&format!(
            "\n### {title} ({id}){}\nAssigned task: {sub_task}\n{}\n",
            if *ok { "" } else { " — FAILED" },
            if *ok { t } else { format!("[FAILED: {t}]") }
        ));
    }
    out.push_str("\nWrite your summary report for your supervisor: conclusions, decisions, points of attention, missing elements. Explicitly mention any FAILURE and its impact. Answer through the structured output: status \"done\" only if every part of the task is really complete and verified (a subordinate's problem that was fixed in a later result counts as resolved); otherwise \"blocked\" with the remaining `blockers`.");
    out
}

/// `failed`: (title, id, assigned task, ok = false, texte avec blocages).
pub(super) fn fix_prompt(task: &str, failed: &[(String, String, String, bool, String)], scope: Option<&[String]>, decision: Option<&str>) -> String {
    let mut out = format!("Original task your supervisor gave you:\n{task}\n\nThese subordinates could NOT complete their work:\n");
    for (title, id, sub_task, _, text) in failed {
        let mut t = text.clone();
        if t.chars().count() > MAX_CHILD_CHARS {
            t = t.chars().take(MAX_CHILD_CHARS).collect::<String>() + "\n[… truncated]";
        }
        out.push_str(&format!("\n### {title} ({id})\nAssigned task: {sub_task}\n[{t}]\n"));
    }
    out.push_str("\nYou are responsible for getting this fixed, properly. Write a short correction plan in `plan` (root cause, then the steps). Then give corrective, self-contained tasks in `assignments` to the subordinate(s) best placed to fix it: possibly a different one than who failed (e.g. the one that has the missing tool or permission, or the specialist team). If nobody in your branch can fix it (a permission or tool nobody has, a prerequisite that does not exist, a decision that is not yours), assign nothing and explain precisely in `escalate` what is needed: your supervisor will take over. Do not play the problem down.");
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
    let mut out = format!("Original task:\n{task}\n\nWhat your subordinates delivered:\n");
    for (title, id, sub_task, _, text) in results {
        let mut t = text.clone();
        if t.chars().count() > MAX_CHILD_CHARS {
            t = t.chars().take(MAX_CHILD_CHARS).collect::<String>() + "\n[… truncated]";
        }
        out.push_str(&format!("\n### {title} ({id})\nAssigned task: {sub_task}\n{t}\n"));
    }
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
