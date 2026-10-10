//! Schémas JSON des sorties structurées et lecture des réponses (statut, délégation, plan de correction).

use super::{Assignment, Blocker, Kind, Outcome, Status};
use crate::model::company::Company;
use crate::runtime::executor::{NodeResult, NodeStatus};
use crate::runtime::scope;
use anyhow::{Result, bail};
use serde_json::{Value, json};

/// Schéma de sortie des appels de travail et de rapport.
pub(super) fn outcome_schema() -> String {
    json!({
        "type": "object",
        "properties": {
            "status": {"type": "string", "enum": ["done", "blocked", "failed"]},
            "report": {"type": "string"},
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
        "required": ["status", "report"]
    })
    .to_string()
}

/// Lit le résultat d'un appel de travail/rapport. `None` : sortie structurée absente ou illisible.
/// Un refus de permission est toujours un blocage, même si l'agent se déclare « done ».
pub(super) fn parse_outcome(r: &NodeResult) -> Option<Outcome> {
    if r.is_error {
        return Some(Outcome::failed(r.output.clone()));
    }
    let v = r.structured.as_ref()?;
    let mut status = match v["status"].as_str()? {
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
    let mut seen: Vec<&String> = vec![];
    let mut denied: Vec<Blocker> = vec![];
    for d in &r.denials {
        if !seen.contains(&d) && denied.len() < 5 {
            seen.push(d);
            denied.push(Blocker { kind: "permission".into(), detail: format!("refused by permissions: {d}"), needs: "authorize it (allow_bash / scope) or give it to a seat that has it".into() });
        }
    }
    if !denied.is_empty() {
        blockers.extend(denied);
        if status == Status::Done {
            status = Status::Blocked;
        }
    }
    let report = text(&v["report"]);
    if status != Status::Done && blockers.is_empty() {
        let detail = report.lines().next().unwrap_or("not completed").to_string();
        blockers.push(Blocker { kind: "other".into(), detail, needs: String::new() });
    }
    Some(Outcome { status, report, blockers })
}

/// Statut affiché d'un nœud, d'après son résultat.
pub(super) fn node_status(kind: Kind, r: &NodeResult) -> NodeStatus {
    if r.is_error {
        return NodeStatus::Failed(r.output.clone());
    }
    if !matches!(kind, Kind::Work | Kind::Report) {
        return NodeStatus::Succeeded;
    }
    match parse_outcome(r) {
        Some(o) if o.status == Status::Done => NodeStatus::Succeeded,
        Some(o) if o.status == Status::Blocked => NodeStatus::Blocked(o.blockers.iter().map(Blocker::line).collect::<Vec<_>>().join("\n")),
        Some(o) => NodeStatus::Failed(o.report),
        None => NodeStatus::Failed("structured result missing".into()),
    }
}

/// Schéma JSON de la délégation : `to` restreint aux ids des subordonnés directs.
pub(super) fn delegation_schema(kid_ids: &[String], with_scope: bool) -> String {
    let mut props = json!({
        "to": {"type": "string", "enum": kid_ids},
        "task": {"type": "string"}
    });
    let mut required = vec!["to", "task"];
    if with_scope {
        props["scope"] = json!({"type": "array", "minItems": 1, "items": {"type": "string"}});
        required.push("scope");
    }
    json!({
        "type": "object",
        "properties": {
            "assignments": {
                "type": "array",
                "minItems": 1,
                "items": {"type": "object", "properties": props, "required": required}
            }
        },
        "required": ["assignments"]
    })
    .to_string()
}

/// Schéma du plan de correction : `plan`, puis des affectations correctives ou une remontée (`escalate`).
pub(super) fn fix_schema(kid_ids: &[String], with_scope: bool) -> String {
    let mut v: Value = serde_json::from_str(&delegation_schema(kid_ids, with_scope)).expect("schéma valide");
    v["properties"]["assignments"].as_object_mut().map(|a| a.remove("minItems"));
    v["properties"]["plan"] = json!({"type": "string"});
    v["properties"]["escalate"] = json!({"type": "string"});
    v["required"] = json!(["plan"]);
    v.to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum FixPlan {
    Assign { plan: String, assignments: Vec<Assignment> },
    /// Personne ne peut corriger dans cette branche : on remonte, avec la raison.
    Escalate(String),
}

/// Lit le plan de correction : des affectations valides priment ; sinon la remontée.
pub(super) fn parse_fix(v: &Value, c: &Company, kids: &[usize], parent_scope: Option<&[String]>) -> FixPlan {
    let reason = v["escalate"].as_str().map(str::trim).filter(|r| !r.is_empty());
    match parse_assignments(v, c, kids, parent_scope) {
        Ok(assignments) => FixPlan::Assign { plan: v["plan"].as_str().unwrap_or_default().to_string(), assignments },
        Err(e) => FixPlan::Escalate(reason.map(String::from).unwrap_or_else(|| e.to_string())),
    }
}

/// Lit la sortie structurée : ignore les destinataires inconnus, fusionne les doublons.
/// `parent_scope` : en phase de réalisation, périmètre du délégant ; chaque `scope` enfant doit y être inclus
/// (les motifs hors périmètre sont écartés ; une affectation sans motif valide est ignorée).
pub(super) fn parse_assignments(v: &Value, c: &Company, kids: &[usize], parent_scope: Option<&[String]>) -> Result<Vec<Assignment>> {
    let arr = v["assignments"].as_array().ok_or_else(|| anyhow::anyhow!("pas de liste « assignments »"))?;
    let mut out: Vec<Assignment> = vec![];
    for a in arr {
        let (Some(to), Some(task)) = (a["to"].as_str(), a["task"].as_str()) else { continue };
        let Some(idx) = kids.iter().copied().find(|&k| c.seats[k].id == to) else { continue };
        if task.trim().is_empty() {
            continue;
        }
        let mut sc: Vec<String> = vec![];
        if let Some(parent) = parent_scope {
            let wanted: Vec<String> = a["scope"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(scope::normalize).collect();
            // Poste ⚒ : jamais « tout le projet » (`**` = écriture sans restriction) ni chemin réservé (.claude, .git…).
            let builder = c.seats[idx].builds;
            sc = wanted
                .into_iter()
                .filter(|w| scope::all_covered(parent, std::slice::from_ref(w)))
                .filter(|w| !(builder && (scope::is_everything(w) || scope::is_sensitive(w))))
                .collect();
            sc.dedup();
            if sc.is_empty() {
                continue; // aucun motif dans le périmètre du délégant
            }
        }
        match out.iter_mut().find(|x| x.to == idx) {
            Some(x) => {
                x.task.push_str("\n\n");
                x.task.push_str(task.trim());
                for m in sc {
                    if !x.scope.contains(&m) {
                        x.scope.push(m);
                    }
                }
            }
            None => out.push(Assignment { to: idx, task: task.trim().to_string(), scope: sc }),
        }
    }
    if out.is_empty() {
        bail!("no valid delegation to a direct subordinate{}", if parent_scope.is_some() { " (with a scope included in yours)" } else { "" });
    }
    Ok(out)
}
