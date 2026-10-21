//! Schémas JSON des sorties structurées et lecture des réponses (statut, délégation, plan de correction).

use super::{Assignment, Blocker, Kind, Outcome, Question, Section, Status};
use crate::model::company::Company;
use crate::runtime::executor::{NodeResult, NodeStatus};
use crate::runtime::scope;
use anyhow::{Result, bail};
use serde_json::{Value, json};

/// Modèles qu'un délégant peut choisir pour un poste (du moins cher au plus capable).
pub(super) const MODELS: [&str; 3] = ["haiku", "sonnet", "opus"];

/// Schéma de sortie des appels de travail et de rapport. Avec un gabarit, `report` est remplacé par un champ
/// obligatoire par section (taille bornée) : la forme de la réponse est celle que le délégant a demandée.
/// `ask` : l'employé peut poser une question cadrée (statut `question`) et attendre la réponse de son supérieur.
pub(super) fn outcome_schema(sections: &[Section], ask: bool) -> String {
    let mut v = outcome_base();
    if ask {
        v["properties"]["status"]["enum"] = json!(["done", "blocked", "failed", "question"]);
        v["properties"]["question"] = json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string", "enum": ["missing_info", "decision", "permission", "ambiguity"]},
                "text": {"type": "string", "maxLength": QUESTION_CHARS},
                "options": {"type": "array", "items": {"type": "string", "maxLength": 60}}
            },
            "required": ["kind", "text"]
        });
    }
    if !sections.is_empty() {
        let props: serde_json::Map<String, Value> =
            sections.iter().map(|s| (s.key.clone(), json!({"type": "string", "maxLength": s.max_chars, "description": s.expect}))).collect();
        v["properties"].as_object_mut().map(|p| p.remove("report"));
        v["properties"]["sections"] = json!({"type": "object", "properties": props, "required": sections.iter().map(|s| s.key.clone()).collect::<Vec<_>>()});
        v["required"] = json!(["status", "sections"]);
    }
    if ask {
        // Un statut `question` n'a ni rapport ni sections à rendre : seul le statut reste obligatoire.
        v["required"] = json!(["status"]);
    }
    v.to_string()
}

/// Taille maximale d'une question d'employé et d'une réponse de supérieur.
pub(super) const QUESTION_CHARS: usize = 150;
pub(super) const ANSWER_CHARS: usize = 300;

/// Schéma de la réponse d'un supérieur : une réponse courte, ou l'aveu qu'il ne peut pas répondre.
pub(super) fn answer_schema() -> String {
    json!({
        "type": "object",
        "properties": {
            "can_answer": {"type": "boolean"},
            "answer": {"type": "string", "maxLength": ANSWER_CHARS}
        },
        "required": ["can_answer", "answer"]
    })
    .to_string()
}

/// Réponse retenue : `None` si le supérieur ne peut pas répondre (la question devient un blocage remonté).
pub(super) fn parse_answer(r: &NodeResult) -> Option<String> {
    let v = r.structured.as_ref().filter(|_| !r.is_error)?;
    let a = v["answer"].as_str()?.trim();
    (v["can_answer"].as_bool() == Some(true) && !a.is_empty()).then(|| a.to_string())
}

fn outcome_base() -> Value {
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
}

/// Lit le résultat d'un appel de travail/rapport. `None` : sortie structurée absente ou illisible.
/// Un refus de permission est toujours un blocage, même si l'agent se déclare « done ».
pub(super) fn parse_outcome(r: &NodeResult, sections: &[Section]) -> Option<Outcome> {
    if r.is_error {
        return Some(Outcome::failed(r.output.clone()));
    }
    let v = r.structured.as_ref()?;
    let mut status = match v["status"].as_str()? {
        "done" => Status::Done,
        "blocked" => Status::Blocked,
        "failed" => Status::Failed,
        "question" => Status::Blocked,
        _ => return None,
    };
    let question = (v["status"] == "question").then(|| Question {
        kind: v["question"]["kind"].as_str().unwrap_or("ambiguity").to_string(),
        text: v["question"]["text"].as_str().unwrap_or_default().trim().to_string(),
        options: v["question"]["options"].as_array().into_iter().flatten().filter_map(|o| o.as_str()).map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).take(4).collect(),
    });
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
    // Question sans texte : rien à répondre, c'est un blocage ordinaire.
    let question = question.filter(|q| !q.text.is_empty());
    if let Some(q) = &question {
        blockers.push(Blocker { kind: "other".into(), detail: format!("question ({}): {}", q.kind, q.text), needs: "an answer from the supervisor".into() });
    }
    let report = if sections.is_empty() {
        text(&v["report"])
    } else {
        // Sections dans l'ordre voulu par le délégant ; une section vide reste visible (« — »).
        sections
            .iter()
            .map(|s| format!("### {}\n{}", s.title, Some(text(&v["sections"][&s.key])).filter(|t| !t.is_empty()).unwrap_or_else(|| "—".into())))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    if status != Status::Done && blockers.is_empty() {
        let detail = report.lines().next().unwrap_or("not completed").to_string();
        blockers.push(Blocker { kind: "other".into(), detail, needs: String::new() });
    }
    Some(Outcome { status, report, blockers, question })
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

/// Schéma JSON de la délégation : `to` restreint aux ids des subordonnés directs.
pub(super) fn delegation_schema(kid_ids: &[String], with_scope: bool) -> String {
    delegation_value(kid_ids, with_scope).to_string()
}

fn delegation_value(kid_ids: &[String], with_scope: bool) -> Value {
    let mut props = json!({
        "to": {"type": "string", "enum": kid_ids},
        "task": {"type": "string"},
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
}

/// Schéma du plan de correction : `plan`, puis des affectations correctives ou une remontée (`escalate`).
pub(super) fn fix_schema(kid_ids: &[String], with_scope: bool) -> String {
    let mut v = delegation_value(kid_ids, with_scope);
    v["properties"]["assignments"].as_object_mut().map(|a| a.remove("minItems"));
    v["properties"]["plan"] = json!({"type": "string"});
    v["properties"]["escalate"] = json!({"type": "string"});
    v["required"] = json!(["plan"]);
    v.to_string()
}

/// Schéma de la revue : verdict, constats, puis (si le résultat n'est pas conforme) les tâches de la redescente.
pub(super) fn review_schema(kid_ids: &[String], with_scope: bool) -> String {
    let mut v = delegation_value(kid_ids, with_scope);
    v["properties"]["assignments"].as_object_mut().map(|a| a.remove("minItems"));
    v["properties"]["accepted"] = json!({"type": "boolean"});
    v["properties"]["findings"] = json!({"type": "string"});
    v["required"] = json!(["accepted", "findings"]);
    v.to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ReviewPlan {
    /// Objectif atteint (ou revue impossible : elle n'ajoute qu'un garde-fou, son absence ne fait pas échouer la mission).
    Accepted,
    /// Objectif non atteint : tâches complémentaires pour les subordonnés compétents.
    Redo { findings: String, assignments: Vec<Assignment> },
    /// Objectif non atteint et rien à relancer dans la branche : à décider avec l'utilisateur.
    Unmet(String),
}

pub(super) fn parse_review(v: &Value, c: &Company, kids: &[usize], parent_scope: Option<&[String]>) -> ReviewPlan {
    if v["accepted"].as_bool() == Some(true) {
        return ReviewPlan::Accepted;
    }
    let findings = v["findings"].as_str().map(str::trim).filter(|f| !f.is_empty()).unwrap_or("goal not reached").to_string();
    match parse_assignments(v, c, kids, parent_scope) {
        Ok(assignments) => ReviewPlan::Redo { findings, assignments },
        Err(_) => ReviewPlan::Unmet(findings),
    }
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
        let sections = Section::template(a["sections"].as_array().into_iter().flatten().map(|s| {
            (s["name"].as_str().unwrap_or_default().to_string(), s["expect"].as_str().unwrap_or_default().to_string(), s["max_chars"].as_u64())
        }));
        let model = a["model"].as_str().map(str::trim).filter(|m| MODELS.contains(m)).map(String::from);
        match out.iter_mut().find(|x| x.to == idx) {
            Some(x) => {
                x.model = x.model.clone().or(model);
                if x.sections.is_empty() {
                    x.sections = sections;
                }
                x.task.push_str("\n\n");
                x.task.push_str(task.trim());
                for m in sc {
                    if !x.scope.contains(&m) {
                        x.scope.push(m);
                    }
                }
            }
            None => out.push(Assignment { sections, to: idx, task: task.trim().to_string(), scope: sc, model }),
        }
    }
    if out.is_empty() {
        bail!("no valid delegation to a direct subordinate{}", if parent_scope.is_some() { " (with a scope included in yours)" } else { "" });
    }
    Ok(out)
}
