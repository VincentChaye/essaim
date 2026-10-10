//! Pilotage d'une mission : phase de conception, point de contrôle, phase de réalisation, clôture du run.

use super::seat::run_seat;
use super::shared::Shared;
use super::{CompanyOptions, MissionOutcome, Phase, PhaseOutcome};
use crate::model::agent::AgentDef;
use crate::model::company::Company;
use crate::runtime::executor::{RunEvent, Runner, cancelled};
use crate::runtime::missions;
use crate::tf;
use anyhow::{Result, bail};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, mpsc, watch};

/// Lance une phase complète depuis le boss. `calls` est le compteur d'appels de toute la mission.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_phase(
    company: &Company,
    agents: &[AgentDef],
    brief: &str,
    phase: Phase,
    root_task: &str,
    opts: &CompanyOptions,
    runner: Runner,
    tx: mpsc::UnboundedSender<RunEvent>,
    cancel: watch::Receiver<bool>,
    calls: Arc<AtomicU32>,
) -> Result<PhaseOutcome> {
    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    company.validate(&names)?;
    let Some(root) = company.root() else { bail!("org chart without a single boss") };
    let before = calls.load(Ordering::SeqCst);
    let sh = Arc::new(Shared {
        company: company.clone(),
        agents: agents.to_vec(),
        brief: brief.to_string(),
        opts: opts.clone(),
        runner,
        tx,
        cancel,
        sem: Semaphore::new(company.max_parallel.max(1)),
        calls,
        cost: Mutex::new(0),
    });
    let root_scope = if phase == Phase::Realisation { vec!["**".to_string()] } else { vec![] };
    let out = run_seat(sh.clone(), root, phase, root_task.to_string(), root_scope).await;
    let cost = *sh.cost.lock().unwrap();
    let used = sh.calls.load(Ordering::SeqCst).min(company.max_calls).saturating_sub(before);
    let ok = out.ok();
    Ok(PhaseOutcome { ok, report: out.text, blockers: if ok { vec![] } else { out.blockers }, tokens: cost, calls: used })
}

const CONCEPTION_TASK: &str = "Lead the design phase of the mission described in the brief: have each domain framed by the right lead.";

fn realisation_task(plan: &str) -> String {
    format!("Here is the consolidated plan, approved at the end of design:\n{plan}\n\nLead its setup in the project: split the work across your realisation branch.")
}

/// Note affichée quand la réalisation est refusée au point de contrôle : le run est « terminé » (code 0)
/// sans aucune modification du projet ; sans cette note, l'écran ne le distingue pas d'une réalisation réussie.
pub fn note_refus(approved: Option<bool>) -> Option<String> {
    (approved == Some(false)).then(|| {
        tf!(
            "réalisation non lancée : plan refusé au point de contrôle, aucun fichier du projet modifié",
            "build not started: plan refused at the checkpoint, no project file changed"
        )
    })
}

/// Mission complète : conception → (point de contrôle) → réalisation. `RunFinished` clôt le run.
pub async fn run_mission(
    company: &Company,
    agents: &[AgentDef],
    brief: &str,
    opts: &CompanyOptions,
    runner: Runner,
    tx: mpsc::UnboundedSender<RunEvent>,
    mut cancel: watch::Receiver<bool>,
) -> Result<MissionOutcome> {
    let calls = Arc::new(AtomicU32::new(0));
    if let Some(d) = &opts.mission_dir {
        missions::mark_done(d, false); // un run neuf invalide l'ancien succès
    }
    let save = |name: &str, text: &str| -> Result<()> {
        match &opts.mission_dir {
            Some(d) => missions::write_output(d, name, text),
            None => Ok(()),
        }
    };
    // Échec d'écriture (livrable ou marque de conception) : le run se ferme en échec, puis l'erreur remonte à l'appelant.
    let fail = |e: anyhow::Error, tokens: u64| {
        // Envoi sans succès possible seulement si le récepteur est fermé : le run se termine de toute façon.
        let _ = tx.send(RunEvent::RunFinished { ok: false, tokens });
        e
    };

    let resumed_plan = if opts.resume { opts.mission_dir.as_deref().and_then(missions::resumable_plan) } else { None };
    let conception = if let Some(plan) = resumed_plan {
        PhaseOutcome { ok: true, report: plan, blockers: vec![], tokens: 0, calls: 0 }
    } else {
        if let Some(d) = &opts.mission_dir {
            missions::clear_conception_mark(d); // un run neuf invalide l'ancien plan
        }
        let c = match run_phase(company, agents, brief, Phase::Conception, CONCEPTION_TASK, opts, runner.clone(), tx.clone(), cancel.clone(), calls.clone()).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(RunEvent::RunFinished { ok: false, tokens: 0 });
                return Err(e);
            }
        };
        save("conception.md", &c.report).map_err(|e| fail(e, c.tokens))?;
        if c.ok && !*cancel.borrow() {
            if let Some(d) = &opts.mission_dir {
                missions::mark_conception_done(d).map_err(|e| fail(e, c.tokens))?;
            }
        }
        c
    };

    let mut outcome = MissionOutcome {
        ok: conception.ok,
        report: conception.report.clone(),
        tokens: conception.tokens,
        calls: conception.calls,
        conception: conception.clone(),
        realisation: None,
        approved: None,
    };

    let has_builders = company.seats.iter().any(|s| s.builds);
    let cancelled_now = *cancel.borrow();
    // Conception non réussie : on ne construit jamais sur une base bloquée (l'utilisateur a déjà pu intervenir).
    if has_builders && !cancelled_now && conception.ok {
        // Point de contrôle : le plan est validé avant de toucher au projet.
        let approved = if company.checkpoint {
            let (rtx, mut rrx) = mpsc::unbounded_channel::<bool>();
            let _ = tx.send(RunEvent::Checkpoint { plan: conception.report.clone(), reply: rtx });
            tokio::select! {
                r = rrx.recv() => r.unwrap_or(false),
                _ = cancelled(&mut cancel) => false,
            }
        } else {
            true
        };
        outcome.approved = Some(approved);
        if approved {
            // Le plan peut avoir été retouché pendant le point de contrôle (conversation avec le boss) : le fichier fait foi.
            let plan = opts
                .mission_dir
                .as_ref()
                .and_then(|d| std::fs::read_to_string(d.join("conception.md")).ok())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| conception.report.clone());
            let task = realisation_task(&plan);
            match run_phase(company, agents, brief, Phase::Realisation, &task, opts, runner, tx.clone(), cancel.clone(), calls).await {
                Ok(r) => {
                    save("rapport.md", &r.report).map_err(|e| fail(e, outcome.tokens + r.tokens))?;
                    outcome.ok = conception.ok && r.ok;
                    outcome.report = r.report.clone();
                    outcome.tokens += r.tokens;
                    outcome.calls += r.calls;
                    outcome.realisation = Some(r);
                }
                Err(e) => {
                    outcome.ok = false;
                    let _ = tx.send(RunEvent::RunFinished { ok: false, tokens: outcome.tokens });
                    return Err(e);
                }
            }
        } else {
            // Réalisation refusée : la mission s'arrête sur le plan, sans que ce soit un échec.
            outcome.ok = conception.ok;
        }
    }
    // Réussie = menée à terme : conception ok, et réalisation faite si l'entreprise en a une (un plan refusé n'est pas un succès).
    if let Some(d) = &opts.mission_dir {
        missions::mark_done(d, outcome.ok && (!has_builders || outcome.realisation.is_some()));
    }
    let _ = tx.send(RunEvent::RunFinished { ok: outcome.ok, tokens: outcome.tokens });
    Ok(outcome)
}
