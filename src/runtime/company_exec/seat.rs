//! Exécution d'un poste : délégation, lots de travail, correction des échecs, rapport de synthèse.

use super::prompt::{escalation_summary, report_prompt, work_prompt};
use super::schema::FixPlan;
use super::shared::Shared;
use super::{Assignment, Blocker, Kind, Phase, SeatOut, Status};
use crate::runtime::executor::EscalationReply;
use crate::runtime::scope;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::task::JoinSet;

/// Exécute des affectations : en conception tout en parallèle ; en réalisation par lots sans chevauchement de
/// fichiers, l'un après l'autre. Un lot qui échoue arrête les suivants (on ne construit pas sur une base cassée).
/// Renvoie les affectations menées (avec leur résultat) et celles jamais démarrées.
async fn run_batch(sh: &Arc<Shared>, phase: Phase, todo: Vec<Assignment>) -> (Vec<(Assignment, SeatOut)>, Vec<Assignment>) {
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
            let (sh2, to, t, s2) = (sh.clone(), a.to, a.task.clone(), a.scope.clone());
            set.spawn(async move { (n, run_seat(sh2, to, phase, t, s2).await) });
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

pub(super) fn run_seat(sh: Arc<Shared>, i: usize, phase: Phase, task: String, sc: Vec<String>) -> Pin<Box<dyn Future<Output = SeatOut> + Send>> {
    Box::pin(async move {
        let kids = sh.kids(i, phase);

        // Feuille : travail direct.
        if kids.is_empty() {
            return sh.act(i, phase, Kind::Work, work_prompt(&task, phase), &kids, &sc).await.into();
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
        let mut decision: Option<String> = None;
        let mut reason: Option<String> = None;
        // Problèmes non résolus à la fin : (titre, texte avec blocages, blocages).
        let mut unresolved: Vec<(String, String, Vec<Blocker>)> = vec![];
        loop {
            let (ran, pending) = run_batch(&sh, phase, todo).await;
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
                if unresolved.is_empty() || sh.cancelled() {
                    break None;
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

        let rep = sh.act(i, phase, Kind::Report, report_prompt(&task, &history), &kids, &sc).await;
        let mut blockers: Vec<Blocker> = unresolved.iter().flat_map(|(_, _, b)| b.clone()).collect();
        if unresolved.is_empty() && rep.status == Status::Done {
            return SeatOut { status: Status::Done, text: rep.report, blockers: vec![] };
        }
        blockers.extend(rep.blockers);
        let status = if unresolved.is_empty() && rep.status == Status::Failed { Status::Failed } else { Status::Blocked };
        SeatOut { status, text: rep.report, blockers }
    })
}
