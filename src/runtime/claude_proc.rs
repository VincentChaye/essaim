use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Événement normalisé issu du flux `--output-format stream-json`.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeEvent {
    Init {
        session_id: String,
        model: String,
        /// "none" = authentification par abonnement (OAuth) ; autre = clé API.
        api_key_source: String,
    },
    Text(String),
    ToolUse { name: String, input: Value },
    /// `message` : cause renvoyée par l'outil, seulement quand `is_error` (bornée à `TOOL_ERROR_CHARS`).
    ToolResult { is_error: bool, message: String },
    Result {
        text: String,
        is_error: bool,
        /// Coût **pondéré** (voir `weigh`) : c'est le chiffre cumulé et affiché.
        tokens: u64,
        /// Volume brut (somme des quatre compteurs), montré en second pour expliquer le coût.
        volume: u64,
        /// Jetons servis par le cache de prompt (`cache_read_input_tokens`), part du volume.
        cached: u64,
        duration_ms: u64,
        /// Sortie structurée (avec `--json-schema`), déjà validée par `claude`.
        structured: Option<Value>,
        /// Appels d'outils refusés par les permissions (« Bash(git status) »…) : jamais passés sous silence.
        denials: Vec<String>,
    },
    /// Usage d'un message `assistant` (même `id` = même message, valeurs à remplacer et non à cumuler) : suivi en temps réel.
    /// `tokens` est pondéré, `volume` est brut.
    Usage { id: String, tokens: u64, volume: u64, cached: u64 },
    /// Quota de l'abonnement (`rate_limit_event`) : utilisation en fraction 0..1 des fenêtres 5 h et 7 j, reset en secondes epoch.
    RateLimit { five_hour: Option<f64>, seven_day: Option<f64>, resets_at: Option<u64> },
    /// Demande de `claude` à son hôte (`control_request`) : il attend un `control_response` sur stdin et ne
    /// fait rien d'autre d'ici là. `subtype` vaut `can_use_tool` pour une demande de permission ; `tool`
    /// est alors l'outil (ou `SandboxNetworkAccess` pour un domaine réseau demandé depuis le bac à sable).
    Control { request_id: String, subtype: String, tool: String, input: Value },
    /// Ligne ignorée ou non reconnue.
    Other,
}

/// Désérialiseurs « tolérants » : une valeur d'un autre type que prévu vaut `None` (comme `Value::as_*` avant),
/// sans tampon intermédiaire (pas de `Value`).
mod lenient {
    use serde::Deserializer;
    use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
    use std::fmt;

    /// Scalaire JSON lu sans tampon ; tableaux et objets sont parcourus puis jetés.
    enum Scalar {
        Str(String),
        Bool(bool),
        Uint(u64),
        Int(i64),
        Float(f64),
        Other,
    }

    struct ScalarVisitor;

    impl<'de> Visitor<'de> for ScalarVisitor {
        type Value = Scalar;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("une valeur JSON")
        }
        fn visit_str<E>(self, v: &str) -> Result<Scalar, E> {
            Ok(Scalar::Str(v.to_owned()))
        }
        fn visit_string<E>(self, v: String) -> Result<Scalar, E> {
            Ok(Scalar::Str(v))
        }
        fn visit_bool<E>(self, v: bool) -> Result<Scalar, E> {
            Ok(Scalar::Bool(v))
        }
        fn visit_u64<E>(self, v: u64) -> Result<Scalar, E> {
            Ok(Scalar::Uint(v))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Scalar, E> {
            Ok(Scalar::Int(v))
        }
        fn visit_f64<E>(self, v: f64) -> Result<Scalar, E> {
            Ok(Scalar::Float(v))
        }
        fn visit_unit<E>(self) -> Result<Scalar, E> {
            Ok(Scalar::Other)
        }
        fn visit_none<E>(self) -> Result<Scalar, E> {
            Ok(Scalar::Other)
        }
        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Scalar, D::Error> {
            d.deserialize_any(self)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Scalar, A::Error> {
            while s.next_element::<IgnoredAny>()?.is_some() {}
            Ok(Scalar::Other)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Scalar, A::Error> {
            while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(Scalar::Other)
        }
    }

    pub fn string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        Ok(match d.deserialize_any(ScalarVisitor)? {
            Scalar::Str(s) => Some(s),
            _ => None,
        })
    }

    pub fn boolean<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
        Ok(match d.deserialize_any(ScalarVisitor)? {
            Scalar::Bool(b) => Some(b),
            _ => None,
        })
    }

    pub fn unsigned<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        Ok(match d.deserialize_any(ScalarVisitor)? {
            Scalar::Uint(n) => Some(n),
            _ => None,
        })
    }

    /// Comme `Value::as_f64` : tout nombre convient.
    pub fn float<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        Ok(match d.deserialize_any(ScalarVisitor)? {
            Scalar::Float(n) => Some(n),
            Scalar::Int(n) => Some(n as f64),
            Scalar::Uint(n) => Some(n as f64),
            _ => None,
        })
    }

    /// Caractères gardés d'un contenu d'outil, à la lecture. Un résultat d'outil réussi (lecture de fichier,
    /// sortie de commande) pèse parfois des mégaoctets : seul un début borné entre en mémoire.
    const RAW_TEXT_CAP: usize = 1000;

    /// Chaîne tronquée dès la désérialisation ; toute autre valeur vaut la chaîne vide.
    pub struct Capped(pub String);

    impl<'de> serde::Deserialize<'de> for Capped {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            Ok(Capped(match d.deserialize_any(ScalarVisitor)? {
                Scalar::Str(s) => s.chars().take(RAW_TEXT_CAP).collect(),
                _ => String::new(),
            }))
        }
    }

    /// Bloc d'un contenu d'outil en liste : seul `text` est lu, tronqué.
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct TextBlock {
        text: Option<Capped>,
    }

    /// Contenu d'un `tool_result` : chaîne, ou blocs `{type, text}` concaténés. Tronqué à la lecture.
    pub fn capped_text<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Option<String>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("un contenu d'outil")
            }
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Some(v.chars().take(RAW_TEXT_CAP).collect()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Self::Value, A::Error> {
                let mut out = String::new();
                while let Some(b) = s.next_element::<TextBlock>()? {
                    let Some(Capped(t)) = b.text else { continue };
                    if out.chars().count() >= RAW_TEXT_CAP {
                        continue; // le reste de la liste est lu puis jeté : le flux doit rester consommé
                    }
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&t);
                }
                Ok(Some(out))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> { Ok(None) }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> { Ok(None) }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> { Ok(None) }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> { Ok(None) }
            fn visit_unit<E>(self) -> Result<Self::Value, E> { Ok(None) }
            fn visit_none<E>(self) -> Result<Self::Value, E> { Ok(None) }
            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> { d.deserialize_any(self) }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(None)
            }
        }
        d.deserialize_any(V)
    }

    /// Tableau de `T` ; toute autre valeur vaut `None` (comme `Value::as_array`).
    pub fn array<'de, D: Deserializer<'de>, T: serde::Deserialize<'de>>(d: D) -> Result<Option<Vec<T>>, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: serde::Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Option<Vec<T>>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("un tableau")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Self::Value, A::Error> {
                let mut v = Vec::with_capacity(s.size_hint().unwrap_or(0).min(64));
                while let Some(x) = s.next_element()? {
                    v.push(x);
                }
                Ok(Some(v))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> { Ok(None) }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> { Ok(None) }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> { Ok(None) }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> { Ok(None) }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> { Ok(None) }
            fn visit_unit<E>(self) -> Result<Self::Value, E> { Ok(None) }
            fn visit_none<E>(self) -> Result<Self::Value, E> { Ok(None) }
            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> { d.deserialize_any(self) }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(None)
            }
        }
        d.deserialize_any(V(std::marker::PhantomData))
    }
}

/// Ligne du flux, limitée aux champs lus (le reste est ignoré sans être construit).
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawLine {
    #[serde(rename = "type", deserialize_with = "lenient::string")]
    kind: Option<String>,
    #[serde(deserialize_with = "lenient::string")]
    subtype: Option<String>,
    #[serde(deserialize_with = "lenient::string")]
    session_id: Option<String>,
    #[serde(deserialize_with = "lenient::string")]
    model: Option<String>,
    #[serde(rename = "apiKeySource", deserialize_with = "lenient::string")]
    api_key_source: Option<String>,
    message: Option<RawMessage>,
    #[serde(deserialize_with = "lenient::string")]
    result: Option<String>,
    #[serde(deserialize_with = "lenient::boolean")]
    is_error: Option<bool>,
    usage: Option<RawUsage>,
    #[serde(deserialize_with = "lenient::unsigned")]
    duration_ms: Option<u64>,
    structured_output: Option<Value>,
    #[serde(deserialize_with = "lenient::array")]
    permission_denials: Option<Vec<RawDenial>>,
    rate_limit_info: Option<RawRateInfo>,
    #[serde(deserialize_with = "lenient::string")]
    request_id: Option<String>,
    request: Option<Value>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawMessage {
    #[serde(deserialize_with = "lenient::string")]
    id: Option<String>,
    #[serde(deserialize_with = "lenient::array")]
    content: Option<Vec<RawBlock>>,
    usage: Option<RawUsage>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawBlock {
    #[serde(rename = "type", deserialize_with = "lenient::string")]
    kind: Option<String>,
    #[serde(deserialize_with = "lenient::string")]
    text: Option<String>,
    #[serde(deserialize_with = "lenient::string")]
    name: Option<String>,
    input: Value,
    #[serde(deserialize_with = "lenient::boolean")]
    is_error: Option<bool>,
    /// Contenu d'un `tool_result` : chaîne, ou liste de blocs `{type, text}`, tronqué dès la lecture.
    #[serde(deserialize_with = "lenient::capped_text")]
    content: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawUsage {
    #[serde(deserialize_with = "lenient::unsigned")]
    input_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient::unsigned")]
    output_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient::unsigned")]
    cache_creation_input_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient::unsigned")]
    cache_read_input_tokens: Option<u64>,
}

impl RawUsage {
    /// Volume brut : somme des quatre compteurs. Ce n'est **pas** un coût — le cache lu y pèse
    /// autant que l'entrée neuve alors qu'il coûte ~10 % du prix, et il représente en pratique
    /// plus de 90 % du volume d'un poste qui enchaîne les tours.
    fn volume(&self) -> u64 {
        [self.input_tokens, self.output_tokens, self.cache_creation_input_tokens, self.cache_read_input_tokens].into_iter().map(|x| x.unwrap_or(0)).sum()
    }

    /// Coût pondéré, en équivalents jetons d'entrée : entrée ×1, sortie ×1, cache lu ×0,1, cache écrit ×1,25.
    /// C'est ce chiffre qui est affiché et cumulé, parce qu'il suit ce que l'abonnement paie vraiment.
    fn weighted(&self) -> u64 {
        weigh(
            self.input_tokens.unwrap_or(0),
            self.output_tokens.unwrap_or(0),
            self.cache_read_input_tokens.unwrap_or(0),
            self.cache_creation_input_tokens.unwrap_or(0),
        )
    }

    fn cached(&self) -> u64 {
        self.cache_read_input_tokens.unwrap_or(0)
    }
}

/// Pondération commune à `claude_proc` et `usage` : un seul endroit où les poids sont écrits.
/// Entrée ×1, **sortie ×5**, cache lu ×0,1, cache écrit ×1,25 — les prix relatifs réels.
///
/// La sortie est à ×5 parce qu'elle coûte bien cela, et parce qu'elle n'est pas marginale : mesuré sur
/// une feuille qui refactorisait un fichier de 1 533 lignes (97 requêtes, contexte 9 k → 252 k), la
/// sortie faisait **24,8 %** du coût réel, deuxième poste derrière le cache lu (62,5 %). À ×1 elle
/// disparaissait de l'affichage, qui sous-estimait alors le coût de 20 %.
pub fn weigh(input: u64, output: u64, cache_read: u64, cache_write: u64) -> u64 {
    input + output * 5 + cache_read / 10 + cache_write * 5 / 4
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawDenial {
    #[serde(deserialize_with = "lenient::string")]
    tool_name: Option<String>,
    tool_input: Option<RawToolInput>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawToolInput {
    #[serde(deserialize_with = "lenient::string")]
    command: Option<String>,
    #[serde(deserialize_with = "lenient::string")]
    file_path: Option<String>,
    #[serde(deserialize_with = "lenient::string")]
    path: Option<String>,
    #[serde(deserialize_with = "lenient::string")]
    pattern: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawRateInfo {
    #[serde(rename = "unifiedWindows")]
    windows: RawWindows,
    #[serde(rename = "resetsAt", deserialize_with = "lenient::unsigned")]
    resets_at: Option<u64>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawWindows {
    five_hour: RawWindow,
    seven_day: RawWindow,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawWindow {
    #[serde(deserialize_with = "lenient::float")]
    utilization: Option<f64>,
    #[serde(rename = "resetsAt", deserialize_with = "lenient::unsigned")]
    resets_at: Option<u64>,
}

/// Longueur conservée de la cause d'une erreur d'outil : assez pour la lire, trop courte pour noyer le journal.
pub const TOOL_ERROR_CHARS: usize = 200;

/// Cause d'une erreur d'outil, sur une ligne et bornée.
/// Module inclus tel quel par les mesures (`tests/perf_baseline.rs`) : pas d'appel aux autres modules du crate.
fn tool_error_message(raw: &str) -> String {
    let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TOOL_ERROR_CHARS { line } else { format!("{}…", line.chars().take(TOOL_ERROR_CHARS - 1).collect::<String>()) }
}

/// « Outil(entrée) » d'un refus de permission, court et lisible.
fn denial_label(d: RawDenial) -> String {
    let tool = d.tool_name.unwrap_or_else(|| "?".into());
    let arg = d.tool_input.and_then(|i| [i.command, i.file_path, i.path, i.pattern].into_iter().flatten().next()).unwrap_or_default();
    let arg: String = arg.chars().take(80).collect();
    if arg.is_empty() { tool } else { format!("{tool}({arg})") }
}

/// Parse une ligne NDJSON en une liste d'événements (un message `assistant`
/// peut contenir plusieurs blocs). Désérialisation ciblée : seuls les champs utiles sont construits.
pub fn parse_line(line: &str) -> Result<Vec<ClaudeEvent>> {
    let v: RawLine = match serde_json::from_str(line) {
        Ok(v) => v,
        // JSON valide mais de forme inattendue (ex. un tableau) : ligne ignorée, comme avant.
        Err(e) if serde_json::from_str::<serde::de::IgnoredAny>(line).is_ok() => {
            let _ = e;
            return Ok(vec![ClaudeEvent::Other]);
        }
        Err(e) => return Err(e).context("ligne stream-json invalide"),
    };
    Ok(match v.kind.as_deref() {
        Some("system") if v.subtype.as_deref() == Some("init") => vec![ClaudeEvent::Init {
            session_id: v.session_id.unwrap_or_default(),
            model: v.model.unwrap_or_default(),
            api_key_source: v.api_key_source.unwrap_or_default(),
        }],
        Some("assistant") => {
            let m = v.message.unwrap_or_default();
            let Some(blocks) = m.content else { return Ok(vec![]) };
            let usage = m.usage.map(|u| ClaudeEvent::Usage { id: m.id.unwrap_or_default(), tokens: u.weighted(), volume: u.volume(), cached: u.cached() });
            usage
                .into_iter()
                .chain(blocks.into_iter().filter_map(|b| match b.kind.as_deref() {
                    Some("text") => Some(ClaudeEvent::Text(b.text.unwrap_or_default())),
                    Some("tool_use") => Some(ClaudeEvent::ToolUse { name: b.name.unwrap_or_default(), input: b.input }),
                    _ => None,
                }))
                .collect()
        }
        Some("control_request") => {
            let r = v.request.unwrap_or_default();
            vec![ClaudeEvent::Control {
                request_id: v.request_id.unwrap_or_default(),
                subtype: r["subtype"].as_str().unwrap_or_default().to_string(),
                tool: r["tool_name"].as_str().unwrap_or_default().to_string(),
                input: r["input"].clone(),
            }]
        }
        Some("rate_limit_event") => {
            let info = v.rate_limit_info.unwrap_or_default();
            vec![ClaudeEvent::RateLimit {
                five_hour: info.windows.five_hour.utilization,
                seven_day: info.windows.seven_day.utilization,
                resets_at: info.windows.five_hour.resets_at.or(info.resets_at),
            }]
        }
        Some("user") => v
            .message
            .unwrap_or_default()
            .content
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b.kind.as_deref() == Some("tool_result"))
            // Un tool_result réussi n'a pas de champ `is_error` (vu sur le flux réel : tests/fixtures) :
            // seul `true` explicite est une erreur d'outil. (Le `result` final, lui, garde « absent = échec ».)
            .map(|b| {
                let is_error = b.is_error.unwrap_or(false);
                // Le contenu n'est gardé que pour une erreur : un résultat réussi peut peser des milliers de lignes.
                ClaudeEvent::ToolResult { is_error, message: if is_error { tool_error_message(b.content.as_deref().unwrap_or_default()) } else { String::new() } }
            })
            .collect(),
        Some("result") => {
            let u = v.usage.unwrap_or_default();
            vec![ClaudeEvent::Result {
                text: v.result.unwrap_or_default(),
                // Absent = échec (R2) : un résultat sans statut explicite n'est jamais compté comme un succès.
                is_error: v.is_error.unwrap_or(true),
                tokens: u.weighted(),
                volume: u.volume(),
                cached: u.cached(),
                duration_ms: v.duration_ms.unwrap_or(0),
                structured: v.structured_output.filter(|x| !x.is_null()),
                denials: v.permission_denials.unwrap_or_default().into_iter().map(denial_label).collect(),
            }]
        }
        _ => vec![ClaudeEvent::Other],
    })
}

/// Limite de l'abonnement atteinte, telle que `claude` l'annonce (texte du résultat ou sortie d'erreur).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UsageLimit {
    /// Réinitialisation annoncée (epoch secondes) ; `None` si le message ne la donne pas.
    pub resets_at: Option<u64>,
}

/// Motifs qui désignent sans ambiguïté la limite de l'abonnement. Volontairement stricts : un agent qui
/// *parle* de limites d'API dans son rapport ne doit pas mettre le run en pause.
const LIMIT_PATTERNS: &[&str] = &[
    "usage limit reached",
    "rate_limit_error",
    "rate limit exceeded",
    "exceeded your rate limit",
    "5-hour limit reached",
    "weekly limit reached",
    "limite d'utilisation atteinte",
];

/// Plafond `--max-budget-usd` épuisé : `claude` arrête l'appel. Ce n'est **pas** la limite de
/// l'abonnement (aucune reprise n'y changerait rien) et ce n'est pas un échec du travail du poste :
/// c'est un réglage d'entreprise à relever. Reconnu pour pouvoir le dire clairement.
pub fn budget_exhausted(text: &str) -> bool {
    let low = text.to_lowercase();
    low.contains("budget limit reached") || (low.contains("budget") && low.contains("exceeded"))
}

/// Reconnaît le message de limite d'abonnement et en extrait l'heure de réinitialisation.
/// `None` = ce texte ne parle pas de la limite.
pub fn usage_limit(text: &str) -> Option<UsageLimit> {
    let low = text.to_lowercase();
    let hit = LIMIT_PATTERNS.iter().any(|p| low.contains(p))
        // « limit reached … resets at … » : formulation longue, des deux morceaux ensemble seulement.
        || (low.contains("limit reached") && (low.contains("reset") || low.contains("try again")));
    hit.then(|| UsageLimit { resets_at: epoch_in(text) })
}

/// Premier horodatage epoch plausible du texte : secondes (10 chiffres) ou millisecondes (13 chiffres).
/// `claude` annonce la limite sous la forme « Claude AI usage limit reached|1761580800 ».
fn epoch_in(text: &str) -> Option<u64> {
    const MIN: u64 = 1_600_000_000; // 2020
    const MAX: u64 = 4_000_000_000; // 2096
    text.split(|c: char| !c.is_ascii_digit())
        .filter_map(|p| match p.len() {
            10 => p.parse::<u64>().ok(),
            13 => p.parse::<u64>().ok().map(|ms| ms / 1000),
            _ => None,
        })
        .find(|s| (MIN..MAX).contains(s))
}

/// Variables qui feraient basculer `claude` vers une facturation API / cloud tiers.
/// Elles sont retirées de l'environnement des processus `claude` lancés par essaim
/// (`claude_command` et terminal intégré) : l'outil ne doit jamais consommer que l'abonnement.
pub const BILLING_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// Préfixe des variables de sélection de fournisseur (Bedrock, Vertex, Foundry… et toute variante future).
pub const BILLING_ENV_PREFIX: &str = "CLAUDE_CODE_USE_";

/// Prédicat unique : la variable doit-elle être retirée de l'environnement d'un processus `claude` ?
/// Vrai pour la liste `BILLING_ENV_VARS` et pour tout nom commençant par `BILLING_ENV_PREFIX`.
pub fn is_billing_env(name: &str) -> bool {
    BILLING_ENV_VARS.contains(&name) || name.starts_with(BILLING_ENV_PREFIX)
}

/// Variables de facturation présentes dans l'environnement courant (à retirer avant tout lancement).
/// `vars_os` : une variable non UTF-8 ne fait pas paniquer, elle est simplement ignorée.
pub fn billing_env_to_remove() -> Vec<String> {
    std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).filter(|k| is_billing_env(k)).collect()
}

/// Commande `claude` : environnement hérité du processus, moins les variables de facturation
/// (`is_billing_env` : jamais de clé API ni de cloud tiers). Point d'entrée des lancements `claude -p`, `--bg`,
/// `claude agents`, `stop` et `rm` ; le terminal intégré (`tui::term`) applique le même retrait.
/// `ESSAIM_CLAUDE_BIN` permet de substituer le binaire (tests).
pub fn claude_command() -> Command {
    let mut cmd = Command::new(std::env::var("ESSAIM_CLAUDE_BIN").unwrap_or_else(|_| "claude".into()));
    for var in billing_env_to_remove() {
        cmd.env_remove(var);
    }
    cmd
}

/// Variables dont la valeur est un secret (masquée dans les textes affichés ou persistés).
const SECRET_VARS: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"];

/// Masquage « au mieux » des secrets connus dans un texte destiné à l'écran ou à un fichier :
/// jetons `sk-ant-…` et affectations `NOM_SECRET=valeur`. Ne remplace pas la prudence à la source.
pub fn redact_secrets(text: &str) -> String {
    redact_secrets_cow(text).into_owned()
}

/// Comme `redact_secrets`, sans allocation quand aucun motif n'est présent (cas courant).
pub fn redact_secrets_cow(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("sk-ant-") && !SECRET_VARS.iter().any(|v| text.contains(v)) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            out.push_str(&redact_token(&token));
            token.clear();
            out.push(c);
        } else {
            token.push(c);
        }
    }
    out.push_str(&redact_token(&token));
    std::borrow::Cow::Owned(out)
}

fn redact_token(t: &str) -> String {
    if let Some(i) = t.find("sk-ant-") {
        return format!("{}[secret masqué]", &t[..i]);
    }
    if let Some((k, _)) = t.split_once('=') {
        let name = k.trim_start_matches(['"', '\'', '`']);
        if SECRET_VARS.contains(&name) {
            return format!("{k}=[secret masqué]");
        }
    }
    t.to_string()
}

/// Délai maximal d'une commande courte (`claude agents`, `--bg`, `stop`, `rm`) : au-delà, le processus est tué.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// Délai maximal d'un nœud (`claude -p` ou session `--bg`) : au-delà, le travail est arrêté.
/// Valeur de repli ; une entreprise fixe le sien avec `node_timeout_s` (défaut 15 min).
pub const NODE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Délai laissé à un agent pour s'arrêter proprement après le prompt d'arrêt de la pause.
/// Mesuré sur `claude` 2.1.316 : l'agent termine son action en vol et rend son bilan en ~3 s ;
/// 45 s couvre largement une commande longue (test, build) en cours au moment de la pause.
pub const PAUSE_GRACE: Duration = Duration::from_secs(45);

/// Paramètres d'un lancement `claude -p`.
#[derive(Debug, Clone, Default)]
pub struct RunSpec {
    pub prompt: String,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub max_budget_usd: Option<f64>,
    /// Sous-agents injectés via `--agents` (nom → définition JSON).
    pub agents: BTreeMap<String, Value>,
    /// Agent principal de la session (`--agent`).
    pub agent: Option<String>,
    pub session_id: Option<String>,
    /// `--resume` : reprend la session d'un appel précédent (contexte conservé, relu depuis le cache de prompt).
    pub resume: Option<String>,
    /// `--tools` : liste d'outils disponibles ; `Some("")` = aucun outil.
    pub tools: Option<String>,
    /// `--allowedTools` : règles autorisées d'office (ex. `Edit(src/**)`), séparées par des virgules à l'envoi.
    pub allowed_tools: Vec<String>,
    /// `--append-system-prompt` : fiche de poste ajoutée au prompt système.
    pub append_system_prompt: Option<String>,
    /// `--json-schema` : schéma de la sortie structurée.
    pub json_schema: Option<String>,
    /// `--mcp-config` : le serveur MCP d'essaim (demandes à l'humain), seul serveur de la session.
    pub mcp_config: Option<String>,
    /// `--permission-prompt-tool` : outil MCP qui reçoit les demandes de permission (profil protégé).
    pub permission_prompt_tool: Option<String>,
    /// `--settings` : réglages ajoutés pour cet appel (bac à sable du profil protégé).
    pub settings: Option<String>,
}

impl RunSpec {
    /// Options communes aux modes `-p` et `--bg`.
    fn common_args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec![];
        if let Some(m) = &self.model {
            a.extend(["--model".into(), m.clone()]);
        }
        if let Some(m) = &self.permission_mode {
            a.extend(["--permission-mode".into(), m.clone()]);
        }
        if !self.agents.is_empty() {
            // Sérialisation infaillible (`BTreeMap<String, Value>`) ; repli vide = `claude` refuse l'option (échec visible).
            a.extend(["--agents".into(), serde_json::to_string(&self.agents).unwrap_or_default()]);
        }
        if let Some(n) = &self.agent {
            a.extend(["--agent".into(), n.clone()]);
        }
        if let Some(t) = &self.tools {
            a.extend(["--tools".into(), t.clone()]);
        }
        if !self.allowed_tools.is_empty() {
            a.extend(["--allowedTools".into(), self.allowed_tools.join(",")]);
        }
        if let Some(p) = &self.append_system_prompt {
            a.extend(["--append-system-prompt".into(), p.clone()]);
        }
        if let Some(sc) = &self.json_schema {
            a.extend(["--json-schema".into(), sc.clone()]);
        }
        a
    }

    pub fn args(&self) -> Vec<String> {
        // `--input-format stream-json` : stdin reste ouvert pendant tout le nœud, ce qui permet d'y
        // pousser le prompt d'arrêt quand le run passe en pause. Conséquence mesurée sur 2.1.316 :
        // le prompt en argv est alors IGNORÉ, il doit partir par stdin (voir `ClaudeProc::spawn_with`).
        let mut a: Vec<String> = ["-p", "--output-format", "stream-json", "--input-format", "stream-json", "--verbose"].map(String::from).into();
        // Environnement isolé : ni serveur MCP (aucun `--mcp-config` n'est passé), ni réglages
        // utilisateur. Sans cela chaque poste hérite des plugins, skills et `settings.json` de la
        // machine — mesuré : prompt système de 9,3 k au lieu de ~1,5 k, relu à chaque tour — et le
        // `model` ou `autoContinueAtUsageLimit` de l'utilisateur écrase les choix d'essaim.
        a.extend(["--strict-mcp-config".into(), "--setting-sources".into(), "project,local".into()]);
        // Le seul serveur MCP admis est celui d'essaim : c'est par lui qu'un agent s'adresse à l'humain.
        if let Some(m) = &self.mcp_config {
            a.extend(["--mcp-config".into(), m.clone()]);
        }
        if let Some(t) = &self.permission_prompt_tool {
            a.extend(["--permission-prompt-tool".into(), t.clone()]);
        }
        if let Some(st) = &self.settings {
            a.extend(["--settings".into(), st.clone()]);
        }
        a.extend(self.common_args());
        if let Some(b) = self.max_budget_usd {
            a.extend(["--max-budget-usd".into(), b.to_string()]);
        }
        if let Some(id) = &self.session_id {
            a.extend(["--session-id".into(), id.clone()]);
        }
        if let Some(id) = &self.resume {
            a.extend(["--resume".into(), id.clone()]);
        }
        // Pas de prompt en argv : avec `--input-format stream-json` il serait ignoré. Il part sur stdin.
        a
    }

    /// Le prompt, encodé pour stdin (`--input-format stream-json`) : une ligne NDJSON, saut de ligne compris.
    pub fn stdin_line(&self) -> String {
        user_message_line(&self.prompt)
    }

}

/// Un message utilisateur du flux d'entrée `stream-json`, en une ligne NDJSON (saut de ligne compris).
/// La sérialisation ne peut pas échouer (chaîne simple) ; le repli vide ferait ignorer le message.
fn user_message_line(text: &str) -> String {
    let v = serde_json::json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}});
    format!("{v}\n")
}

/// Plafond de la sortie d'erreur conservée (octets). Au-delà, le reste est lu et jeté : le processus ne se bloque pas.
pub const STDERR_CAP: usize = 16 * 1024;

/// Lit `reader` jusqu'à la fin et ne garde que `cap` octets. Si la sortie est tronquée, le texte le dit
/// (troncature journalisée dans le texte d'erreur, sans canal supplémentaire).
async fn read_capped<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> String {
    let mut kept: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = buf.get(..n).unwrap_or_default();
                let room = cap.saturating_sub(kept.len());
                if chunk.len() > room {
                    truncated = true;
                }
                kept.extend_from_slice(chunk.get(..room.min(chunk.len())).unwrap_or_default());
            }
            // Erreur de lecture de stderr : fin de la capture. Sans effet sur le résultat (stdout reste la source).
            Err(_) => break,
        }
    }
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if truncated {
        text.push_str(&format!("\n[… sortie d'erreur tronquée à {cap} octets]"));
    }
    text
}

/// Envoie SIGKILL au groupe de processus `pid` (le processus lancé avec `process_group(0)` en est le chef).
/// Sans crate `libc` : commande `kill -KILL -- -<pgid>`. Échec ignoré : le processus peut déjà être terminé,
/// et `Child::kill` reste le repli direct.
#[cfg(unix)]
async fn kill_group(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", "--"])
        .arg(format!("-{pid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

#[cfg(not(unix))]
async fn kill_group(_pid: u32) {}

pub struct ClaudeProc {
    child: Child,
    /// Entrée du flux `stream-json`, gardée ouverte pendant tout le nœud pour pouvoir y pousser le
    /// prompt d'arrêt de la pause. `None` dès qu'elle est fermée (`close_stdin`), ce qui autorise
    /// `claude` à sortir : tant qu'elle est ouverte, le processus attend d'autres messages — le
    /// `result`, lui, arrive sans attendre l'EOF.
    stdin: Option<tokio::process::ChildStdin>,
    /// Prompt du nœud, en attente d'envoi par `send_prompt`.
    prompt_line: String,
    /// Tâche de lecture de stderr (bornée) ; récupérée une fois le processus terminé.
    /// `allow(dead_code)` TEMPORAIRE : consommée par `executor::run_local` (étape c), hors périmètre de ce poste.
    #[allow(dead_code)]
    stderr: Option<JoinHandle<String>>,
}

impl ClaudeProc {
    /// Lance `claude` et pousse chaque événement dans `tx`. Le canal se ferme à la fin du flux.
    /// Si la session annonce une clé API (`apiKeySource` différent de `none`), émet un `Result` en erreur
    /// et arrête le processus (groupe, voir `kill`).
    /// `cmd` est normalement `claude_command()` ; un autre programme sert aux tests.
    /// Le processus est lancé dans son propre groupe (`process_group(0)`, unix) : `kill` atteint ses sous-processus.
    pub fn spawn_with(mut cmd: Command, spec: &RunSpec, tx: mpsc::UnboundedSender<ClaudeEvent>) -> Result<Self> {
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .args(spec.args())
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("cannot launch `claude` (is it in the PATH?)")?;
        let stdin = child.stdin.take();
        let prompt_line = spec.stdin_line();
        let stdout = child.stdout.take().context("stdout indisponible")?;
        let stderr = child.stderr.take().map(|e| tokio::spawn(read_capped(e, STDERR_CAP)));
        let mut guard = child.id();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(evs) = parse_line(&line) {
                    for e in evs {
                        // Garde-fou : refuser toute session qui ne serait pas sur l'abonnement.
                        if let ClaudeEvent::Init { api_key_source, .. } = &e {
                            if api_key_source != "none" {
                                let _ = tx.send(ClaudeEvent::Result {
                                    text: format!(
                                        "refused: the session uses an API key (apiKeySource={api_key_source}), not the subscription"
                                    ),
                                    is_error: true,
                                    tokens: 0,
                                    volume: 0,
                                    cached: 0,
                                    duration_ms: 0,
                                    structured: None,
                                    denials: vec![],
                                });
                                if let Some(pid) = guard.take() {
                                    kill_group(pid).await;
                                }
                                return;
                            }
                        }
                        if tx.send(e).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(Self { child, stdin, prompt_line, stderr })
    }

    /// Envoie le prompt du nœud sur stdin. À appeler une fois, juste après `spawn_with` :
    /// avec `--input-format stream-json`, c'est la seule façon de le transmettre.
    pub async fn send_prompt(&mut self) -> Result<()> {
        let line = std::mem::take(&mut self.prompt_line);
        self.write_line(&line).await
    }

    /// Pousse un message utilisateur dans la session en cours. Mesuré sur `claude` 2.1.316 : il est pris
    /// au prochain aller-retour d'outil, sans attendre la fin de la tâche — c'est ce qui permet à la pause
    /// d'arrêter un agent en vol. Erreur rendue à l'appelant, dont le repli est `kill`.
    pub async fn send_user(&mut self, text: &str) -> Result<()> {
        self.write_line(&user_message_line(text)).await
    }

    /// Répond à un `control_request`. `body` : la décision (`{"behavior":"allow",…}` ou `{"behavior":"deny",…}`) ;
    /// `None` : demande que l'hôte ne sait pas traiter, rendue en erreur pour que `claude` n'attende pas.
    pub async fn send_control_response(&mut self, request_id: &str, body: Option<Value>) -> Result<()> {
        let response = match body {
            Some(b) => serde_json::json!({"subtype": "success", "request_id": request_id, "response": b}),
            None => serde_json::json!({"subtype": "error", "request_id": request_id, "error": "unsupported control request"}),
        };
        self.write_line(&format!("{}\n", serde_json::json!({"type": "control_response", "response": response}))).await
    }

    async fn write_line(&mut self, line: &str) -> Result<()> {
        let w = self.stdin.as_mut().context("stdin déjà fermé")?;
        w.write_all(line.as_bytes()).await?;
        w.flush().await.map_err(Into::into)
    }

    /// Ferme stdin : `claude` peut alors sortir. Tant qu'il est ouvert, le processus attend d'autres
    /// messages même après avoir émis son `result`. Sans effet s'il est déjà fermé.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Arrête le processus **et ses descendants** (groupe, unix), puis attend sa fin : sans cela, un sous-processus
    /// lancé par `claude` survit à l'annulation (R4). `Child::kill` reste le repli si le groupe est introuvable.
    pub async fn kill(&mut self) -> Result<()> {
        // `id()` vaut None une fois le processus réapé : pas de signal vers un pid réutilisé.
        if let Some(pid) = self.child.id() {
            kill_group(pid).await;
        }
        self.child.kill().await.map_err(Into::into)
    }

    /// Sortie d'erreur capturée (bornée à `STDERR_CAP`), à appeler une fois le processus terminé ;
    /// vide si la tâche ne se termine pas dans 2 s (descendant qui garde stderr ouvert) : diagnostic best-effort.
    /// Les secrets connus sont masqués (`redact_secrets`).
    /// `allow(dead_code)` TEMPORAIRE : appelée par `executor::run_local` (étape c), hors périmètre de ce poste.
    #[allow(dead_code)]
    pub async fn stderr_text(&mut self) -> String {
        let Some(handle) = self.stderr.take() else {
            return String::new();
        };
        match tokio::time::timeout(Duration::from_secs(2), handle).await {
            Ok(Ok(text)) => redact_secrets(&text),
            // Tâche interrompue ou délai dépassé : pas de diagnostic, le code de sortie reste disponible.
            Ok(Err(_)) | Err(_) => String::new(),
        }
    }

    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        self.child.wait().await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Demande de `claude` à son hôte, telle que relevée sur 2.1.322 (domaine réseau depuis le bac à sable).
    #[test]
    fn demande_de_controle_lue() {
        let line = r#"{"type":"control_request","request_id":"573a","request":{"subtype":"can_use_tool","tool_name":"SandboxNetworkAccess","display_name":"SandboxNetworkAccess","input":{"host":"example.com"},"tool_use_id":"d90e","description":"Allow network connection to example.com?"}}"#;
        match &parse_line(line).unwrap()[..] {
            [ClaudeEvent::Control { request_id, subtype, tool, input }] => {
                assert_eq!((request_id.as_str(), subtype.as_str(), tool.as_str()), ("573a", "can_use_tool", "SandboxNetworkAccess"));
                assert_eq!(input["host"], "example.com");
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn parse_fixture_reelle() {
        let src = include_str!("../../tests/fixtures/simple.jsonl");
        let evs: Vec<_> = src
            .lines()
            .flat_map(|l| parse_line(l).unwrap())
            .collect();
        assert!(matches!(&evs[0], ClaudeEvent::Init { model, api_key_source, .. }
            if model.contains("haiku") && api_key_source == "none"));
        assert!(evs.contains(&ClaudeEvent::Text("pong".into())));
        match evs.last().unwrap() {
            ClaudeEvent::Result { text, is_error, tokens, .. } => {
                assert_eq!(text, "pong");
                assert!(!is_error);
                assert!(*tokens > 0);
            }
            e => panic!("dernier événement inattendu : {e:?}"),
        }
    }

    #[test]
    fn limite_d_abonnement_reconnue_et_datee() {
        // Forme réelle du flux `claude -p` : message puis epoch, séparés par une barre verticale.
        assert_eq!(usage_limit("Claude AI usage limit reached|1761580800"), Some(UsageLimit { resets_at: Some(1_761_580_800) }));
        assert_eq!(usage_limit("5-hour limit reached"), Some(UsageLimit { resets_at: None }));
        assert_eq!(usage_limit("Limit reached. Your limit will reset at 3pm."), Some(UsageLimit { resets_at: None }));
        assert!(usage_limit("API Error: 429 {\"type\":\"rate_limit_error\"}").is_some());
        assert_eq!(usage_limit("retry at 1761580800000").map(|l| l.resets_at), None, "sans message de limite : pas une pause");
        // Un agent qui parle de limites dans son rapport ne doit pas mettre le run en pause.
        for innocent in ["j'ai ajouté une limite de débit à l'API", "the API documents a rate limit of 50 rps", "tests ok, limit 10"] {
            assert_eq!(usage_limit(innocent), None, "{innocent}");
        }
    }

    #[test]
    fn parse_tool_use_et_result() {
        let l = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Read","input":{"file_path":"/x"}}]}}"#;
        let evs = parse_line(l).unwrap();
        assert_eq!(evs.len(), 2);
        assert!(matches!(&evs[1], ClaudeEvent::ToolUse { name, .. } if name == "Read"));
        let u = r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true}]}}"#;
        assert_eq!(parse_line(u).unwrap(), vec![ClaudeEvent::ToolResult { is_error: true, message: String::new() }]);
    }

    #[test]
    fn tool_result_garde_la_cause_de_l_erreur() {
        let u = r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"Output does not match required schema:\n  /sections/a: must NOT have more than 300 characters (got 352)"}]}}"#;
        let ClaudeEvent::ToolResult { is_error: true, message } = parse_line(u).unwrap().remove(0) else { panic!("erreur d'outil attendue") };
        assert_eq!(message, "Output does not match required schema: /sections/a: must NOT have more than 300 characters (got 352)", "cause sur une seule ligne");

        // Contenu en blocs, et succès : rien à garder.
        let b = r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":[{"type":"text","text":"boum"}]}]}}"#;
        assert_eq!(parse_line(b).unwrap(), vec![ClaudeEvent::ToolResult { is_error: true, message: "boum".into() }]);
        let ok = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"des milliers de lignes"}]}}"#;
        assert_eq!(parse_line(ok).unwrap(), vec![ClaudeEvent::ToolResult { is_error: false, message: String::new() }]);

        // Contenu énorme : borné dès la lecture, puis à l'affichage.
        let gros = format!(r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","is_error":true,"content":"{}"}}]}}}}"#, "a".repeat(100_000));
        let ClaudeEvent::ToolResult { message, .. } = parse_line(&gros).unwrap().remove(0) else { panic!("erreur d'outil attendue") };
        assert_eq!(message.chars().count(), TOOL_ERROR_CHARS, "cause bornée");
        assert!(message.ends_with('…'));
    }

    #[test]
    fn prompt_hors_argv_et_jamais_pris_pour_une_option() {
        let spec = RunSpec {
            prompt: "--rm -rf".into(),
            model: Some("haiku".into()),
            max_budget_usd: Some(0.2),
            ..Default::default()
        };
        let a = spec.args();
        // Le prompt part sur stdin : il n'apparaît nulle part dans la ligne de commande, donc il ne
        // peut plus être confondu avec une option (garantie plus forte que le `--` d'avant).
        assert!(!a.iter().any(|x| x.contains("--rm -rf")), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["--model", "haiku"]));
        assert!(a.windows(2).any(|w| w == ["--input-format", "stream-json"]));
        // Une seule ligne NDJSON, avec le prompt échappé en JSON.
        let line = spec.stdin_line();
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1, "{line:?}");
        let v: Value = serde_json::from_str(line.trim()).expect("ligne JSON valide");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"][0]["text"], "--rm -rf");
    }

    #[test]
    fn plafond_de_budget_distingue_de_la_limite_d_abonnement() {
        // Les deux parlent de « limit reached » : les confondre ferait attendre une réinitialisation
        // qui n'arrivera jamais, ou rejouer un appel que le plafond coupera à nouveau.
        assert!(budget_exhausted("Budget limit reached"));
        assert!(budget_exhausted("estimated budget exceeded"));
        assert!(usage_limit("Budget limit reached").is_none(), "un plafond de budget n'est pas la limite de l'abonnement");
        assert!(!budget_exhausted("Claude AI usage limit reached|1761580800"));
        assert!(usage_limit("Claude AI usage limit reached|1761580800").is_some());
    }

    #[test]
    fn environnement_isole_des_reglages_de_la_machine() {
        let a = RunSpec::default().args();
        // Sans ces deux options, chaque poste hérite des serveurs MCP, plugins, skills et du `model`
        // de l'utilisateur — relus à chaque tour, et ils écrasent les choix d'essaim.
        assert!(a.iter().any(|x| x == "--strict-mcp-config"), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["--setting-sources", "project,local"]), "{a:?}");
    }

    #[test]
    fn message_utilisateur_sur_une_seule_ligne() {
        // Un prompt multiligne ne doit pas casser le protocole NDJSON (une ligne = un message).
        let line = user_message_line("a\nb\n\"c\"");
        assert_eq!(line.matches('\n').count(), 1, "{line:?}");
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["message"]["content"][0]["text"], "a\nb\n\"c\"");
    }

    #[test]
    fn options_de_poste_dans_les_arguments() {
        let spec = RunSpec {
            prompt: "p".into(),
            tools: Some(String::new()),
            allowed_tools: vec!["Edit(src/**)".into(), "Write(src/**)".into()],
            append_system_prompt: Some("fiche".into()),
            json_schema: Some("{}".into()),
            permission_mode: Some("dontAsk".into()),
            ..Default::default()
        };
        let a = spec.args();
        assert!(a.windows(2).any(|w| w == ["--tools", ""]), "aucun outil = chaîne vide");
        assert!(a.windows(2).any(|w| w == ["--allowedTools", "Edit(src/**),Write(src/**)"]));
        assert!(a.windows(2).any(|w| w == ["--append-system-prompt", "fiche"]));
        assert!(a.windows(2).any(|w| w == ["--json-schema", "{}"]));
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "dontAsk"]));
        assert!(!a.iter().any(|x| x == "--" || x == "p"), "le prompt ne passe plus en argv : il part sur stdin");
    }

    #[test]
    fn lectures_de_cache_comptees_a_part() {
        let l = r#"{"type":"result","is_error":false,"result":"ok","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":20,"cache_read_input_tokens":100}}"#;
        match &parse_line(l).unwrap()[0] {
            // Volume 135 ; pondéré 70 = 10 (entrée) + 5×5 (sortie) + 100/10 (cache lu) + 20×1,25 (cache écrit).
            ClaudeEvent::Result { tokens, volume, cached, .. } => assert_eq!((*tokens, *volume, *cached), (70, 135, 100)),
            e => panic!("{e:?}"),
        }
        let sans = r#"{"type":"result","is_error":false,"result":"ok"}"#;
        assert!(matches!(&parse_line(sans).unwrap()[0], ClaudeEvent::Result { cached: 0, .. }));
    }

    #[test]
    fn reprise_de_session_dans_les_arguments() {
        let a = RunSpec { prompt: "p".into(), resume: Some("sid".into()), ..Default::default() }.args();
        assert!(a.windows(2).any(|w| w == ["--resume", "sid"]));
        // Serveur MCP d'essaim, outil de permission et bac à sable : passés seulement s'ils sont demandés.
        let nu = RunSpec::default().args();
        assert!(!nu.iter().any(|x| x == "--mcp-config" || x == "--permission-prompt-tool" || x == "--settings"), "{nu:?}");
        let m = RunSpec { mcp_config: Some("{}".into()), permission_prompt_tool: Some("mcp__essaim__permission".into()), settings: Some("{\"sandbox\":{}}".into()), ..Default::default() }.args();
        assert!(m.windows(2).any(|w| w == ["--mcp-config", "{}"]), "{m:?}");
        assert!(m.windows(2).any(|w| w == ["--permission-prompt-tool", "mcp__essaim__permission"]), "{m:?}");
        assert!(m.windows(2).any(|w| w == ["--settings", "{\"sandbox\":{}}"]), "{m:?}");
        assert!(m.iter().any(|x| x == "--strict-mcp-config"), "aucun autre serveur MCP que celui d'essaim");
        assert!(!RunSpec::default().args().iter().any(|x| x == "--resume"));
    }

    #[test]
    fn quota_et_usage_temps_reel() {
        let q = r#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":0.04,"resetsAt":10},"seven_day":{"utilization":0.01,"resetsAt":20}}}}"#;
        assert_eq!(parse_line(q).unwrap(), vec![ClaudeEvent::RateLimit { five_hour: Some(0.04), seven_day: Some(0.01), resets_at: Some(10) }]);
        let a = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":3,"output_tokens":2,"cache_read_input_tokens":10}}}"#;
        let evs = parse_line(a).unwrap();
        assert_eq!(evs[0], ClaudeEvent::Usage { id: "m1".into(), tokens: 14, volume: 15, cached: 10 }, "pondéré : 3 + 2×5 + 10/10");
        assert_eq!(evs[1], ClaudeEvent::Text("hi".into()));
    }

    #[test]
    fn sortie_structuree_dans_le_resultat() {
        let l = r#"{"type":"result","result":"{}","is_error":false,"total_cost_usd":0.01,"duration_ms":5,"structured_output":{"assignments":[{"to":"a","task":"t"}]}}"#;
        match &parse_line(l).unwrap()[0] {
            ClaudeEvent::Result { structured: Some(v), .. } => assert_eq!(v["assignments"][0]["to"], "a"),
            e => panic!("{e:?}"),
        }
        let l = r#"{"type":"result","result":"x","is_error":false}"#;
        assert!(matches!(&parse_line(l).unwrap()[0], ClaudeEvent::Result { structured: None, .. }));
    }

    #[test]
    fn variables_de_facturation_reconnues() {
        for v in BILLING_ENV_VARS {
            assert!(is_billing_env(v), "{v}");
        }
        assert!(is_billing_env("CLAUDE_CODE_USE_MANTLE"), "toute variante CLAUDE_CODE_USE_* est retirée");
        assert!(!is_billing_env("CLAUDE_CODE_USER_NAME"), "pas de faux positif sur un nom voisin");
        assert!(!is_billing_env("PATH") && !is_billing_env("HOME") && !is_billing_env("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    /// `essaim.sh` duplique la liste (`env -u … claude auth status`) : elle doit rester identique au prédicat.
    #[test]
    fn essaim_sh_retire_les_memes_variables() {
        let src = include_str!("../../essaim.sh");
        let start = src.find("env -u").expect("ligne `env -u` introuvable dans essaim.sh");
        let end = start + src[start..].find("claude auth status").expect("`claude auth status` introuvable");
        let tokens: Vec<&str> = src[start..end].split_whitespace().collect();
        let mut listed: Vec<&str> = tokens.windows(2).filter(|w| w[0] == "-u").map(|w| w[1]).collect();
        let mut expected: Vec<&str> = BILLING_ENV_VARS.to_vec();
        listed.sort();
        expected.sort();
        assert_eq!(listed, expected, "essaim.sh et BILLING_ENV_VARS divergent");
        assert!(listed.iter().all(|v| is_billing_env(v)));
    }

    #[test]
    fn parse_tolerant_aux_types_inattendus() {
        // Mêmes résultats qu'avec `Value` : champ de mauvais type = valeur par défaut, JSON non objet = Other.
        assert_eq!(parse_line("[1,2]").unwrap(), vec![ClaudeEvent::Other]);
        assert!(parse_line("pas du json").is_err());
        let l = r#"{"type":"result","result":{"x":1},"is_error":"non","usage":{"input_tokens":"3","output_tokens":4},"permission_denials":[{"tool_name":"Bash","tool_input":{"command":"git status"}},{}]}"#;
        match &parse_line(l).unwrap()[0] {
            ClaudeEvent::Result { text, is_error, tokens, denials, .. } => {
                assert_eq!((text.as_str(), *is_error, *tokens), ("", true, 20), "entrée de mauvais type = 0 ; sortie 4 × 5");
                assert_eq!(denials, &vec!["Bash(git status)".to_string(), "?".to_string()]);
            }
            e => panic!("{e:?}"),
        }
        let u = r#"{"type":"user","message":{"content":"texte"}}"#;
        assert!(parse_line(u).unwrap().is_empty());
    }

    #[test]
    fn masquage_sans_motif_sans_allocation() {
        assert!(matches!(redact_secrets_cow("rien\n à  voir"), std::borrow::Cow::Borrowed(_)));
        assert!(matches!(redact_secrets_cow("sk-ant-abc"), std::borrow::Cow::Owned(_)));
    }

    #[test]
    fn masquage_des_secrets_connus() {
        let s = redact_secrets("erreur sk-ant-api03-XYZ123 fin\nANTHROPIC_API_KEY=abc123 ANTHROPIC_AUTH_TOKEN=\"tok\" ok");
        assert!(!s.contains("XYZ123") && !s.contains("abc123") && !s.contains("tok"), "{s}");
        assert!(s.contains("erreur") && s.contains("fin") && s.contains("ok"), "{s}");
        assert_eq!(redact_secrets("rien à voir\n  ici"), "rien à voir\n  ici");
    }

    #[tokio::test]
    async fn refus_cle_api_arrete_le_processus() {
        // Faux `claude` : annonce une session sur clé API puis reste en vie.
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(r#"echo '{"type":"system","subtype":"init","session_id":"s","model":"m","apiKeySource":"ANTHROPIC_API_KEY"}'; exec sleep 30"#);
        let spec = RunSpec { cwd: std::env::temp_dir(), ..Default::default() };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut p = ClaudeProc::spawn_with(cmd, &spec, tx).unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
        assert!(matches!(ev, ClaudeEvent::Result { is_error: true, .. }), "{ev:?}");
        tokio::time::timeout(Duration::from_secs(10), p.wait()).await.expect("processus non arrêté").unwrap();
    }

    #[tokio::test]
    #[ignore = "lance un vrai `claude` (coût ~0,001 $)"]
    async fn spawn_reel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let spec = RunSpec {
            prompt: "Réponds uniquement: pong".into(),
            cwd: std::env::temp_dir(),
            model: Some("haiku".into()),
            max_budget_usd: Some(0.2),
            ..Default::default()
        };
        let mut p = ClaudeProc::spawn_with(claude_command(), &spec, tx).unwrap();
        let mut got = None;
        while let Some(e) = rx.recv().await {
            if let ClaudeEvent::Result { text, .. } = e {
                got = Some(text);
            }
        }
        p.wait().await.unwrap();
        assert_eq!(got.as_deref().map(str::trim), Some("pong"));
    }
}
