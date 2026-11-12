//! Construction des prompts et de la carte de poste (fonctions pures, sans I/O).

use super::Phase;
use super::Section;
use crate::model::company::Company;
use crate::runtime::textutil::truncate_middle;
use crate::tui::settings::Lang;

/// Consigne de choix du modèle, ajoutée à tout prompt qui produit des affectations.
pub(super) const MODEL_ADVICE: &str = " Per assignment you may set `model`: \"haiku\" for mechanical or simple work (inventories, checks, small well-specified edits), \"sonnet\" for analysis or code needing judgment, \"opus\" only for hard architecture or subtle reasoning. Pick the cheapest level that does the task well; when in doubt for code that must be correct, \"sonnet\". On a leaf it is its own model; on a manager, the default for the leaves below. Seats marked `model pinned` keep their model; omit `model` to keep the default.";

/// Un résultat d'enfant tronqué au-delà de cette taille dans le prompt de rapport du parent.
pub(super) const MAX_CHILD_CHARS: usize = 6000;

/// La tâche confiée à un subordonné, rappelée dans les prompts de rapport et de revue : début+fin
/// au-delà de cette taille (le plan et la tâche d'origine ne sont jamais tronqués).
pub(super) const MAX_SUBTASK_CHARS: usize = 1500;

/// Nom complet de l'outil de demande à l'humain, tel que le modèle le voit.
pub(super) const ASK: &str = "mcp__essaim__ask_human";

/// Fichiers de la mission, tels qu'un poste doit les désigner (relatifs au projet quand c'est possible).
/// Les postes y trouvent ce que leur supérieur n'a pas recopié, et y déposent leurs livrables longs.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct MissionPaths {
    pub brief: String,
    pub plan: String,
    pub notes: String,
    pub decisions: String,
}

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

/// Un poste n'a pas fini et l'humain répond : le poste reprend sa session avec cette réponse.
pub(super) fn human_reply_prompt(note: &str) -> String {
    let note = note.trim();
    let head = if note.is_empty() { "The human asks you to try again.".to_string() } else { format!("The human answers what stopped you:\n{note}") };
    format!("{head}\n\nContinue your task with this and finish it. Do not redo what is already done. Then answer through the structured output as originally asked.")
}

/// Seat card appended to the system prompt of every call. Written in English (cheaper in tokens);
/// the user's language tells the model what to write in.
/// Order matters for the prompt cache, which matches on the longest identical prefix: what is shared
/// by the whole company (phase, rules) comes first, what is specific to the seat comes last.
///
/// **Le brief ne va qu'au boss.** C'est tout l'intérêt d'une hiérarchie : le boss digère la demande et
/// n'en descend que la part utile, chaque supérieur affinant pour ses subordonnés. Un poste reçoit donc
/// la tâche et les pointeurs écrits par son supérieur ; ce qui lui manque, il va le chercher lui-même
/// (projet, web) ou le demande à l'humain — jamais à son supérieur, dont chaque réponse coûterait un appel.
#[allow(clippy::too_many_arguments)]
pub(super) fn role_card(lang: Lang, c: &Company, i: usize, brief: &str, phase: Phase, kids: &[usize], scope: &[String], paths: Option<&MissionPaths>) -> String {
    let s = &c.seats[i];
    // Phase et règles d'abord : elles sont identiques pour tous les postes, donc elles forment le plus
    // long préfixe commun possible, celui que le cache de prompt sert d'une session à l'autre.
    let mut out = String::new();
    match phase {
        Phase::Conception => out.push_str("PLAN PHASE: the leader writes the mission plan alone. Nothing is built yet: the plan is shown to the human, then carried out by the company.\n"),
        Phase::Realisation => out.push_str("REALISATION PHASE: the plan is approved, the company now carries it out in the project.\n"),
    }
    out.push_str("\nRules: be concise and actionable; stay within your seat's remit. Never claim a success you could not verify. You have every tool and every shell command: use them freely inside the project folder (your working directory), and never modify anything outside it.\n");
    out.push_str(&format!(
        "Fact or intent: a FACT (what the project contains, what a documentation says) you look up yourself, in the project or on the web, without asking anyone. An INTENT (what the human wants: an ambiguity, a choice between valid options, a priority, a success criterion) you ask the human with the `{ASK}` tool, as soon as it appears and before building on a guess. Never ask your supervisor anything during a task.\n"
    ));
    out.push_str(&format!(
        "Stuck: at most two attempts at the same failure. Then ask the human (`{ASK}`, kind \"blocked\" or \"action\") with your diagnosis and the exact action needed, wait for the answer and continue. Never work around a failure silently.\n"
    ));
    if let Some(p) = paths {
        out.push_str(&format!(
            "Shared notes of the mission: `{}/`. Read `{}` before asking the human, so you never ask what was already answered. Write any long deliverable there as `<your-seat-id>-<topic>.md` and report only a short summary with its path.\n",
            p.notes, p.decisions
        ));
    }
    out.push_str(&format!("The user's language is {}: write everything you produce in it.\n", lang.name()));
    // Le brief au boss seulement ; aux autres, où le trouver s'ils en ont besoin.
    // Volontairement court : c'est dans le prompt système, donc relu à chaque tour d'outil.
    if c.root() == Some(i) {
        out.push_str(&format!("\nCompany mission (brief framed with the leader):\n{brief}\n"));
    } else {
        out.push_str("\nYou do not see the company brief: your supervisor's task and its context pointers are your starting point. Do not widen the task.");
        match paths {
            Some(p) => out.push_str(&format!(" If you need them, the brief is `{}` and the approved plan `{}`.\n", p.brief, p.plan)),
            None => out.push('\n'),
        }
    }
    let mut chain: Vec<String> = c.chain_of_command(i).iter().rev().map(|&p| c.seats[p].title.clone()).collect();
    chain.push(format!("{} (you)", s.title));
    out.push_str(&format!(
        "\nYou hold the seat \"{}\" ({}, id `{}`) in the company \"{}\", under the identity of the agent \"{}\".\nChain of command: {}.\nCurrent phase: {}.\n",
        s.title,
        c.level(i),
        s.id,
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
            let role = if c.is_leaf(k) { if ks.builds { " · builds in the project" } else { " · analyses and advises" } } else { " · leads a team" };
            let pinned = ks.model.as_ref().map(|m| format!(" · model pinned: {m}")).unwrap_or_default();
            out.push_str(&format!("- {} · {}{}{role}{pinned}\n", ks.id, ks.title, if expertise.is_empty() { String::new() } else { format!(" · {expertise}") }));
        }
        if phase == Phase::Realisation {
            out.push_str("You coordinate: you split the work, hand each subordinate the context it needs, check the results yourself (read the project, run the tests) and report. You do not do your subordinates' work in their place.\n");
        }
    } else if phase == Phase::Realisation {
        // Un poste seul dans l'entreprise fait tout lui-même ; sinon `builds` distingue qui réalise de qui conseille.
        if s.builds || c.seats.len() == 1 {
            out.push_str("You carry out the work in the project yourself.\n");
        } else {
            out.push_str("You analyse and advise: your deliverables are findings and notes. Modify project files only if your task explicitly asks for it.\n");
        }
    }
    if let Some(m) = &s.helpers {
        out.push_str(&format!(
            "You have light assistants (subagent \"{}\", model {m}): hand them only mechanical, fully specified chores that take no decision (listing or counting files, running a command and returning its raw output, extracting lines); they never edit files. You stay responsible: decide yourself, give precise instructions, and verify what they return.\n",
            super::shared::HELPER
        ));
    }
    if phase == Phase::Realisation && kids.is_empty() {
        if scope.is_empty() {
            out.push_str("No file scope was assigned to you: do not modify project files (the shared notes excepted).\n");
        } else {
            out.push_str(&format!("File scope assigned to you: {}. Modify project files only within it: other seats work on the rest at the same time.\n", scope.join(", ")));
        }
    }
    out
}

/// Le boss écrit le plan seul. `outline` : l'organigramme complet, pour répartir le travail par poste.
pub(super) fn plan_prompt(outline: &[String]) -> String {
    format!(
        "Write the plan of the mission described in the brief.\n\n1. Explore what you need to plan well: the project, and the web for anything external.\n2. BEFORE writing, ask the human every question whose answer would change the plan (`{ASK}`, kind \"question\", each with concrete options and the one you recommend; group them in one or two calls). This is where an answer saves the most work downstream. Ask nothing you can find out yourself.\n3. Write the plan as your report: the goal and checkable success criteria; the work split by seat of your company (who does what, in what order, on which files); the decisions taken and the assumptions that remain; the risks.\n\nYour company:\n{}\n\nThe plan is shown to the human for approval, then handed back to you to lead its realisation: make it self-sufficient. You build nothing now. Answer only through the structured output: status \"done\" with the plan in `report`; \"blocked\" with `blockers` only if you cannot plan at all.",
        outline.join("\n")
    )
}

pub(super) fn delegate_prompt(task: &str, scope: &[String]) -> String {
    let mut out = format!(
        "Task received from your supervisor:\n{task}\n\nSplit it among your direct subordinates: involve only those who are useful, and give each a self-contained, precise task that does not overlap with the others. You may look at the project first if it helps you split well.\n\nYour subordinates see neither the brief nor the task you received: what you write is ALL they get. So each task must stand alone — objective, the constraints that apply, how to tell it is done — and carry only what that subordinate needs."
    );
    // Des pointeurs plutôt que des copies : le subordonné lit ce qu'il lui faut, au lieu que le contenu
    // traverse chaque niveau de la hiérarchie (et soit relu à chaque tour d'outil).
    out.push_str(
        " For each one give `context`: pointers to what it needs — project files or folders (with line ranges when useful), notes of the mission, URLs — instead of copying their content. Point to what you already know so it does not search again; it looks up anything else on its own.",
    );
    // Une session longue coûte tours × contexte moyen, donc son coût croît avec le carré de sa
    // durée : six étapes dans une seule session coûtent plusieurs fois les mêmes six en six sessions.
    out.push_str(
        " One assignment = ONE step, small enough to end in a single coherent commit. If a subordinate's work needs several successive steps, give it SEVERAL assignments, one per step, in order: they run one after the other, each starting from a clean session. Never pack a sequence of steps into one assignment — a session that accumulates everything costs several times the same work split into steps.",
    );
    out.push_str(&format!(
        "\n\nYour file scope: `{}`. For each subordinate that will modify files, give a `scope`: project-relative file patterns (e.g. `src/api/**`, `docs/guide.md`), ALL included in yours. It only serves to run non-overlapping work in parallel: two subordinates whose scopes overlap run one after the other, so prefer disjoint scopes. Omit `scope` for a subordinate that only analyses.",
        scope.join("`, `")
    ));
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
/// `builds` : le poste réalise dans le projet (sinon il analyse et conseille). `commit` : un commit par étape.
pub(super) fn work_prompt(task: &str, sections: &[Section], step: Option<(usize, usize)>, builds: bool, commit: bool) -> String {
    let end = format!(
        " Answer only through the structured output: status \"done\" ONLY if the task is really complete (verified as far as you can) with every section filled; otherwise \"blocked\" with `blockers` (kind, exact cause, what is needed). Never report \"done\" with a known problem. Before reporting \"blocked\", ask the human (`{ASK}`) whenever their answer or their action would let you finish."
    );
    let (sec, rep) = (sections_hint(sections), if sections.is_empty() { "`report`" } else { "the sections" });
    // Étape d'une série : la session est neuve, donc le poste ignore ce qui précède. Il faut lui dire
    // qu'il ne doit ni refaire ni anticiper, sinon il relit le projet pour « vérifier » les étapes faites.
    let head = match step {
        Some((n, m)) => format!("Step {n} of {m} of what your supervisor assigned you. The earlier steps are already done: do not redo them, do not audit them, and do not start the later ones — this step alone is your task.\n\n"),
        None => String::new(),
    };
    if !builds {
        return format!("{head}Task:\n{task}{sec}\n\nProduce your deliverable (analysis, specification, recommendations) in {rep}; put anything long in the shared notes and give its path.{end}");
    }
    // Déplacer du code en réécrivant le fichier entier coûte plusieurs fois sa taille : un Edit
    // transporte l'ancien ET le nouveau texte, et tout cela est relu à chaque tour suivant.
    let commit = if commit && step.is_some() { " End this step with one commit covering exactly it." } else { "" };
    format!(
        "{head}Task:\n{task}{sec}\n\nCarry it out in the project, within your scope, verify it (tests, build), then summarize precisely what you did in {rep}. To relocate code, use `git mv` then narrow edits; never rewrite a whole file just to move or re-indent code.{commit}\n\nIf a test fails or a prerequisite is missing: fix what is yours to fix; if it is not, do not continue as if it worked.{end}"
    )
}

/// Bloc « tâche d'origine + résultats », identique octet pour octet dans le rapport et la revue : leur début
/// commun est ainsi servi par le cache de prompt. `results`: (title, id, assigned task, success, text).
fn results_block(task: &str, results: &[(String, String, String, bool, String)]) -> String {
    let mut out = format!("Original task your supervisor gave you:\n{task}\n\nResults from your subordinates:\n");
    for (title, id, sub_task, ok, text) in results {
        let t = truncate_middle(text, MAX_CHILD_CHARS);
        out.push_str(&format!(
            "\n### {title} ({id}){}\nAssigned task: {}\n{}\n",
            if *ok { "" } else { " — NOT COMPLETED" },
            truncate_middle(sub_task, MAX_SUBTASK_CHARS),
            if *ok { t } else { format!("[NOT COMPLETED: {t}]") }
        ));
    }
    out
}

pub(super) fn report_prompt(task: &str, results: &[(String, String, String, bool, String)], sections: &[Section]) -> String {
    let mut out = results_block(task, results);
    out.push_str(&sections_hint(sections));
    out.push_str("\nWrite your summary report for your supervisor: conclusions, decisions, points of attention, missing elements. Explicitly mention anything NOT COMPLETED and its impact. Answer through the structured output: status \"done\" only if every part of the task is really complete and verified (a subordinate's problem that was fixed in a later result counts as resolved); otherwise \"blocked\" with the remaining `blockers`.");
    out
}

/// Revue du boss : le résultat atteint-il l'objectif ? Elle constate ; c'est l'humain qui décide de la suite.
pub(super) fn review_prompt(task: &str, results: &[(String, String, String, bool, String)]) -> String {
    let mut out = results_block(task, results);
    out.push_str("\nYou are accountable for the result reaching the goal. Review it against the success criteria of the brief and of the plan: check it yourself (tests, build, git diff, reading the files) instead of trusting reports. Treat any statement of failure, partial result, missing deliverable or unmeasured criterion as unmet unless a later result clearly resolved it. You verify, you do not fix.\n- Goal fully reached and verified: `accepted` true, with a one-line `findings`.\n- Otherwise: `accepted` false and `findings` = exactly what is missing or wrong, each gap on its own line with where to look. The human reads it and decides whether to send the work back down, accept it as is, or stop: write it so they can decide.\n\nAnswer only through the structured output.");
    out
}

/// Tâche de la descente corrective que l'humain a demandée après une revue non conforme.
pub(super) fn corrective_task(task: &str, findings: &str, note: &str) -> String {
    let mut out = format!("Your review of the result found these gaps:\n{}\n", findings.trim());
    if !note.trim().is_empty() {
        out.push_str(&format!("\nInstruction from the human:\n{}\n", note.trim()));
    }
    out.push_str(&format!("\nHave these gaps closed: assign only what is needed for that, to the subordinates best placed. The original task, for reference:\n{task}"));
    out
}

/// Texte de la demande à l'humain pour un poste qui n'a pas fini : ce qui l'arrête, et ce qu'il lui faudrait.
pub(super) fn blocked_question(lang: Lang, title: &str, report: &str, blockers: &[String]) -> String {
    let mut out = format!("{} : {}", title, lang.t("ce poste n'a pas pu finir sa tâche.", "this seat could not finish its task."));
    for b in blockers {
        out.push_str(&format!("\n- {b}"));
    }
    let report = report.trim();
    if !report.is_empty() && blockers.len() < 2 {
        out.push_str(&format!("\n\n{}", truncate_middle(report, 1200)));
    }
    out.push_str(lang.t(
        "\n\nRépondez en texte libre pour relancer le poste avec votre réponse.",
        "\n\nAnswer in free text to send the seat back to work with your answer.",
    ));
    out
}

/// Texte de la demande à l'humain après une revue non conforme.
pub(super) fn review_question(lang: Lang, findings: &str) -> String {
    format!(
        "{}\n{}\n\n{}",
        lang.t("La revue finale ne valide pas le résultat :", "The final review does not accept the result:"),
        truncate_middle(findings.trim(), 2400),
        lang.t("Répondez en texte libre pour relancer avec une consigne.", "Answer in free text to send it back with an instruction.")
    )
}
