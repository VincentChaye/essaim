//! Schémas JSON des sorties structurées et lecture des réponses (statut, délégation, plan de correction).

use super::{Assignment, Blocker, Kind, Outcome, Section, Status};
use crate::model::company::Company;
use crate::runtime::executor::{NodeResult, NodeStatus};
use crate::runtime::scope;
use crate::runtime::textutil::{take_chars, truncate_middle};
use anyhow::{Result, bail};
use serde_json::{Value, json};

/// Modèles qu'un délégant peut choisir pour un poste (du moins cher au plus capable).
pub(super) const MODELS: [&str; 3] = ["haiku", "sonnet", "opus"];

/// Schéma de sortie des appels de travail et de rapport, **le même pour tous les postes et tous les appels**.
///
/// `claude` présente ce schéma au modèle comme un outil (`StructuredOutput`), et les outils ouvrent le
/// préfixe que le cache de prompt partage entre les appels. Mesuré sur `claude` 2.1.322 : outils et prompt
/// système commun font environ 6 000 jetons ; avec un schéma propre à chaque appel (un champ par section
/// demandée) ils étaient réécrits dans le cache à chaque appel, avec un schéma constant ils y sont relus.
/// Les sections voulues par le délégant sont donc décrites dans le prompt (`sections_hint`) et rendues dans
/// `sections`, une entrée par clé ; le schéma n'en nomme aucune.
///
/// Aucune borne `maxLength` non plus : `claude` valide la sortie structurée et renvoie une erreur d'outil au
/// moindre dépassement (« must NOT have more than N characters »), que le modèle ne sait pas éviter — il
/// compte mal les caractères. Les tailles sont annoncées dans le prompt et appliquées à la lecture.
pub(super) fn outcome_schema() -> String {
    json!({
        "type": "object",
        "properties": {
            "status": {"type": "string", "enum": ["done", "blocked", "failed"]},
            "report": {"type": "string", "description": "your answer, when the task asks for no sections"},
            "sections": {"type": "object", "additionalProperties": {"type": "string"}, "description": "when the task asks for sections: one entry per section, under the key the task gives"},
            "blockers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string", "enum": ["tests", "permission", "prerequisite", "bug", "scope", "other"]},
                        "detail": {"type": "string"},
                        "needs": {"type": "string"}
                    },
                    "required": ["kind", "detail"]
                }
            }
        },
        "required": ["status"]
    })
    .to_string()
}

/// Lit le résultat d'un appel de travail/rapport. `None` : sortie structurée absente ou illisible.
/// Le statut est celui que l'agent déclare : un refus de l'humain en cours de route (profil protégé) n'est
/// pas un blocage en soi, l'agent a pu faire autrement — c'est à lui de dire s'il a fini.
pub(super) fn parse_outcome(r: &NodeResult, sections: &[Section]) -> Option<Outcome> {
    if r.is_error {
        return Some(Outcome { session: r.session_id.clone(), ..Outcome::failed(r.output.clone()) });
    }
    let v = r.structured.as_ref()?;
    let status = match v["status"].as_str()? {
        "done" => Status::Done,
        "blocked" => Status::Blocked,
        "failed" => Status::Failed,
        _ => return None,
    };
    let text = |x: &Value| x.as_str().unwrap_or_default().trim().to_string();
    let mut blockers: Vec<Blocker> = v["blockers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| Blocker { kind: text(&b["kind"]).chars().take(20).collect(), detail: text(&b["detail"]), needs: text(&b["needs"]) })
        .filter(|b| !b.detail.is_empty())
        .collect();
    let filled = |s: &Section| !text(&v["sections"][&s.key]).is_empty();
    let report = if sections.is_empty() {
        // Rapport libre. Le schéma n'impose plus `report` : une réponse rangée dans `sections` est reprise telle quelle.
        Some(text(&v["report"])).filter(|t| !t.is_empty()).unwrap_or_else(|| {
            let parts = v["sections"].as_object().into_iter().flatten().map(|(k, x)| format!("### {k}\n{}", text(x)));
            parts.collect::<Vec<_>>().join("\n\n")
        })
    } else if !sections.iter().any(filled) && !text(&v["report"]).is_empty() {
        // Sections demandées, réponse rendue d'un bloc dans `report` : gardée, dans le budget total du gabarit.
        truncate_middle(&text(&v["report"]), sections.iter().map(|s| s.max_chars).sum())
    } else {
        // Sections dans l'ordre voulu par le délégant ; une section vide reste visible (« — »),
        // une section trop longue est tronquée (le budget du délégant tient, sans erreur d'outil).
        sections
            .iter()
            .map(|s| format!("### {}\n{}", s.title, Some(truncate_middle(&text(&v["sections"][&s.key]), s.max_chars)).filter(|t| !t.is_empty()).unwrap_or_else(|| "—".into())))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    if status != Status::Done && blockers.is_empty() {
        let detail = report.lines().next().unwrap_or("not completed").to_string();
        blockers.push(Blocker { kind: "other".into(), detail, needs: String::new() });
    }
    Some(Outcome { status, report, blockers, session: r.session_id.clone() })
}

/// Statut affiché d'un nœud, d'après son résultat.
pub(super) fn node_status(kind: Kind, r: &NodeResult, sections: &[Section]) -> NodeStatus {
    if r.is_error {
        return NodeStatus::Failed(r.output.clone());
    }
    if !matches!(kind, Kind::Work | Kind::Report) {
        return NodeStatus::Succeeded;
    }
    match parse_outcome(r, sections) {
        Some(o) if o.status == Status::Done => NodeStatus::Succeeded,
        Some(o) if o.status == Status::Blocked => NodeStatus::Blocked(o.blockers.iter().map(Blocker::line).collect::<Vec<_>>().join("\n")),
        Some(o) => NodeStatus::Failed(o.report),
        None => NodeStatus::Failed("structured result missing".into()),
    }
}

/// Schéma JSON de la délégation, **le même pour tous les postes** (voir `outcome_schema` : un schéma constant
/// laisse le cache de prompt servir le préfixe commun). `to` n'est donc pas restreint ici aux subordonnés
/// directs : la fiche de poste les nomme, et `parse_assignments` écarte tout autre destinataire.
/// `stage` : la vague, c'est-à-dire l'ordre entre subordonnés (1 d'abord). `context` : des pointeurs (fichiers, notes, URL) plutôt que
/// des copies. `scope` : facultatif, il ne sert qu'à ordonner les travaux qui toucheraient les mêmes fichiers.
pub(super) fn delegation_schema() -> String {
    json!({
        "type": "object",
        "properties": {
            "assignments": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "to": {"type": "string", "description": "id of one of your direct subordinates"},
                        "task": {"type": "string"},
                        "stage": {"type": "integer", "description": "1 = starts first (default); a later stage starts once every earlier stage is finished"},
                        "context": {"type": "array", "items": {"type": "string"}, "description": "pointers: project paths (optionally path:line-line), mission notes, URLs"},
                        "scope": {"type": "array", "items": {"type": "string"}, "description": "file patterns this subordinate will modify"},
                        "model": {"type": "string", "enum": MODELS},
                        "sections": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": {"type": "string"},
                                    "expect": {"type": "string"},
                                    "max_chars": {"type": "integer"}
                                },
                                "required": ["name", "expect"]
                            }
                        }
                    },
                    "required": ["to", "task"]
                }
            }
        },
        "required": ["assignments"]
    })
    .to_string()
}

/// Schéma de la revue : un verdict et des constats. Elle ne relance rien : l'humain décide de la suite.
pub(super) fn review_schema() -> String {
    json!({
        "type": "object",
        "properties": {
            "accepted": {"type": "boolean"},
            "findings": {"type": "string"}
        },
        "required": ["accepted", "findings"]
    })
    .to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Review {
    /// Objectif atteint (ou revue impossible : elle n'ajoute qu'un garde-fou, son absence ne fait pas échouer la mission).
    Accepted,
    /// Objectif non atteint : les écarts constatés, à présenter à l'humain.
    Unmet(String),
}

pub(super) fn parse_review(v: &Value) -> Review {
    if v["accepted"].as_bool() == Some(true) {
        return Review::Accepted;
    }
    Review::Unmet(v["findings"].as_str().map(str::trim).filter(|f| !f.is_empty()).unwrap_or("goal not reached").to_string())
}

/// Nombre de pointeurs de contexte conservés par affectation, et taille de chacun.
const MAX_POINTERS: usize = 12;
const POINTER_CHARS: usize = 300;

/// Tâche telle que le subordonné la reçoit : le texte du délégant, puis ses pointeurs de contexte.
fn with_context(task: &str, a: &Value) -> String {
    let pointers: Vec<String> = a["context"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(|x| take_chars(x.trim(), POINTER_CHARS)).filter(|x| !x.is_empty()).take(MAX_POINTERS).collect();
    if pointers.is_empty() {
        return task.to_string();
    }
    format!("{task}\n\nContext pointers from your supervisor (read what you need, look up the rest yourself):\n- {}", pointers.join("\n- "))
}

/// Deux vagues qui se suivent et ne mobilisent qu'un seul et même subordonné n'en font qu'une : ses affectations
/// deviennent des étapes successives, ce qui donne le même ordre. Sans cela une branche (manager, puis ses
/// subordonnés) referait délégation et rapport à chaque vague — mesuré sur une mission réelle : 36 appels au
/// lieu des 16 estimés, un supérieur ayant donné trois vagues au même manager.
/// `sorted` : affectations triées par vague, au plus une par (poste, vague).
fn merge_solo_stages(sorted: Vec<Assignment>) -> Vec<Assignment> {
    // Vagues qui ne portent qu'une affectation.
    let solo = |stage: u32| sorted.iter().filter(|a| a.stage == stage).count() == 1;
    let mut out: Vec<Assignment> = vec![];
    for a in &sorted {
        // `last.stage` reste celui de la première vague fusionnée : seule, elle aussi, par construction.
        let merge = out.last().is_some_and(|last| last.to == a.to && solo(last.stage) && solo(a.stage));
        match out.last_mut() {
            Some(last) if merge => {
                last.steps.extend(a.steps.iter().cloned());
                last.task.push_str("\n\n");
                last.task.push_str(&a.task);
                last.model = last.model.clone().or_else(|| a.model.clone());
                if last.sections.is_empty() {
                    last.sections = a.sections.clone();
                }
                for m in &a.scope {
                    if !last.scope.contains(m) {
                        last.scope.push(m.clone());
                    }
                }
            }
            _ => out.push(a.clone()),
        }
    }
    out
}

/// Vague la plus tardive acceptée d'un délégant (au-delà, la valeur est ramenée à cette borne).
const MAX_STAGE: u64 = 20;

/// Lit la sortie structurée : ignore les destinataires inconnus, regroupe les affectations d'un même poste et
/// d'une même vague en étapes, et rend les affectations dans l'ordre des vagues (`stage`).
/// `parent_scope` : périmètre du délégant. Le périmètre ne donne plus de droits : il sert à ne pas faire
/// travailler deux postes sur les mêmes fichiers en même temps. Les motifs hors du périmètre du délégant sont
/// écartés. Sans motif valide, un poste qui réalise (ou qui dirige une branche) hérite du périmètre du
/// délégant — il passera donc après les autres — et un poste d'analyse n'en a aucun : il ne gêne personne.
pub(super) fn parse_assignments(v: &Value, c: &Company, kids: &[usize], parent_scope: &[String]) -> Result<Vec<Assignment>> {
    let arr = v["assignments"].as_array().ok_or_else(|| anyhow::anyhow!("pas de liste « assignments »"))?;
    let mut out: Vec<Assignment> = vec![];
    for a in arr {
        let (Some(to), Some(task)) = (a["to"].as_str(), a["task"].as_str()) else { continue };
        let Some(idx) = kids.iter().copied().find(|&k| c.seats[k].id == to) else { continue };
        if task.trim().is_empty() {
            continue;
        }
        let mut sc: Vec<String> = a["scope"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str())
            .map(scope::normalize)
            .filter(|w| scope::all_covered(parent_scope, std::slice::from_ref(w)))
            .collect();
        sc.dedup();
        if sc.is_empty() && (c.seats[idx].builds || !c.is_leaf(idx)) {
            sc = parent_scope.to_vec();
        }
        let sections = Section::template(a["sections"].as_array().into_iter().flatten().map(|s| {
            (s["name"].as_str().unwrap_or_default().to_string(), s["expect"].as_str().unwrap_or_default().to_string(), s["max_chars"].as_u64())
        }));
        let model = a["model"].as_str().map(str::trim).filter(|m| MODELS.contains(m)).map(String::from);
        let step = with_context(task.trim(), a);
        // Vague absente, nulle ou illisible : la première.
        let stage = a["stage"].as_u64().unwrap_or(1).clamp(1, MAX_STAGE) as u32;
        // Plusieurs affectations au même subordonné dans la même vague = plusieurs **étapes successives**,
        // pas une tâche fusionnée : chacune repart d'une session neuve, ce qui garde les contextes petits.
        // `task` reste leur concaténation, pour les rapports et l'historique du supérieur. D'une vague à
        // l'autre, le même poste reçoit des affectations distinctes : d'autres travaillent entre les deux.
        match out.iter_mut().find(|x| x.to == idx && x.stage == stage) {
            Some(x) => {
                x.model = x.model.clone().or(model);
                if x.sections.is_empty() {
                    x.sections = sections;
                }
                x.steps.push(step);
                x.task.push_str("\n\n");
                x.task.push_str(task.trim());
                for m in sc {
                    if !x.scope.contains(&m) {
                        x.scope.push(m);
                    }
                }
            }
            None => out.push(Assignment { sections, to: idx, stage, steps: vec![step], task: task.trim().to_string(), scope: sc, model }),
        }
    }
    if out.is_empty() {
        let ids: Vec<&str> = kids.iter().map(|&k| c.seats[k].id.as_str()).collect();
        bail!("no valid delegation to a direct subordinate (`to` must be one of: {})", ids.join(", "));
    }
    // Tri stable : dans une vague, l'ordre d'écriture du délégant est conservé.
    out.sort_by_key(|a| a.stage);
    let out = merge_solo_stages(out);
    // Plusieurs vagues : chaque poste apprend que sa tâche n'est qu'une part d'un ensemble ordonné. Sans cela il
    // se déclare bloqué pour un travail prévu plus tard (« tests non lancés »), ce qui arrête les vagues suivantes.
    let mut stages: Vec<u32> = out.iter().map(|a| a.stage).collect();
    stages.dedup();
    let mut out = out;
    if stages.len() > 1 {
        for a in &mut out {
            let rank = stages.iter().position(|&s| s == a.stage).map_or(1, |k| k + 1);
            let note = if rank < stages.len() {
                format!("\n\n(Stage {rank} of {} in your supervisor's plan. Work it keeps for a later stage is not yours now: do not start it, and do not report \"blocked\" because it is still to come — judge \"done\" on this task alone.)", stages.len())
            } else {
                format!("\n\n(Stage {rank} of {}, the last, in your supervisor's plan: the earlier stages are finished, build on their results.)", stages.len())
            };
            for step in &mut a.steps {
                step.push_str(&note);
            }
        }
    }
    Ok(out)
}
