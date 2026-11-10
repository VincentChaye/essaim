//! Consommation de jetons, lue dans les transcriptions de Claude Code.
//!
//! Chaque session `claude` — terminal interactif, agent de fond, appel `-p` d'un poste — écrit un
//! fichier `~/.claude/projects/<dossier>/<session>.jsonl`. Chaque message de l'assistant y porte son
//! `usage` : c'est la seule source qui couvre *tout* ce que l'abonnement paie, essaim compris.
//!
//! Deux précautions sans lesquelles les chiffres sont faux :
//! - un même message apparaît une fois par bloc de contenu, et une session reprise (`--resume`) recopie
//!   l'historique dans une nouvelle transcription : un `message.id` n'est compté **qu'une fois, toutes
//!   transcriptions confondues**, sinon la consommation est multipliée (ici : un tiers de lignes en
//!   double dans un fichier, et 35 millions de jetons recopiés entre fichiers) ;
//! - les fichiers ne font que grossir par la fin : on ne relit que les octets neufs (211 Mio de
//!   transcriptions au premier passage, quelques kilo-octets aux suivants).

use crate::model::user_claude_dir;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Jetons d'un message, d'une session ou d'une fenêtre, par nature.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Tally {
    /// Entrée neuve (ni lue ni écrite dans le cache de prompt).
    pub input: u64,
    pub output: u64,
    /// Entrée servie par le cache de prompt : la part la moins chère, et de loin la plus grosse.
    pub cache_read: u64,
    /// Entrée écrite dans le cache de prompt.
    pub cache_write: u64,
    /// Nombre de messages de l'assistant comptés.
    pub messages: u64,
}

impl Tally {
    /// Volume brut : somme des quatre natures. Ce n'est **pas** un coût — le cache lu y pèse autant
    /// que l'entrée neuve alors qu'il coûte ~10 % du prix, et il fait en pratique plus de 90 % du
    /// volume d'un poste qui enchaîne les tours. À n'afficher qu'en second, pour expliquer le coût.
    pub fn volume(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// Coût pondéré, en équivalents jetons d'entrée : c'est le chiffre à comparer et à classer.
    /// Poids dans `claude_proc::weigh`, écrits à un seul endroit.
    pub fn weighted(&self) -> u64 {
        crate::runtime::claude_proc::weigh(self.input, self.output, self.cache_read, self.cache_write)
    }

    pub fn add(&mut self, o: &Tally) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.messages += o.messages;
    }
}

/// Un message de l'assistant : quand, avec quel modèle, pour quels jetons.
#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub ms: u64,
    pub model: String,
    /// Message d'un sous-agent (`Task`) de la session.
    pub sidechain: bool,
    pub tally: Tally,
}

/// Consommation d'une session `claude`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct SessionUsage {
    pub id: String,
    /// Titre de la session, ou début de sa première demande ; vide si la transcription n'en donne aucun.
    pub label: String,
    /// Dossier de travail de la session (le « projet »).
    pub cwd: String,
    pub first_ms: u64,
    pub last_ms: u64,
    pub tally: Tally,
    /// Part des sous-agents (`Task`) dans `tally`.
    pub sidechain: Tally,
    /// Modèles utilisés, du plus gros consommateur au plus petit.
    pub models: Vec<String>,
}

/// Agrégats d'une fenêtre de temps, tels que le tableau de bord les affiche.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Window {
    pub total: Tally,
    pub sessions: Vec<SessionUsage>,
    /// Par dossier de travail, du plus gros au plus petit.
    pub projects: Vec<(String, Tally)>,
    pub models: Vec<(String, Tally)>,
    /// Séries temporelles : début de tranche (ms epoch) et jetons de la tranche.
    pub buckets: Vec<(u64, Tally)>,
    pub step_ms: u64,
}

/// Ce qu'un rafraîchissement a lu (affiché dans le pied de page du tableau de bord).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refreshed {
    pub files: usize,
    pub new_bytes: u64,
    pub messages: u64,
}

/// État d'une transcription déjà lue : ce qui permet de ne relire que la fin.
#[derive(Debug, Default)]
struct FileState {
    /// Octets déjà lus, toujours sur une fin de ligne (une ligne en cours d'écriture est relue plus tard).
    offset: u64,
    session: String,
    cwd: String,
    label: String,
    /// Numéro interne de la transcription : désigne le propriétaire d'un `message.id`.
    idx: u32,
    msgs: Vec<Msg>,
    /// Essais de lecture d'un libellé : borne la relecture des grosses lignes utilisateur.
    label_tries: u8,
}

/// Lecteur incrémental des transcriptions.
#[derive(Debug)]
pub struct Scanner {
    dir: PathBuf,
    files: HashMap<PathBuf, FileState>,
    /// Propriétaire de chaque `message.id` compté : la première transcription qui l'a vu. Une copie
    /// du même message ailleurs (session reprise) n'est pas recomptée.
    owners: HashMap<String, u32>,
    next_idx: u32,
}

/// Plafond de taille d'une ligne analysée (une ligne d'`usage` pèse quelques kilo-octets ; un résultat
/// d'outil peut peser des mégaoctets et n'apporte rien ici).
const MAX_LINE: usize = 512 * 1024;

/// Essais de lecture du libellé d'une session avant de renoncer.
const LABEL_TRIES: u8 = 20;

/// Caractères gardés du libellé d'une session.
const LABEL_CHARS: usize = 110;

impl Scanner {
    /// Lecteur du dossier de transcriptions de l'utilisateur (`~/.claude/projects`).
    pub fn new() -> Self {
        Self::with_dir(user_claude_dir().unwrap_or_default().join("projects"))
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir, files: HashMap::new(), owners: HashMap::new(), next_idx: 0 }
    }

    /// Relit la fin des transcriptions modifiées et oublie celles qui ont disparu.
    pub fn refresh(&mut self) -> Refreshed {
        self.scan(false)
    }

    /// `reprise` : second passage après une remise à zéro (jamais plus d'un).
    fn scan(&mut self, reprise: bool) -> Refreshed {
        let mut out = Refreshed::default();
        // Les plus anciennes d'abord : un message recopié par une reprise reste attribué à la session
        // qui l'a réellement payé, et l'attribution ne change pas d'un rafraîchissement à l'autre.
        let mut found: Vec<(u64, PathBuf)> = vec![];
        let Ok(projects) = std::fs::read_dir(&self.dir) else { return out };
        for proj in projects.flatten() {
            let Ok(files) = std::fs::read_dir(proj.path()) else { continue };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let age = f.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
                found.push((age, path));
            }
        }
        found.sort();
        out.files = found.len();
        for (_, path) in &found {
            let next = self.next_idx;
            let st = self.files.entry(path.clone()).or_insert_with(|| {
                next_state(next)
            });
            if st.idx == next {
                self.next_idx += 1;
            }
            let idx = st.idx;
            let (bytes, msgs, remis) = read_new(path, st, &mut self.owners);
            if remis {
                // Transcription réécrite : ses messages sont relus, ses anciens identifiants libérés.
                self.owners.retain(|_, v| *v != idx);
                let st = self.files.get_mut(path).expect("état présent");
                *st = next_state(idx);
                let (b2, m2, _) = read_new(path, st, &mut self.owners);
                out.new_bytes += b2;
                out.messages += m2;
                continue;
            }
            out.new_bytes += bytes;
            out.messages += msgs;
        }
        let gardes: HashSet<PathBuf> = found.into_iter().map(|(_, p)| p).collect();
        if self.files.keys().all(|p| gardes.contains(p)) {
            return out;
        }
        // Transcription supprimée : les messages qu'elle possédait ont peut-être une copie ailleurs
        // (session reprise), qui doit reprendre le compte. Seule une relecture complète le garantit ;
        // c'est rare (suppression décidée par l'utilisateur) et le total reste juste.
        self.files.clear();
        self.owners.clear();
        self.next_idx = 0;
        if reprise { out } else { self.scan(true) }
    }

    /// Nombre de transcriptions suivies.
    pub fn files(&self) -> usize {
        self.files.len()
    }

    /// Agrégats de la fenêtre `[since_ms, ∞[` (0 = tout l'historique). `now_ms` borne la dernière tranche.
    pub fn window(&self, since_ms: u64, now_ms: u64) -> Window {
        let step = step_ms(since_ms, now_ms);
        let offset = local_offset_ms();
        let mut w = Window { step_ms: step, ..Default::default() };
        let mut buckets: HashMap<u64, Tally> = HashMap::new();
        let mut projects: HashMap<&str, Tally> = HashMap::new();
        let mut models: HashMap<&str, Tally> = HashMap::new();
        for st in self.files.values() {
            let mut s = SessionUsage {
                id: st.session.clone(),
                label: st.label.clone(),
                cwd: st.cwd.clone(),
                ..Default::default()
            };
            let mut per_model: HashMap<&str, u64> = HashMap::new();
            for m in st.msgs.iter().filter(|m| m.ms >= since_ms) {
                s.tally.add(&m.tally);
                if m.sidechain {
                    s.sidechain.add(&m.tally);
                }
                if s.first_ms == 0 || m.ms < s.first_ms {
                    s.first_ms = m.ms;
                }
                s.last_ms = s.last_ms.max(m.ms);
                *per_model.entry(m.model.as_str()).or_default() += m.tally.weighted();
                models.entry(m.model.as_str()).or_default().add(&m.tally);
                buckets.entry(bucket_of(m.ms, step, offset)).or_default().add(&m.tally);
            }
            if s.tally.messages == 0 {
                continue;
            }
            projects.entry(st.cwd.as_str()).or_default().add(&s.tally);
            w.total.add(&s.tally);
            let mut ms: Vec<(&str, u64)> = per_model.into_iter().collect();
            ms.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            s.models = ms.into_iter().map(|(m, _)| m.to_string()).collect();
            w.sessions.push(s);
        }
        w.sessions.sort_by(|a, b| b.tally.weighted().cmp(&a.tally.weighted()).then(a.id.cmp(&b.id)));
        w.projects = sorted_pairs(projects);
        w.models = sorted_pairs(models);
        // Toutes les tranches de la fenêtre, y compris vides : un creux doit se voir.
        let from = if since_ms > 0 { since_ms } else { w.sessions.iter().map(|s| s.first_ms).min().unwrap_or(now_ms) };
        let (first, last) = (bucket_of(from, step, offset), bucket_of(now_ms.max(from), step, offset));
        let mut t = first;
        while t <= last && w.buckets.len() < 5000 {
            w.buckets.push((t, buckets.get(&t).copied().unwrap_or_default()));
            t += step;
        }
        w
    }

    /// Consommation d'une session donnée, sur tout l'historique (jointure avec les terminaux ouverts).
    pub fn session(&self, id: &str) -> Option<SessionUsage> {
        let st = self.files.values().find(|s| s.session == id)?;
        let mut s = SessionUsage { id: st.session.clone(), label: st.label.clone(), cwd: st.cwd.clone(), ..Default::default() };
        for m in &st.msgs {
            s.tally.add(&m.tally);
            if s.first_ms == 0 {
                s.first_ms = m.ms;
            }
            s.last_ms = s.last_ms.max(m.ms);
        }
        Some(s)
    }
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

fn sorted_pairs(m: HashMap<&str, Tally>) -> Vec<(String, Tally)> {
    let mut v: Vec<(String, Tally)> = m.into_iter().map(|(k, t)| (k.to_string(), t)).collect();
    v.sort_by(|a, b| b.1.weighted().cmp(&a.1.weighted()).then(a.0.cmp(&b.0)));
    v
}

/// Début de la tranche qui contient `ms`, alignée sur l'heure locale (une journée commence à minuit
/// chez l'utilisateur, pas à minuit UTC).
fn bucket_of(ms: u64, step: u64, offset: i64) -> u64 {
    let local = ms as i64 + offset;
    let floored = local.div_euclid(step as i64) * step as i64;
    (floored - offset).max(0) as u64
}

/// Décalage de l'heure locale par rapport à UTC, en millisecondes.
fn local_offset_ms() -> i64 {
    use chrono::Offset;
    chrono::Local::now().offset().fix().local_minus_utc() as i64 * 1000
}

/// Pas des tranches selon la largeur de la fenêtre : assez de barres pour voir la forme, assez peu
/// pour qu'elles restent lisibles.
pub fn step_ms(since_ms: u64, now_ms: u64) -> u64 {
    const MIN: u64 = 60_000;
    const H: u64 = 60 * MIN;
    let span = if since_ms == 0 { u64::MAX } else { now_ms.saturating_sub(since_ms) };
    match span {
        s if s <= 8 * H => 10 * MIN,
        s if s <= 2 * 24 * H => H,
        s if s <= 16 * 24 * H => 6 * H,
        _ => 24 * H,
    }
}

/// Instant epoch (ms) d'un horodatage ISO-8601 de transcription (`2026-10-30T12:15:29.959Z`).
fn epoch_ms(iso: &str) -> Option<u64> {
    let t = chrono::DateTime::parse_from_rfc3339(iso).ok()?.timestamp_millis();
    (t > 0).then_some(t as u64)
}

/// Ligne de transcription, réduite à ce qui sert ici. Tout le reste (contenu des messages, pièces
/// jointes, formes de requête) est ignoré par serde.
#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "isSidechain")]
    sidechain: Option<bool>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
    #[serde(rename = "customTitle")]
    custom_title: Option<String>,
    message: Option<RawMsg>,
}

#[derive(Deserialize)]
struct RawMsg {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
}

/// `usage` d'un message. Les champs absents ou nuls valent 0 ; `iterations` et le reste sont ignorés
/// (les totaux du haut les résument déjà).
#[derive(Deserialize, Default)]
struct RawUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

/// Première demande de l'utilisateur, pour servir de libellé quand la session n'a pas de titre.
#[derive(Deserialize)]
struct RawUser {
    message: Option<RawUserMsg>,
}

#[derive(Deserialize)]
struct RawUserMsg {
    content: Option<serde_json::Value>,
}

/// État vierge d'une transcription numérotée `idx`.
fn next_state(idx: u32) -> FileState {
    FileState { idx, ..Default::default() }
}

/// Lit les octets neufs de `path` et met `st` à jour.
/// Renvoie (octets lus, messages comptés, transcription réécrite — à relire depuis le début).
fn read_new(path: &Path, st: &mut FileState, owners: &mut HashMap<String, u32>) -> (u64, u64, bool) {
    let Ok(meta) = std::fs::metadata(path) else { return (0, 0, false) };
    let len = meta.len();
    if len == st.offset {
        return (0, 0, false);
    }
    // Fichier raccourci ou réécrit : l'état n'a plus de sens, l'appelant le remet à zéro.
    if len < st.offset {
        return (0, 0, true);
    }
    let Ok(mut f) = std::fs::File::open(path) else { return (0, 0, false) };
    if f.seek(SeekFrom::Start(st.offset)).is_err() {
        return (0, 0, false);
    }
    if st.session.is_empty() {
        st.session = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    }
    let mut r = BufReader::with_capacity(64 * 1024, f);
    let (mut read, mut msgs) = (0u64, 0u64);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                // Ligne incomplète (fichier en cours d'écriture) : on la relira au prochain passage.
                if !buf.ends_with(b"\n") {
                    break;
                }
                read += n as u64;
                st.offset += n as u64;
                if buf.len() <= MAX_LINE && feed(st, &String::from_utf8_lossy(&buf), owners) {
                    msgs += 1;
                }
            }
            Err(_) => break,
        }
    }
    (read, msgs, false)
}

/// Prend en compte une ligne de transcription. Renvoie vrai si elle a ajouté un message.
fn feed(st: &mut FileState, line: &str, owners: &mut HashMap<String, u32>) -> bool {
    let has_usage = line.contains("\"usage\"");
    let has_title = line.contains("Title\"");
    let try_label = st.label.is_empty() && st.label_tries < LABEL_TRIES && line.contains("\"user\"");
    if !has_usage && !has_title && !try_label {
        return false;
    }
    let Ok(v) = serde_json::from_str::<RawLine>(line) else { return false };
    if let Some(cwd) = v.cwd.filter(|c| !c.is_empty()) {
        st.cwd = cwd;
    }
    if let Some(sid) = v.session_id.filter(|s| !s.is_empty()) {
        st.session = sid;
    }
    if let Some(t) = v.ai_title.or(v.custom_title).filter(|t| !t.trim().is_empty()) {
        st.label = clip(t.trim());
        return false;
    }
    let sidechain = v.sidechain.unwrap_or(false);
    if try_label && v.kind.as_deref() == Some("user") && !sidechain {
        st.label_tries += 1;
        if let Some(text) = serde_json::from_str::<RawUser>(line).ok().and_then(|u| u.message).and_then(|m| m.content).as_ref().and_then(first_text) {
            st.label = clip(&text);
        }
    }
    if v.kind.as_deref() != Some("assistant") {
        return false;
    }
    let Some(msg) = v.message else { return false };
    let Some(u) = msg.usage else { return false };
    // Un message arrive en autant de lignes qu'il a de blocs de contenu, et une reprise le recopie
    // dans une autre transcription : une seule occurrence doit compter, où qu'elle soit.
    if let Some(id) = msg.id.filter(|i| !i.is_empty()) {
        if owners.contains_key(&id) {
            return false;
        }
        owners.insert(id, st.idx);
    }
    let tally = Tally {
        input: u.input_tokens.unwrap_or(0),
        output: u.output_tokens.unwrap_or(0),
        cache_read: u.cache_read_input_tokens.unwrap_or(0),
        cache_write: u.cache_creation_input_tokens.unwrap_or(0),
        messages: 1,
    };
    let ms = v.timestamp.as_deref().and_then(epoch_ms).unwrap_or(0);
    st.msgs.push(Msg { ms, model: msg.model.unwrap_or_else(|| "?".into()), sidechain, tally });
    true
}

/// Premier texte d'un contenu de message (chaîne simple, ou premier bloc `text` d'un tableau).
fn first_text(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    v.as_array()?.iter().find_map(|b| b.get("text").and_then(|t| t.as_str()).map(String::from))
}

/// Libellé d'une ligne : une seule ligne, bornée, sans marqueur de collage.
fn clip(text: &str) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = one.split("<pasted_content").next().unwrap_or(&one).trim().to_string();
    if cut.chars().count() <= LABEL_CHARS {
        return cut;
    }
    cut.chars().take(LABEL_CHARS).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("essaim-usage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(p.join("projet-a")).unwrap();
        p
    }

    /// Ligne d'`usage` d'un message assistant, telle que `claude` l'écrit.
    fn ligne(id: &str, ts: &str, model: &str, (i, o, r, w): (u64, u64, u64, u64)) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":false,"timestamp":"{ts}","sessionId":"s1","cwd":"/p","message":{{"id":"{id}","model":"{model}","role":"assistant","content":[{{"type":"text","text":"x"}}],"usage":{{"input_tokens":{i},"output_tokens":{o},"cache_read_input_tokens":{r},"cache_creation_input_tokens":{w},"iterations":[{{"input_tokens":999}}]}}}}}}"#
        )
    }

    #[test]
    fn blocs_du_meme_message_comptes_une_fois() {
        let dir = tmp();
        let f = dir.join("projet-a/s1.jsonl");
        let l = ligne("msg_1", "2026-10-30T12:00:00.000Z", "claude-opus-5", (10, 20, 300, 40));
        // Deux blocs de contenu : deux lignes, un seul message facturé.
        std::fs::write(&f, format!("{l}\n{l}\n")).unwrap();
        let mut sc = Scanner::with_dir(dir);
        let r = sc.refresh();
        assert_eq!((r.files, r.messages), (1, 1));
        let w = sc.window(0, 1_793_000_000_000);
        assert_eq!(w.total.volume(), 370);
        assert_eq!(w.total.messages, 1);
        assert_eq!(w.total.volume() - w.total.cache_read, 70, "hors cache lu, le message pèse 70 jetons");
        assert_eq!(w.sessions.len(), 1);
        assert_eq!(w.sessions[0].cwd, "/p");
        assert_eq!(w.models, vec![("claude-opus-5".to_string(), w.total)]);
    }

    #[test]
    fn message_recopie_par_une_reprise_compte_une_fois() {
        let dir = tmp();
        // Deux transcriptions qui portent le même message : une session reprise recopie l'historique.
        let l = ligne("msg_1", "2026-10-30T12:00:00.000Z", "claude-opus-5", (10, 20, 300, 40));
        std::fs::write(dir.join("projet-a/origine.jsonl"), format!("{l}\n")).unwrap();
        std::fs::write(dir.join("projet-a/reprise.jsonl"), format!("{}\n", l.replace("\"s1\"", "\"s2\""))).unwrap();
        let mut sc = Scanner::with_dir(dir.clone());
        assert_eq!(sc.refresh().messages, 1, "le message recopié ne doit pas être recompté");
        let w = sc.window(0, 1_793_000_000_000);
        assert_eq!(w.total.volume(), 370);
        // Attribué à une seule session, et la même d'un rafraîchissement à l'autre.
        let avec: Vec<&str> = w.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(avec.len(), 1);
        sc.refresh();
        assert_eq!(sc.window(0, 1_793_000_000_000).sessions.iter().map(|s| s.id.clone()).collect::<Vec<_>>(), vec![avec[0].to_string()]);
        // La transcription propriétaire disparaît : ses jetons reviennent à la copie, sans double compte.
        std::fs::remove_file(dir.join(format!("projet-a/{}", if avec[0] == "s1" { "origine.jsonl" } else { "reprise.jsonl" }))).unwrap();
        sc.refresh();
        let w = sc.window(0, 1_793_000_000_000);
        assert_eq!(w.total.volume(), 370);
        assert_eq!(w.sessions.len(), 1);
    }

    #[test]
    fn lecture_incrementale_puis_reecriture() {
        let dir = tmp();
        let f = dir.join("projet-a/s1.jsonl");
        std::fs::write(&f, ligne("m1", "2026-10-30T12:00:00.000Z", "haiku", (1, 1, 0, 0)) + "\n").unwrap();
        let mut sc = Scanner::with_dir(dir.clone());
        assert_eq!(sc.refresh().messages, 1);
        // Rien de neuf : aucun octet relu.
        let r = sc.refresh();
        assert_eq!((r.new_bytes, r.messages), (0, 0));
        // Ajout en fin de fichier : seuls les octets neufs sont lus.
        let suite = ligne("m2", "2026-10-30T12:05:00.000Z", "haiku", (2, 2, 0, 0)) + "\n";
        std::fs::write(&f, std::fs::read_to_string(&f).unwrap() + &suite).unwrap();
        let r = sc.refresh();
        assert_eq!(r.messages, 1);
        assert_eq!(r.new_bytes, suite.len() as u64);
        assert_eq!(sc.window(0, 1_793_000_000_000).total.volume(), 6);
        // Fichier réécrit plus court : l'état est reconstruit sans double comptage.
        std::fs::write(&f, ligne("m9", "2026-10-30T12:00:00.000Z", "haiku", (5, 0, 0, 0)) + "\n").unwrap();
        sc.refresh();
        assert_eq!(sc.window(0, 1_793_000_000_000).total.volume(), 5);
        // Transcription supprimée : oubliée.
        std::fs::remove_file(&f).unwrap();
        sc.refresh();
        assert_eq!(sc.files(), 0);
        assert_eq!(sc.window(0, 1_793_000_000_000).total.volume(), 0);
    }

    #[test]
    fn ligne_incomplete_relue_plus_tard() {
        let dir = tmp();
        let f = dir.join("projet-a/s1.jsonl");
        let l = ligne("m1", "2026-10-30T12:00:00.000Z", "haiku", (7, 0, 0, 0));
        let (debut, fin) = l.split_at(l.len() / 2);
        std::fs::write(&f, debut).unwrap();
        let mut sc = Scanner::with_dir(dir);
        assert_eq!(sc.refresh().messages, 0, "une ligne sans fin de ligne n'est pas analysée");
        std::fs::write(&f, format!("{debut}{fin}\n")).unwrap();
        assert_eq!(sc.refresh().messages, 1);
        assert_eq!(sc.window(0, 1_793_000_000_000).total.volume(), 7);
    }

    #[test]
    fn libelle_titre_puis_premiere_demande() {
        let dir = tmp();
        let a = dir.join("projet-a/s1.jsonl");
        std::fs::write(
            &a,
            format!(
                "{}\n{}\n{}\n",
                r#"{"type":"user","isSidechain":false,"sessionId":"s1","cwd":"/p","message":{"role":"user","content":"compte les jetons   s'il te plait"}}"#,
                r#"{"type":"ai-title","aiTitle":"Tableau de bord des jetons","sessionId":"s1"}"#,
                ligne("m1", "2026-10-30T12:00:00.000Z", "haiku", (1, 0, 0, 0))
            ),
        )
        .unwrap();
        let b = dir.join("projet-a/s2.jsonl");
        std::fs::write(
            &b,
            format!(
                "{}\n{}\n",
                r#"{"type":"user","isSidechain":false,"sessionId":"s2","cwd":"/p","message":{"role":"user","content":[{"type":"text","text":"sans titre"}]}}"#,
                ligne("m2", "2026-10-30T12:00:00.000Z", "haiku", (1, 0, 0, 0)).replace("\"s1\"", "\"s2\"")
            ),
        )
        .unwrap();
        let mut sc = Scanner::with_dir(dir);
        sc.refresh();
        let w = sc.window(0, 1_793_000_000_000);
        let label = |id: &str| w.sessions.iter().find(|s| s.id == id).map(|s| s.label.clone()).unwrap_or_default();
        assert_eq!(label("s1"), "Tableau de bord des jetons", "le titre a le dernier mot");
        assert_eq!(label("s2"), "sans titre");
    }

    #[test]
    fn tranches_alignees_et_creux_visibles() {
        let dir = tmp();
        let f = dir.join("projet-a/s1.jsonl");
        // Deux messages à deux heures d'écart : la tranche vide entre les deux doit apparaître.
        std::fs::write(
            &f,
            format!(
                "{}\n{}\n",
                ligne("m1", "2026-10-30T10:30:00.000Z", "haiku", (100, 0, 0, 0)),
                ligne("m2", "2026-10-30T12:30:00.000Z", "haiku", (200, 0, 0, 0))
            ),
        )
        .unwrap();
        let mut sc = Scanner::with_dir(dir);
        sc.refresh();
        let now = epoch_ms("2026-10-30T13:00:00.000Z").unwrap();
        let w = sc.window(now - 24 * 3_600_000, now);
        assert_eq!(w.step_ms, 3_600_000, "fenêtre d'un jour : tranches d'une heure");
        assert_eq!(w.buckets.len(), 25);
        let pleines: Vec<u64> = w.buckets.iter().filter(|(_, t)| t.volume() > 0).map(|(_, t)| t.volume()).collect();
        assert_eq!(pleines, vec![100, 200]);
        assert!(w.buckets.windows(2).all(|p| p[1].0 - p[0].0 == w.step_ms), "tranches contiguës");
        // Fenêtre courte : les messages hors fenêtre ne comptent plus.
        let courte = sc.window(now - 3_600_000, now);
        assert_eq!(courte.total.volume(), 200);
        assert_eq!(courte.step_ms, 600_000);
    }

    #[test]
    fn lignes_illisibles_et_usage_partiel_ignores_sans_casser() {
        let dir = tmp();
        let f = dir.join("projet-a/s1.jsonl");
        std::fs::write(
            &f,
            format!(
                "{}\n{}\n{}\n{}\n",
                "{pas du json",
                r#"{"type":"assistant","timestamp":"pas une date","sessionId":"s1","message":{"id":"m0","model":"haiku","usage":{"input_tokens":5}}}"#,
                r#"{"type":"assistant","sessionId":"s1","message":{"id":"m3","model":"haiku","usage":{"input_tokens":null,"output_tokens":3}}}"#,
                ligne("m1", "2026-10-30T12:00:00.000Z", "haiku", (1, 0, 0, 0))
            ),
        )
        .unwrap();
        let mut sc = Scanner::with_dir(dir);
        assert_eq!(sc.refresh().messages, 3);
        assert_eq!(sc.window(0, 1_793_000_000_000).total.volume(), 9);
    }

    #[test]
    fn horodatage_et_pas_des_tranches() {
        assert_eq!(epoch_ms("2026-10-30T12:15:29.959Z"), Some(1_793_362_529_959));
        assert_eq!(epoch_ms("2026-10-30T12:15:29+02:00"), Some(1_793_355_329_000));
        assert_eq!(epoch_ms("hier"), None);
        let now = 1_793_362_529_959;
        assert_eq!(step_ms(now - 6 * 3_600_000, now), 600_000);
        assert_eq!(step_ms(now - 24 * 3_600_000, now), 3_600_000);
        assert_eq!(step_ms(now - 7 * 24 * 3_600_000, now), 6 * 3_600_000);
        assert_eq!(step_ms(0, now), 24 * 3_600_000, "tout l'historique : une tranche par jour");
        // Alignement : une tranche d'un jour commence à minuit local, pas à minuit UTC.
        let step = 24 * 3_600_000;
        let b = bucket_of(now, step, local_offset_ms());
        assert!(b <= now && now - b < step);
        assert_eq!(bucket_of(b + step - 1, step, local_offset_ms()), b);
    }

    #[test]
    fn libelle_borne_et_collage_retire() {
        assert_eq!(clip("  a   b \n c "), "a b c");
        assert_eq!(clip("question <pasted_content id=\"1\">tout un fichier"), "question");
        let long = "é".repeat(400);
        let c = clip(&long);
        assert_eq!(c.chars().count(), LABEL_CHARS + 1, "tronqué sur un caractère, pas sur un octet");
    }
}
