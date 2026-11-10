//! Exécution d'un poste : délégation, lots de travail, correction des échecs, rapport de synthèse.

use super::prompt::{escalation_summary, report_prompt, work_prompt};
use super::schema::{FixPlan, ReviewPlan};
use super::shared::Shared;
use super::{Assignment, Blocker, Kind, Phase, SeatOut, Section, Status, join_steps};
use crate::runtime::executor::EscalationReply;
use crate::runtime::scope;
use crate::runtime::textutil::truncate_middle;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::task::JoinSet;

/// Taille conservée (début+fin) du texte d'un résultat d'un tour précédent dans l'historique du poste.
const OLD_ROUND_CHARS: usize = 2000;

/// Exécute des affectations : en conception tout en parallèle ; en réalisation par lots sans chevauchement de
/// fichiers, l'un après l'autre. Un lot qui échoue arrête les suivants (on ne construit pas sur une base cassée).
/// Renvoie les affectations menées (avec leur résultat) et celles jamais démarrées.
async fn run_batch(sh: &Arc<Shared>, from: usize, phase: Phase, todo: Vec<Assignment>) -> (Vec<(Assignment, SeatOut)>, Vec<Assignment>) {
    let lots: Vec<Vec<usize>> = match phase {
        Phase::Conception => vec![(0..todo.len()).collect()],
        Phase::Realisation => scope::batches(&todo.iter().map(|a| a.scope.clone()).collect::<Vec<_>>()),
    };
    let mut outs: Vec<Option<SeatOut>> = (0..todo.len()).map(|_| None).collect();
    let mut started = vec![false; todo.len()];
    for lot in lots {
        if sh.cancelled() {
            break;
        }
        let mut set: JoinSet<(usize, SeatOut)> = JoinSet::new();
        for &n in &lot {
            started[n] = true;
            let a = &todo[n];
            sh.set_hint(from, a);
            let (sh2, to, t, s2, sec) = (sh.clone(), a.to, a.steps.clone(), a.scope.clone(), a.sections.clone());
            set.spawn(async move { (n, run_seat(sh2, to, phase, t, s2, sec).await) });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((n, out)) = joined {
                outs[n] = Some(out);
            }
        }
        if lot.iter().any(|&n| outs[n].as_ref().is_none_or(|o| !o.ok())) {
            break;
        }
    }
    let (mut ran, mut pending) = (vec![], vec![]);
    for ((a, out), was_started) in todo.into_iter().zip(outs).zip(started) {
        match (out, was_started) {
            (Some(o), _) => ran.push((a, o)),
            (None, true) => ran.push((a, SeatOut::failed("the node was interrupted"))),
            (None, false) => pending.push(a),
        }
    }
    (ran, pending)
}

/// Rangée du rapport d'un supérieur : (titre, id, tâche, ok, texte avec blocages).
fn result_row(sh: &Shared, a: &Assignment, out: &SeatOut) -> (String, String, String, bool, String) {
    let s = &sh.company.seats[a.to];
    (s.title.clone(), s.id.clone(), a.task.clone(), out.ok(), out.for_supervisor())
}

/// `steps` : les étapes confiées au poste, dans l'ordre (au moins une).
pub(super) fn run_seat(sh: Arc<Shared>, i: usize, phase: Phase, steps: Vec<String>, sc: Vec<String>, sections: Vec<Section>) -> Pin<Box<dyn Future<Output = SeatOut> + Send>> {
    Box::pin(async move {
        let kids = sh.kids(i, phase);
        // Un poste interne raisonne sur l'ensemble de ce qu'on lui confie : il le redécoupe lui-même.
        let task = steps.join("\n\n");

        // Feuille : travail direct, une étape après l'autre. Chaque étape est un appel `claude` neuf,
        // donc un contexte qui ne porte que cette étape — c'est tout l'intérêt du découpage. Une étape
        // qui n'aboutit pas arrête les suivantes : on ne construit pas sur une base cassée.
        if kids.is_empty() {
            let mut done: Vec<(usize, SeatOut)> = vec![];
            for (n, step) in steps.iter().enumerate() {
                if sh.cancelled() {
                    break;
                }
                let only = steps.len() == 1;
                let out: SeatOut = sh
                    .act(i, phase, Kind::Work, work_prompt(step, phase, &sections, (!only).then_some((n + 1, steps.len())), sh.company.allows_git(i)), &kids, &sc, &sections, (!only).then_some(n as u32))
                    .await
                    .into();
                let ok = out.ok();
                done.push((n + 1, out));
                if !ok {
                    break;
                }
            }
            if done.is_empty() {
                return SeatOut::failed("cancelled");
            }
            return join_steps(done);
        }

        // Poste interne : délégation → travail des subordonnés → (correction des échecs) → rapport de synthèse.
        let assignments = match sh.delegate(i, phase, &task, &kids, &sc).await {
            Ok(a) => a,
            Err(e) => return SeatOut::failed(format!("delegation impossible: {e}")),
        };
        let is_root = sh.company.root() == Some(i);
        let mut history: Vec<(String, String, String, bool, String)> = vec![];
        let mut todo = assignments;
        let mut rounds_left = sh.company.max_fix_rounds;
        // Seul le CEO relit le résultat contre l'objectif et peut relancer une redescente.
        let mut reviews_left = if is_root { sh.company.max_review_rounds } else { 0 };
        let mut decision: Option<String> = None;
        let mut reason: Option<String> = None;
        // Problèmes non résolus à la fin : (titre, texte avec blocages, blocages).
        let mut unresolved: Vec<(String, String, Vec<Blocker>)> = vec![];
        // Rangées de `history` déjà réduites (tours précédents) : une seule réduction, jamais deux.
        let mut compacted = 0;
        loop {
            let (ran, pending) = run_batch(&sh, i, phase, todo).await;
            // Les résultats des tours précédents ne servent plus qu'en rappel : début+fin, le tour courant reste entier.
            for row in &mut history[compacted..] {
                row.4 = truncate_middle(&row.4, OLD_ROUND_CHARS);
            }
            compacted = history.len();
            let mut failed_rows = vec![];
            unresolved.clear();
            for (a, out) in &ran {
                let row = result_row(&sh, a, out);
                if !out.ok() {
                    let title = row.0.clone();
                    unresolved.push((title.clone(), row.4.clone(), out.blockers.iter().map(|b| Blocker { detail: format!("{title}: {}", b.detail), ..b.clone() }).collect()));
                    failed_rows.push(row.clone());
                }
                history.push(row);
            }
            for a in &pending {
                let title = sh.company.seats[a.to].title.clone();
                let msg = "not started: a previous batch is blocked".to_string();
                unresolved.push((title.clone(), msg.clone(), vec![Blocker { kind: "prerequisite".into(), detail: format!("{title}: {msg}"), needs: String::new() }]));
            }
            // Résolution : correctifs dans la branche, puis remontée ; le CEO en dernier recours s'adresse à l'utilisateur.
            let next = loop {
                if sh.cancelled() {
                    break None;
                }
                if unresolved.is_empty() {
                    if reviews_left == 0 {
                        break None;
                    }
                    reviews_left -= 1;
                    match sh.review(i, phase, &task, &history, &kids, &sc, reviews_left + 1).await {
                        ReviewPlan::Accepted => break None,
                        ReviewPlan::Redo { assignments, .. } => break Some(assignments),
                        ReviewPlan::Unmet(findings) => {
                            let title = sh.company.seats[i].title.clone();
                            let msg = format!("review: goal not reached: {findings}");
                            unresolved.push((title.clone(), msg.clone(), vec![Blocker { kind: "other".into(), detail: format!("{title}: {msg}"), needs: String::new() }]));
                            failed_rows.push((title, sh.company.seats[i].id.clone(), task.clone(), false, msg));
                            reason = Some(findings);
                            rounds_left = 0;
                            continue;
                        }
                    }
                }
                if rounds_left > 0 {
                    rounds_left -= 1;
                    match sh.fix(i, phase, &task, &failed_rows, &kids, &sc, decision.as_deref()).await {
                        FixPlan::Assign { assignments, .. } => {
                            let mut next = assignments;
                            next.extend(pending.iter().cloned());
                            break Some(next);
                        }
                        FixPlan::Escalate(r) => {
                            reason = Some(r);
                            rounds_left = 0;
                        }
                    }
                } else if is_root {
                    let shown: Vec<(String, String)> = unresolved.iter().map(|(t, x, _)| (t.clone(), x.clone())).collect();
                    match sh.ask_user(escalation_summary(&shown, reason.as_deref())).await {
                        EscalationReply::Retry => {
                            rounds_left = sh.company.max_fix_rounds.max(1);
                            reviews_left = reviews_left.max(1);
                            decision = sh.resolution();
                        }
                        EscalationReply::Stop => break None,
                    }
                } else {
                    break None;
                }
            };
            match next {
                Some(n) => todo = n,
                None => break,
            }
        }

        let rep = sh.act(i, phase, Kind::Report, report_prompt(&task, &history, &sections), &kids, &sc, &sections, None).await;
        let mut blockers: Vec<Blocker> = unresolved.iter().flat_map(|(_, _, b)| b.clone()).collect();
        if unresolved.is_empty() && rep.status == Status::Done {
            return SeatOut { status: Status::Done, text: rep.report, blockers: vec![] };
        }
        blockers.extend(rep.blockers);
        let status = if unresolved.is_empty() && rep.status == Status::Failed { Status::Failed } else { Status::Blocked };
        SeatOut { status, text: rep.report, blockers }
    })
}
