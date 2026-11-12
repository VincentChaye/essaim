//! Exécution d'un poste : délégation, lots de travail, rapport de synthèse, revue du boss.
//!
//! Aucune boucle de correction : un poste qui n'aboutit pas s'adresse à l'humain, et chaque nouvel essai
//! est une décision de l'humain (voir `Shared::work`).

use super::prompt::{corrective_task, report_prompt, work_prompt};
use super::schema::Review;
use super::shared::{Shared, Verdict};
use super::{Assignment, Blocker, Kind, Phase, SeatOut, Section, Status, join_steps};
use crate::runtime::scope;
use crate::runtime::textutil::truncate_middle;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::task::JoinSet;

/// Taille conservée (début+fin) du texte d'un résultat d'un tour précédent dans l'historique du poste.
const OLD_ROUND_CHARS: usize = 2000;

/// Exécute des affectations par lots sans chevauchement de fichiers, l'un après l'autre (les affectations
/// sans périmètre — de l'analyse — vont toutes dans le premier lot). Un lot qui n'aboutit pas arrête les
/// suivants : on ne construit pas sur une base cassée, sauf si l'humain a dit de continuer sans ce poste.
/// Renvoie les affectations menées (avec leur résultat) et celles jamais démarrées.
async fn run_batch(sh: &Arc<Shared>, from: usize, phase: Phase, todo: Vec<Assignment>) -> (Vec<(Assignment, SeatOut)>, Vec<Assignment>) {
    let lots = scope::batches(&todo.iter().map(|a| a.scope.clone()).collect::<Vec<_>>());
    let mut outs: Vec<Option<SeatOut>> = (0..todo.len()).map(|_| None).collect();
    let mut started = vec![false; todo.len()];
    for lot in lots {
        if sh.stopped() {
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
        if lot.iter().any(|&n| outs[n].as_ref().is_none_or(|o| !o.ok() && !o.waived)) {
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
        let kids = sh.kids(i);
        // Un poste interne raisonne sur l'ensemble de ce qu'on lui confie : il le redécoupe lui-même.
        let task = steps.join("\n\n");

        // Feuille : travail direct, une étape après l'autre. Chaque étape est un appel `claude` neuf,
        // donc un contexte qui ne porte que cette étape — c'est tout l'intérêt du découpage. Une étape
        // qui n'aboutit pas arrête les suivantes, sauf si l'humain a dit de continuer.
        if kids.is_empty() {
            let builds = sh.company.seats[i].builds || sh.company.seats.len() == 1;
            let mut done: Vec<(usize, SeatOut)> = vec![];
            for (n, step) in steps.iter().enumerate() {
                if sh.stopped() {
                    break;
                }
                let only = steps.len() == 1;
                let prompt = work_prompt(step, &sections, (!only).then_some((n + 1, steps.len())), builds, sh.company.commit_steps);
                let (o, waived) = sh.work(i, phase, prompt, &kids, &sc, &sections, (!only).then_some(n as u32)).await;
                let out = SeatOut { waived, ..o.into() };
                let go_on = out.ok() || out.waived;
                done.push((n + 1, out));
                if !go_on {
                    break;
                }
            }
            if done.is_empty() {
                return SeatOut::failed("cancelled");
            }
            return join_steps(done);
        }

        // Poste interne : délégation → travail des subordonnés → rapport de synthèse.
        // Le boss ajoute une revue du résultat ; un écart va à l'humain, qui peut demander une descente corrective.
        let is_root = sh.company.root() == Some(i);
        let mut to_split = task.clone();
        let mut history: Vec<(String, String, String, bool, String)> = vec![];
        // Problèmes non résolus à la fin : (titre, texte avec blocages, blocages).
        let mut unresolved: Vec<(String, String, Vec<Blocker>)> = vec![];
        // Rangées de `history` déjà réduites (descentes précédentes) : une seule réduction, jamais deux.
        let mut compacted = 0;
        // L'humain a dit de continuer malgré tout ce qui reste non résolu.
        let mut waived;
        loop {
            // Une délégation qui échoue (sortie illisible deux fois, appel en erreur) : l'humain décide.
            let todo = loop {
                match sh.delegate(i, phase, &to_split, &kids, &sc).await {
                    Ok(a) => break Some(a),
                    Err(_) if sh.stopped() => break None,
                    Err(e) => {
                        let b = vec![Blocker { kind: "other".into(), detail: format!("delegation impossible: {e}"), needs: String::new() }];
                        match sh.ask_blocked(i, "", &b).await {
                            Verdict::Retry(note) if !note.trim().is_empty() => to_split = format!("{to_split}\n\nInstruction from the human:\n{}", note.trim()),
                            Verdict::Retry(_) => {}
                            Verdict::Stop => {
                                sh.halted.store(true, std::sync::atomic::Ordering::SeqCst);
                                return SeatOut::failed(format!("delegation impossible: {e}"));
                            }
                            v => return SeatOut { waived: v == Verdict::Waive, ..SeatOut::failed(format!("delegation impossible: {e}")) },
                        }
                    }
                }
            };
            let Some(todo) = todo else { return SeatOut::failed("cancelled") };
            let (ran, pending) = run_batch(&sh, i, phase, todo).await;
            // Les résultats des descentes précédentes ne servent plus qu'en rappel : début+fin, la descente courante reste entière.
            for row in &mut history[compacted..] {
                row.4 = truncate_middle(&row.4, OLD_ROUND_CHARS);
            }
            compacted = history.len();
            unresolved.clear();
            waived = true;
            for (a, out) in &ran {
                let row = result_row(&sh, a, out);
                if !out.ok() {
                    let title = row.0.clone();
                    unresolved.push((title.clone(), row.4.clone(), out.blockers.iter().map(|b| Blocker { detail: format!("{title}: {}", b.detail), ..b.clone() }).collect()));
                    waived &= out.waived;
                }
                history.push(row);
            }
            for a in &pending {
                let title = sh.company.seats[a.to].title.clone();
                let msg = "not started: a previous batch did not complete".to_string();
                unresolved.push((title.clone(), msg.clone(), vec![Blocker { kind: "prerequisite".into(), detail: format!("{title}: {msg}"), needs: String::new() }]));
                waived = false;
            }
            // La revue ne porte que sur un résultat que la branche tient pour complet : sinon ce qui manque est déjà connu.
            if !is_root || !sh.company.reviews() || !unresolved.is_empty() || sh.stopped() {
                break;
            }
            match sh.review(i, phase, &task, &history, &kids, &sc).await {
                Review::Accepted => break,
                Review::Unmet(findings) => {
                    let title = sh.company.seats[i].title.clone();
                    let gap = (title.clone(), format!("review: goal not reached: {findings}"), vec![Blocker { kind: "other".into(), detail: format!("{title}: review: goal not reached: {findings}"), needs: String::new() }]);
                    match sh.ask_review(i, &findings).await {
                        // Descente corrective demandée par l'humain : une nouvelle délégation, limitée aux écarts.
                        Verdict::Retry(note) => to_split = corrective_task(&task, &findings, &note),
                        Verdict::Waive => break,
                        Verdict::Stop => {
                            sh.halted.store(true, std::sync::atomic::Ordering::SeqCst);
                            unresolved.push(gap);
                            waived = false;
                            break;
                        }
                        Verdict::Nobody => {
                            unresolved.push(gap);
                            waived = false;
                            break;
                        }
                    }
                }
            }
        }

        let rep = sh.act(i, phase, Kind::Report, report_prompt(&task, &history, &sections), &kids, &sc, &sections, None, None).await;
        let mut blockers: Vec<Blocker> = unresolved.iter().flat_map(|(_, _, b)| b.clone()).collect();
        if unresolved.is_empty() && rep.status == Status::Done {
            return SeatOut { status: Status::Done, text: rep.report, blockers: vec![], waived: false };
        }
        blockers.extend(rep.blockers);
        let status = if unresolved.is_empty() && rep.status == Status::Failed { Status::Failed } else { Status::Blocked };
        // Tout ce qui reste non résolu a été écarté par l'humain : la branche n'arrête pas ses voisines.
        SeatOut { status, text: rep.report, blockers, waived: waived && !unresolved.is_empty() }
    })
}
