use crate::model::company::Company;
use super::app::*;
use super::ui::draw;
use crate::runtime::claude_proc::ClaudeEvent;
use crate::runtime::executor::{NodeStatus, RunEvent};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;
use tokio::sync::mpsc;

fn tmp_root() -> PathBuf {
    let d = std::env::temp_dir().join(format!("essaim-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(d.join(".claude/agents")).unwrap();
    d
}

fn new_app(root: &PathBuf) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    App::new(root.clone(), tx)
}

fn screen(app: &App) -> String {
    let mut t = Terminal::new(TestBackend::new(110, 34)).unwrap();
    t.draw(|f| draw(f, app)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn press(app: &mut App, c: KeyCode) {
    app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn write_agent(root: &PathBuf, name: &str) {
    std::fs::write(root.join(format!(".claude/agents/{name}.md")), format!("---\nname: {name}\ndescription: desc de {name}\n---\nprompt de {name}\n")).unwrap();
}

#[test]
fn onglet_agents_liste_et_detail() {
    let root = tmp_root();
    write_agent(&root, "poete");
    let app = new_app(&root);
    let s = screen(&app);
    assert!(s.contains("Agents"));
    assert!(s.contains("poete"));
    assert!(s.contains("desc de poete"));
    assert!(s.contains("prompt de poete"));
}

#[test]
fn creation_d_agent_au_clavier() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('N'));
    assert!(matches!(app.mode, Mode::EditAgent(_)));
    type_str(&mut app, "relecteur");
    press(&mut app, KeyCode::Tab);
    type_str(&mut app, "Relit le code");
    press(&mut app, KeyCode::Enter); // → modèle (liste de choix)
    assert!(screen(&app).contains("(défaut)") && screen(&app).contains("sonnet"));
    press(&mut app, KeyCode::Right); // défaut → haiku
    press(&mut app, KeyCode::Tab); // → outils
    type_str(&mut app, "Read, Grep");
    press(&mut app, KeyCode::Tab); // → prompt
    type_str(&mut app, "Tu relis.");
    press(&mut app, KeyCode::Enter);
    type_str(&mut app, "Sois bref.");
    assert!(screen(&app).contains("Nouvel agent"));
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

    assert!(matches!(app.mode, Mode::Normal), "{}", app.status);
    let src = std::fs::read_to_string(root.join(".claude/agents/relecteur.md")).unwrap();
    let a = crate::model::agent::AgentDef::parse(&src).unwrap();
    assert_eq!(a.description, "Relit le code");
    assert_eq!(a.model.as_deref(), Some("haiku"));
    assert_eq!(a.tools, Some(vec!["Read".into(), "Grep".into()]));
    assert_eq!(a.prompt, "Tu relis.\nSois bref.");
    assert!(app.agents.iter().any(|x| x.name == "relecteur"));
}

#[test]
fn sauvegarde_refusee_sans_description() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('N'));
    type_str(&mut app, "x");
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(matches!(app.mode, Mode::EditAgent(_)), "le formulaire reste ouvert");
    assert!(app.status.contains("description"));
    assert!(!root.join(".claude/agents/x.md").exists());
}

#[test]
fn renommage_supprime_l_ancien_fichier() {
    let root = tmp_root();
    write_agent(&root, "ancien");
    let mut app = new_app(&root);
    let i = app.agents.iter().position(|a| a.name == "ancien").unwrap();
    app.agent_sel = i;
    press(&mut app, KeyCode::Char('e'));
    for _ in 0..6 {
        press(&mut app, KeyCode::Backspace);
    }
    type_str(&mut app, "nouveau");
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(root.join(".claude/agents/nouveau.md").exists());
    assert!(!root.join(".claude/agents/ancien.md").exists());
}

#[test]
fn suppression_avec_confirmation() {
    let root = tmp_root();
    write_agent(&root, "poete");
    let mut app = new_app(&root);
    app.agent_sel = app.agents.iter().position(|a| a.name == "poete").unwrap();
    press(&mut app, KeyCode::Char('D'));
    assert!(screen(&app).contains("Supprimer"));
    press(&mut app, KeyCode::Char('x')); // refus
    assert!(root.join(".claude/agents/poete.md").exists());
    press(&mut app, KeyCode::Char('D'));
    press(&mut app, KeyCode::Char('y'));
    assert!(!root.join(".claude/agents/poete.md").exists());
}

#[test]
fn affichage_d_un_run() {
    let root = tmp_root();
    write_agent(&root, "poete");
    let mut app = new_app(&root);
    let (cancel, _rx) = tokio::sync::watch::channel(false);
    app.run = Some(RunView {
        name: "demo".into(),
        nodes: ["a", "b"].map(|id| NodeView::new(id, "poete")).into(),
        sel: 0,
        cost: 0.0,
        finished: None,
        cancel,
    });
    app.tab = Tab::Run;
    app.on_run_event(RunEvent::NodeStarted { id: "a".into() });
    app.on_run_event(RunEvent::Node { id: "a".into(), event: ClaudeEvent::Text("bonjour\nmonde".into()) });
    app.on_run_event(RunEvent::Node { id: "a".into(), event: ClaudeEvent::ToolUse { name: "Read".into(), input: serde_json::json!({}) } });
    let s = screen(&app);
    assert!(s.contains("en cours"));
    assert!(s.contains("bonjour") && s.contains("monde"));
    assert!(s.contains("outil : Read"));

    app.on_run_event(RunEvent::NodeFinished { id: "a".into(), status: NodeStatus::Failed("boum".into()), cost_usd: 0.01 });
    app.on_run_event(RunEvent::NodeFinished { id: "b".into(), status: NodeStatus::Skipped("dépendance a non réussie".into()), cost_usd: 0.0 });
    app.on_run_event(RunEvent::RunFinished { ok: false, cost_usd: 0.01 });
    let s = screen(&app);
    assert!(s.contains("terminé avec erreurs"));
    assert!(s.contains("✖") && s.contains("⊘"));
    assert!(s.contains("— boum"));
    assert!(!app.running());
}

#[test]
fn navigation_onglets_et_quitter_annule_le_run() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.tab, Tab::Company);
    press(&mut app, KeyCode::BackTab);
    press(&mut app, KeyCode::BackTab);
    assert_eq!(app.tab, Tab::Terminal);
    let (cancel, rx) = tokio::sync::watch::channel(false);
    app.run = Some(RunView { name: "x".into(), nodes: vec![], sel: 0, cost: 0.0, finished: None, cancel });
    press(&mut app, KeyCode::Char('q'));
    assert!(app.quit);
    assert!(*rx.borrow(), "le run en cours est annulé à la sortie");
}

#[test]
fn rendu_etroit_sans_panique() {
    let root = tmp_root();
    write_agent(&root, "poete");
    let mut app = new_app(&root);
    for (w, h) in [(20, 8), (40, 12), (80, 24)] {
        for key in ['1', '2', '3', '4', '5'] {
            press(&mut app, KeyCode::Char(key));
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| draw(f, &app)).unwrap();
        }
        press(&mut app, KeyCode::Char('1'));
        press(&mut app, KeyCode::Char('N'));
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, &app)).unwrap();
        press(&mut app, KeyCode::Esc);
    }
}

fn fake_session(id: &str, name: &str, state: &str) -> crate::runtime::sessions::Session {
    crate::runtime::sessions::Session {
        id: Some(id.into()),
        pid: Some(1),
        cwd: "/projet".into(),
        kind: "background".into(),
        started_at: Some(1),
        session_id: Some(format!("{id}-introuvable")),
        name: Some(name.into()),
        status: Some("idle".into()),
        state: Some(state.into()),
    }
}

#[test]
fn onglet_sessions_vide_et_note_cloud() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('4'));
    let s = screen(&app);
    assert!(s.contains("Aucune session background"));
    assert!(s.contains("cloud"));
}

#[test]
fn onglet_sessions_liste_selection_et_detail() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('4'));
    app.on_event(AppEvent::Sessions {
        list: Some(vec![fake_session("aaa11111", "essaim-ab12cd-noter", "done"), fake_session("bbb22222", "autre", "working")]),
        error: None,
    });
    let s = screen(&app);
    assert!(s.contains("Sessions background (2)"));
    assert!(s.contains("essaim-ab12cd-noter") && s.contains("autre"));
    assert!(s.contains("aaa11111"), "détail de la première session");
    press(&mut app, KeyCode::Char('j'));
    assert!(screen(&app).contains("bbb22222"));
    // La sélection suit la session (par id) quand la liste est rafraîchie dans un autre ordre.
    app.on_event(AppEvent::Sessions {
        list: Some(vec![fake_session("aaa11111", "essaim-ab12cd-noter", "done"), fake_session("ccc33333", "nouvelle", "working"), fake_session("bbb22222", "autre", "done")]),
        error: None,
    });
    assert_eq!(app.selected_session().unwrap().id.as_deref(), Some("bbb22222"));
}

#[test]
fn erreur_de_listing_conserve_la_liste() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('4'));
    app.on_event(AppEvent::Sessions { list: Some(vec![fake_session("aaa11111", "x", "done")]), error: None });
    app.on_event(AppEvent::Sessions { list: None, error: Some("claude introuvable".into()) });
    assert_eq!(app.sessions.len(), 1);
    assert!(screen(&app).contains("claude introuvable"));
}

#[test]
fn attacher_et_supprimer_une_session() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('4'));
    app.on_event(AppEvent::Sessions { list: Some(vec![fake_session("aaa11111", "essaim-x", "done")]), error: None });
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.attach_request.as_deref(), Some("aaa11111"));
    press(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.mode, Mode::ConfirmRemoveSession(..)));
    assert!(screen(&app).contains("Supprimer la session"));
    press(&mut app, KeyCode::Char('n')); // refus : rien ne part
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.status.is_empty());
}

#[test]
fn notice_remplace_le_statut() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.on_event(AppEvent::Notice("session aaa arrêtée".into()));
    assert!(screen(&app).contains("session aaa arrêtée"));
}

fn run_view(names: &[&str]) -> RunView {
    let (cancel, _rx) = tokio::sync::watch::channel(false);
    RunView {
        name: "demo".into(),
        nodes: names.iter().map(|id| NodeView::new(id, "poete")).collect(),
        sel: 0,
        cost: 0.0,
        finished: None,
        cancel,
    }
}

#[test]
fn le_run_est_enregistre_a_la_fin_et_visible_dans_l_historique() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.run = Some(run_view(&["a", "b"]));
    app.run_id = Some(crate::runtime::store::new_id(5000));
    app.run_started_ms = 5000;
    app.tab = Tab::Run;
    app.on_run_event(RunEvent::Node { id: "a".into(), event: ClaudeEvent::Text("salut".into()) });
    app.on_run_event(RunEvent::NodeFinished { id: "a".into(), status: NodeStatus::Succeeded, cost_usd: 0.02 });
    app.on_run_event(RunEvent::NodeFinished { id: "b".into(), status: NodeStatus::Failed("boum".into()), cost_usd: 0.0 });
    assert!(app.history.is_empty(), "rien d'écrit avant la fin du run");
    app.on_run_event(RunEvent::RunFinished { ok: false, cost_usd: 0.02 });
    assert_eq!(app.history.len(), 1);
    assert_eq!(app.history[0].finished, Some(false));

    press(&mut app, KeyCode::Char('h'));
    let s = screen(&app);
    assert!(s.contains("Historique (1)") && s.contains("demo") && s.contains("échec"));
    assert!(s.contains("a [poete]") && s.contains("b [poete]"));
}

#[test]
fn rouvrir_un_run_passe() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.run = Some(run_view(&["a"]));
    app.run_id = Some(crate::runtime::store::new_id(7000));
    app.tab = Tab::Run;
    app.on_run_event(RunEvent::Node { id: "a".into(), event: ClaudeEvent::Text("journal du nœud a".into()) });
    app.on_run_event(RunEvent::NodeFinished { id: "a".into(), status: NodeStatus::Succeeded, cost_usd: 0.0 });
    app.on_run_event(RunEvent::RunFinished { ok: true, cost_usd: 0.0 });

    // Nouvelle session de l'outil : l'historique vient du disque.
    let mut app2 = new_app(&root);
    app2.tab = Tab::Run;
    assert!(app2.run.is_none());
    press(&mut app2, KeyCode::Char('h'));
    press(&mut app2, KeyCode::Enter);
    assert!(!app2.show_history);
    let s = screen(&app2);
    assert!(s.contains("terminé ✔") && s.contains("journal du nœud a"));
    assert!(app2.run_id.is_none(), "la relecture n'écrase pas l'historique");
}

#[test]
fn run_interrompu_et_suppression_d_historique() {
    let root = tmp_root();
    let rec = crate::runtime::store::RunRecord {
        id: crate::runtime::store::new_id(9000),
        pipeline: "coupe".into(),
        started_ms: 9000,
        finished: None,
        cost_usd: 0.0,
        nodes: vec![crate::runtime::store::NodeRecord { id: "a".into(), agent: "x".into(), label: String::new(), depth: 0, status: None, cost: 0.0, log: vec![] }],
    };
    crate::runtime::store::save(&root, &rec).unwrap();
    let mut app = new_app(&root);
    app.tab = Tab::Run;
    press(&mut app, KeyCode::Char('h'));
    assert!(screen(&app).contains("interrompu"));
    press(&mut app, KeyCode::Enter);
    assert!(screen(&app).contains("terminé avec erreurs"), "un run interrompu s'affiche comme en erreur");
    press(&mut app, KeyCode::Char('h'));
    press(&mut app, KeyCode::Char('D'));
    assert!(matches!(app.mode, Mode::ConfirmAction(_)));
    assert!(!app.history.is_empty(), "rien n'est supprimé avant confirmation");
    press(&mut app, KeyCode::Char('y'));
    assert!(app.history.is_empty());
    assert!(screen(&app).contains("Aucun run enregistré"));
}

#[test]
fn historique_refuse_pendant_un_run() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.run = Some(run_view(&["a"])); // finished = None → en cours
    app.tab = Tab::Run;
    app.history = vec![crate::runtime::store::RunRecord { id: "1".into(), pipeline: "p".into(), started_ms: 1, finished: Some(true), cost_usd: 0.0, nodes: vec![] }];
    press(&mut app, KeyCode::Char('h'));
    press(&mut app, KeyCode::Enter);
    assert!(app.status.contains("en cours"));
    assert_eq!(app.run.as_ref().unwrap().nodes.len(), 1);
}

fn model_field(app: &App) -> (Vec<String>, usize) {
    match &app.mode {
        Mode::EditAgent(f) => match &f.fields[2] {
            Field::Choice { options, idx } => (options.clone(), *idx),
            _ => panic!("le modèle doit être une liste de choix"),
        },
        _ => panic!("formulaire fermé"),
    }
}

#[test]
fn le_modele_est_une_liste_de_choix_circulaire() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('N'));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab); // focus sur le modèle
    let (opts, idx) = model_field(&app);
    assert_eq!(opts, vec!["", "haiku", "sonnet", "opus", "fable"]);
    assert_eq!(idx, 0, "défaut au départ");
    press(&mut app, KeyCode::Left);
    assert_eq!(model_field(&app).1, 4, "retour au début = dernier choix");
    press(&mut app, KeyCode::Right);
    assert_eq!(model_field(&app).1, 0);
    type_str(&mut app, "xyz"); // la saisie libre est ignorée
    assert_eq!(model_field(&app).1, 0);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(model_field(&app).1, 2);
    assert!(screen(&app).contains("sonnet"));
}

#[test]
fn modele_par_defaut_non_ecrit_et_modele_choisi_ecrit() {
    let root = tmp_root();
    let mut app = new_app(&root);
    for (name, rights) in [("sans-modele", 0), ("avec-opus", 3)] {
        press(&mut app, KeyCode::Char('N'));
        type_str(&mut app, name);
        press(&mut app, KeyCode::Tab);
        type_str(&mut app, "desc");
        press(&mut app, KeyCode::Tab);
        for _ in 0..rights {
            press(&mut app, KeyCode::Right);
        }
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Normal), "{}", app.status);
    }
    let read = |n: &str| std::fs::read_to_string(root.join(format!(".claude/agents/{n}.md"))).unwrap();
    assert!(!read("sans-modele").contains("model:"));
    assert!(read("avec-opus").contains("model: opus"));
}

#[test]
fn modele_personnalise_conserve_a_l_edition() {
    let root = tmp_root();
    std::fs::write(root.join(".claude/agents/perso.md"), "---\nname: perso\ndescription: d\nmodel: claude-opus-5-5\n---\np\n").unwrap();
    let mut app = new_app(&root);
    app.agent_sel = app.agents.iter().position(|a| a.name == "perso").unwrap();
    press(&mut app, KeyCode::Char('e'));
    let (opts, idx) = model_field(&app);
    assert_eq!(opts.last().unwrap(), "claude-opus-5-5");
    assert_eq!(opts[idx], "claude-opus-5-5");
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(std::fs::read_to_string(root.join(".claude/agents/perso.md")).unwrap().contains("model: claude-opus-5-5"));
}

#[test]
fn n_demande_l_assistant_avec_les_noms_pris() {
    let root = tmp_root();
    write_agent(&root, "poete");
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('n'));
    assert!(matches!(app.mode, Mode::Normal), "plus de formulaire");
    let (kind, args) = app.assistant_request.take().expect("assistant demandé");
    assert_eq!(kind, AssistantKind::Agent);
    assert!(args.iter().any(|a| a == "plan"));
    assert!(args.iter().any(|a| a.contains("poete")));
}

#[test]
fn retour_de_l_assistant_selectionne_le_nouvel_agent() {
    let root = tmp_root();
    write_agent(&root, "poete");
    let mut app = new_app(&root);
    let before: Vec<String> = app.agents.iter().map(|a| a.name.clone()).collect();
    write_agent(&root, "zzz-nouveau");
    app.after_assistant(AssistantKind::Agent, &before);
    assert_eq!(app.agents[app.agent_sel].name, "zzz-nouveau");
    assert_eq!(app.status, "agent « zzz-nouveau » créé");
}

#[test]
fn retour_de_l_assistant_signale_un_fichier_illisible_ou_rien() {
    let root = tmp_root();
    let mut app = new_app(&root);
    let before: Vec<String> = app.agents.iter().map(|a| a.name.clone()).collect();
    app.after_assistant(AssistantKind::Agent, &before);
    assert_eq!(app.status, "aucun agent créé");
    std::fs::write(root.join(".claude/agents/casse.md"), "n'importe quoi").unwrap();
    app.after_assistant(AssistantKind::Agent, &before);
    assert!(app.status.contains("casse.md") && app.status.contains("illisible"));
}

#[test]
fn les_touches_vont_au_processus_et_s_affichent_dans_l_onglet() {
    let (mut app, mut rx) = term_app("printf 'nom ? '; read x; printf 'bonjour %s\\n' \"$x\"; sleep 30");
    pump_until(&mut app, &mut rx, "invite", |a| screen(a).contains("nom ?"));
    type_str(&mut app, "Léa");
    press(&mut app, KeyCode::Enter);
    pump_until(&mut app, &mut rx, "réponse du processus", |a| screen(a).contains("bonjour Léa"));
    assert!(screen(&app).contains("Claude ●"), "pastille de session active dans la barre d'onglets");
    assert!(screen(&app).contains("Ctrl+\\"));
}

fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

#[test]
fn changer_d_onglet_depuis_la_session_sans_quitter_le_focus_a_la_main() {
    let (mut app, mut rx) = term_app("printf 'pret\\n'; cat");
    pump_until(&mut app, &mut rx, "sortie", |a| screen(a).contains("pret"));
    assert!(app.term_focus);

    // F3 : onglet Run, directement, sans Ctrl+\ préalable.
    press(&mut app, KeyCode::F(3));
    assert_eq!(app.tab, Tab::Run);
    assert!(!app.term_focus);
    assert!(app.term.is_some(), "la session continue en arrière-plan");

    // Retour par F5 : le focus revient tout seul, les frappes repartent dans la session.
    press(&mut app, KeyCode::F(5));
    assert_eq!(app.tab, Tab::Terminal);
    assert!(app.term_focus);
    type_str(&mut app, "ok");
    pump_until(&mut app, &mut rx, "frappes reçues", |a| screen(a).contains("ok"));
}

#[test]
fn les_raccourcis_ne_sont_pas_envoyes_au_processus() {
    let (mut app, mut rx) = term_app("cat");
    app.on_key(alt('1'));
    assert_eq!(app.tab, Tab::Agents);
    app.on_key(alt('5'));
    assert_eq!(app.tab, Tab::Terminal);
    std::thread::sleep(Duration::from_millis(150));
    while let Ok(ev) = rx.try_recv() {
        app.on_event(ev);
    }
    let s = screen(&app);
    assert!(!s.contains("^[") && !s.contains("1^"), "rien n'a fuité dans la session : {s}");
}

#[test]
fn raccourcis_azerty_et_ctrl_page() {
    let root = tmp_root();
    let mut app = new_app(&root);
    for (c, tab) in [('&', Tab::Agents), ('é', Tab::Company), ('"', Tab::Run), ('\'', Tab::Sessions), ('(', Tab::Terminal)] {
        app.on_key(alt(c));
        assert_eq!(app.tab, tab, "Alt+{c}");
    }
    app.on_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL));
    assert_eq!(app.tab, Tab::Sessions);
    app.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL));
    assert_eq!(app.tab, Tab::Agents, "le cycle boucle");
}

#[test]
fn les_raccourcis_sont_ignores_dans_un_formulaire() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('N'));
    app.on_key(alt('3'));
    press(&mut app, KeyCode::F(3));
    assert_eq!(app.tab, Tab::Agents);
    assert!(matches!(app.mode, Mode::EditAgent(_)), "le formulaire reste ouvert");
}

#[test]
fn les_autres_touches_de_fonction_vont_a_la_session() {
    let (mut app, mut rx) = term_app("cat -v");
    press(&mut app, KeyCode::F(7)); // F7 n'est pas un raccourci d'onglet
    pump_until(&mut app, &mut rx, "F7 reçue", |a| screen(a).contains("^[[18~"));
    assert_eq!(app.tab, Tab::Terminal);
}

#[test]
fn ctrl_c_et_q_sont_pour_le_processus_tant_que_le_focus_y_est() {
    let (mut app, mut rx) = term_app("printf 'ok\\n'; sleep 30");
    pump_until(&mut app, &mut rx, "sortie", |a| screen(a).contains("ok"));
    press(&mut app, KeyCode::Char('q'));
    app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(!app.quit, "ni q ni Ctrl+C ne quittent l'outil");
}

#[test]
fn ctrl_antislash_rend_la_navigation_puis_entree_reprend() {
    let (mut app, mut rx) = term_app("sleep 30");
    // crossterm livre Ctrl+\ sous la forme Ctrl+'4'
    app.on_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL));
    assert!(!app.term_focus);
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(app.tab, Tab::Agents, "navigation libre, la session continue");
    assert!(app.term.is_some());
    press(&mut app, KeyCode::Char('5'));
    assert!(app.term_focus, "revenir sur l'onglet redonne le focus à la session");
    // Ctrl+\ sur l'onglet lui-même : touches de navigation, la session reste visible.
    app.on_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL));
    assert!(!app.term_focus);
    assert!(screen(&app).contains("Entrée reprendre"));
    press(&mut app, KeyCode::Enter);
    assert!(app.term_focus);
    let _ = &mut rx;
}

#[test]
fn x_ferme_la_session_et_ramene_a_l_onglet_d_origine() {
    let (mut app, _rx) = term_app("sleep 30");
    app.term_focus = false;
    press(&mut app, KeyCode::Char('x'));
    assert!(app.term.is_some(), "x demande d'abord confirmation");
    press(&mut app, KeyCode::Char('n'));
    assert!(app.term.is_some());
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.term.is_none());
    assert_eq!(app.tab, Tab::Agents);
}

#[test]
fn fin_rapide_ou_en_erreur_garde_l_ecran_puis_une_touche_ferme() {
    let (mut app, mut rx) = term_app("echo 'erreur: dossier non approuvé'; exit 2");
    pump_until(&mut app, &mut rx, "fin du processus", |a| a.term.as_ref().is_some_and(|t| t.exited));
    let s = screen(&app);
    assert!(s.contains("dossier non approuvé"), "le message d'erreur reste lisible");
    assert!(s.contains("Session terminée (code 2)"));
    assert_eq!(app.tab, Tab::Terminal);
    press(&mut app, KeyCode::Char('a'));
    assert!(app.term.is_none());
    assert_eq!(app.tab, Tab::Agents);
    assert!(app.status.contains("code 2"));
}

#[test]
fn un_exit_perime_ne_ferme_pas_la_session_suivante() {
    let (mut app, _rx) = term_app("sleep 30");
    app.on_event(AppEvent::TermExit(3)); // id d'une ancienne session
    assert!(!app.term.as_ref().unwrap().exited);
}

#[test]
fn le_collage_arrive_d_un_bloc() {
    let (mut app, mut rx) = term_app("cat");
    app.on_event(AppEvent::Paste("ligne1\nligne2".into()));
    pump_until(&mut app, &mut rx, "texte collé", |a| screen(a).contains("ligne1") && screen(a).contains("ligne2"));
    app.term_focus = false;
    app.on_event(AppEvent::Paste("ignoré".into()));
    std::thread::sleep(Duration::from_millis(150));
    while let Ok(ev) = rx.try_recv() {
        app.on_event(ev);
    }
    assert!(!screen(&app).contains("ignoré"), "pas de collage hors focus");
}

#[test]
fn le_terminal_suit_la_taille_de_la_zone() {
    let (mut app, _rx) = term_app("sleep 30");
    app.set_body_size(100, 30);
    assert_eq!(app.term.as_ref().unwrap().size(), (30, 100));
}

#[test]
fn onglet_claude_vide_explique_comment_l_ouvrir() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('5'));
    assert!(screen(&app).contains("Aucune session Claude ouverte"));
}

// ---------- onglet Entreprise ----------

const ACME: &str = r#"name = "acme"
max_calls = 10

[[seat]]
id = "ceo"
title = "CEO"
agent = "boss"

[[seat]]
id = "cto"
title = "CTO"
agent = "poete"
reports_to = "ceo"
expertise = "architecture"

[[seat]]
id = "dev"
title = "Backend Engineer"
agent = "poete"
reports_to = "cto"
builds = true
allow_bash = ["cargo test"]
"#;

fn company_app() -> App {
    let root = tmp_root();
    write_agent(&root, "boss");
    write_agent(&root, "poete");
    std::fs::create_dir_all(root.join(".claude/companies")).unwrap();
    std::fs::write(root.join(".claude/companies/acme.toml"), ACME).unwrap();
    new_app(&root)
}

#[test]
fn onglet_entreprise_arbre_niveaux_et_validation() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    assert_eq!(app.tab, Tab::Company);
    let s = screen(&app);
    assert!(s.contains("Entreprises (1)") && s.contains("acme"));
    assert!(s.contains("organigramme valide"));
    assert!(s.contains("└─ N-2") || s.contains("N-2"), "{s}");
    assert!(s.contains("CEO") && s.contains("CTO") && s.contains("Backend Engineer"));
    assert!(s.contains("⚒"), "marqueur des postes de réalisation");
    assert!(s.contains("conception 5") && s.contains("réalisation 5") && s.contains("= 10"), "estimation affichée : {s}");
}

#[test]
fn detail_du_poste_selectionne() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    assert!(screen(&app).contains("Poste : CEO (N)"));
    press(&mut app, KeyCode::Char('j'));
    let s = screen(&app);
    assert!(s.contains("Poste : CTO (N-1)") && s.contains("Supérieur : CEO"), "{s}");
    assert!(s.contains("architecture"));
    press(&mut app, KeyCode::Char('j'));
    let s = screen(&app);
    assert!(s.contains("Poste : Backend Engineer (N-2)") && s.contains("peut modifier"), "{s}");
    assert!(s.contains("cargo test"));
}

#[test]
fn organigramme_invalide_signale() {
    let mut app = company_app();
    let root = app.root.clone();
    std::fs::write(root.join(".claude/companies/boiteuse.toml"), ACME.replace("agent = \"boss\"", "agent = \"fantome\"")).unwrap();
    std::fs::write(root.join(".claude/companies/illisible.toml"), "= pas du toml").unwrap();
    app.reload();
    press(&mut app, KeyCode::Char('2'));
    let s = screen(&app);
    assert!(s.contains("⚠ boiteuse") && s.contains("⚠ illisible"), "{s}");
    press(&mut app, KeyCode::Char('c')); // → boiteuse
    assert!(screen(&app).contains("agent inconnu"));
    press(&mut app, KeyCode::Char('c')); // → illisible
    assert!(screen(&app).contains("Illisible"));
    press(&mut app, KeyCode::Char('C'));
    assert_eq!(app.comp_sel, 1);
}

#[test]
fn entreprise_vide_et_edition_du_toml() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('2'));
    assert!(screen(&app).contains("Aucune entreprise"));
    press(&mut app, KeyCode::Char('E'));
    assert!(app.editor_request.is_none(), "rien à éditer");

    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('E'));
    assert!(app.editor_request.as_ref().unwrap().ends_with("acme.toml"));
}

#[test]
fn la_selection_de_poste_reste_dans_l_arbre() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    for _ in 0..10 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.seat_sel, 2, "3 postes : dernière ligne");
    for _ in 0..10 {
        press(&mut app, KeyCode::Up);
    }
    assert_eq!(app.seat_sel, 0);
}

#[test]
fn cinq_onglets_et_rendu_etroit() {
    let mut app = company_app();
    for key in ['1', '2', '3', '4', '5'] {
        press(&mut app, KeyCode::Char(key));
        for (w, h) in [(20, 8), (50, 14), (120, 40)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| draw(f, &app)).unwrap();
        }
    }
    assert!(screen(&app).contains("5 Claude"));
    assert!(screen(&app).contains("2 Entreprise"));
}

#[test]
fn run_d_entreprise_noeuds_dynamiques_indentes_et_persistes() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.run = Some(run_view(&[]));
    app.run_id = Some(crate::runtime::store::new_id(8000));
    app.tab = Tab::Run;
    let label = |id: &str, l: &str, depth: usize| RunEvent::Label { id: id.into(), label: l.into(), agent: "boss".into(), depth };
    app.on_run_event(label("ceo/conception/delegate", "CEO · délègue", 0));
    app.on_run_event(RunEvent::NodeStarted { id: "ceo/conception/delegate".into() });
    app.on_run_event(label("cto/conception/delegate", "CTO · délègue", 1));
    app.on_run_event(label("dev/conception/work", "Backend Engineer · analyse", 2));
    app.on_run_event(RunEvent::Node { id: "dev/conception/work".into(), event: ClaudeEvent::Text("analyse en cours".into()) });
    let s = screen(&app);
    assert!(s.contains("CEO · délègue") && s.contains("  CTO · délègue") && s.contains("    Backend Engineer · analyse"), "{s}");
    app.run.as_mut().unwrap().sel = 2;
    assert!(screen(&app).contains("Journal : Backend Engineer · analyse"));
    assert!(screen(&app).contains("analyse en cours"));

    // Une annonce répétée met à jour le nœud existant au lieu d'en créer un second.
    app.on_run_event(label("cto/conception/delegate", "CTO · délègue", 1));
    assert_eq!(app.run.as_ref().unwrap().nodes.len(), 3);

    app.on_run_event(RunEvent::NodeFinished { id: "ceo/conception/delegate".into(), status: NodeStatus::Succeeded, cost_usd: 0.01 });
    app.on_run_event(RunEvent::RunFinished { ok: true, cost_usd: 0.01 });
    let rec = &app.history[0];
    assert_eq!(rec.nodes[1].label, "CTO · délègue");
    assert_eq!(rec.nodes[2].depth, 2);

    // Relecture depuis l'historique : libellés et indentation conservés.
    let mut app2 = new_app(&root);
    app2.tab = Tab::Run;
    press(&mut app2, KeyCode::Char('h'));
    press(&mut app2, KeyCode::Enter);
    assert!(screen(&app2).contains("    Backend Engineer · analyse"));
}

// ---------- aide (Ctrl+K) ----------

fn ctrl_k() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)
}

#[test]
fn indice_d_aide_en_haut_a_droite() {
    let root = tmp_root();
    let app = new_app(&root);
    let s = screen(&app);
    let first = s.lines().next().unwrap();
    assert!(first.contains("Ctrl+K aide"), "{first}");
    assert!(first.find("essaim").unwrap() < first.find("Ctrl+K aide").unwrap());
}

#[test]
fn ctrl_k_ouvre_et_ferme_l_aide() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.on_key(ctrl_k());
    assert!(app.help_open);
    let s = screen(&app);
    assert!(s.contains("aide · Agents") && s.contains("ESSAIM(1)") && s.contains("ONGLET 1"), "{s}");
    assert!(!s.contains("ONGLET 2") && !s.contains("SYNOPSIS"), "seulement l'onglet courant : {s}");
    app.on_key(ctrl_k());
    assert!(!app.help_open);
    app.on_key(ctrl_k());
    press(&mut app, KeyCode::Esc);
    assert!(!app.help_open);
}

#[test]
fn l_aide_capte_les_touches_et_defile() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.on_key(ctrl_k());
    press(&mut app, KeyCode::Char('2')); // ne change pas d'onglet
    press(&mut app, KeyCode::Char('n')); // ne lance pas d'assistant
    assert_eq!(app.tab, Tab::Agents);
    assert!(app.assistant_request.is_none());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.help_scroll, 2);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.help_scroll, 1);
    press(&mut app, KeyCode::Char('q'));
    assert!(!app.help_open && !app.quit, "q ferme l'aide sans quitter l'outil");
    app.on_key(ctrl_k());
    assert_eq!(app.help_scroll, 0, "défilement remis à zéro");
}

#[test]
fn ctrl_k_fonctionne_dans_la_session_claude() {
    let (mut app, _rx) = term_app("sleep 30");
    app.on_key(ctrl_k());
    assert!(app.help_open, "intercepté avant d'atteindre le processus");
    app.on_key(ctrl_k());
    assert!(app.term_focus, "le focus de la session est conservé");
}

#[test]
fn aide_lisible_en_petit_ecran() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.on_key(ctrl_k());
    for (w, h) in [(20, 8), (50, 14), (120, 40)] {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, &app)).unwrap();
    }
}

#[test]
fn pager_comme_less() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.on_key(ctrl_k());
    press(&mut app, KeyCode::Char('m')); // manuel complet
    screen(&app); // le dessin calcule la hauteur de page et le défilement maximal
    let (page, max) = (app.help_page.get() as i32, app.help_max.get());
    assert!(max > 0, "le manuel est plus long que l'écran");
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.help_scroll as i32, page - 1, "espace = une page");
    press(&mut app, KeyCode::Char('b'));
    assert_eq!(app.help_scroll, 0);
    press(&mut app, KeyCode::Char('d'));
    assert_eq!(app.help_scroll as i32, page / 2);
    press(&mut app, KeyCode::Char('u'));
    assert_eq!(app.help_scroll, 0);
    press(&mut app, KeyCode::Char('G'));
    assert_eq!(app.help_scroll, max, "G = fin");
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.help_scroll, max, "pas de défilement au-delà de la fin");
    let s = screen(&app);
    assert!(s.contains("VOIR AUSSI") && s.contains("100%"), "{s}");
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.help_scroll, 0);
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.help_scroll, 0, "pas de défilement avant le début");
}

#[test]
fn le_manuel_suit_la_largeur_du_terminal() {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.on_key(ctrl_k());
    for (w, h) in [(30, 10), (60, 20), (130, 50)] {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, &app)).unwrap();
    }
}

// ---------- missions (touche m) ----------

fn write_brief(root: &PathBuf, id: &str, text: &str) {
    let dir = crate::runtime::missions::create(root, id).unwrap();
    crate::runtime::missions::write_output(&dir, "brief.md", text).unwrap();
}

#[test]
fn m_demande_une_session_boss_en_mode_plan() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('m'));
    let (kind, args) = app.assistant_request.take().expect("assistant demandé");
    assert_eq!(kind, AssistantKind::Mission);
    assert!(args.iter().any(|a| a == "plan"));
    let sys = args.iter().find(|a| a.contains("framing a mission")).unwrap();
    assert!(sys.contains("\"CEO\" of the company \"acme\""));
    assert!(sys.contains("N-1  CTO"), "l'organigramme est fourni au boss");
    assert!(sys.contains("marked ⚒"), "acme a un poste de réalisation");
    // Le dossier de la mission est créé d'avance et son chemin est donné au modèle.
    let dir = app.pending_mission_dir.clone().expect("dossier de mission");
    assert!(dir.is_dir());
    let id = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(sys.contains(&format!(".essaim/missions/{id}/brief.md")));
}

#[test]
fn m_refuse_sans_entreprise_valide() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('m'));
    assert!(app.assistant_request.is_none() && app.status.contains("aucune entreprise"));

    let mut app = company_app();
    std::fs::write(app.root.join(".claude/companies/acme.toml"), ACME.replace("agent = \"boss\"", "agent = \"fantome\"")).unwrap();
    app.reload();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('m'));
    assert!(app.assistant_request.is_none() && app.status.contains("invalide"), "{}", app.status);
    assert!(app.pending_mission_dir.is_none(), "rien n'est créé sur disque");
}

#[test]
fn retour_du_boss_brief_ecrit_selectionne_la_mission() {
    let mut app = company_app();
    write_brief(&app.root, "0000000000001-aaaa", "# Ancienne mission\n");
    app.reload();
    let before = app.names_for(AssistantKind::Mission);
    assert_eq!(before.len(), 1);
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('m'));
    let dir = app.pending_mission_dir.clone().unwrap();
    app.assistant_request = None;
    crate::runtime::missions::write_output(&dir, "brief.md", "# Page d'accueil\n## Objectif\nx").unwrap();
    app.after_assistant(AssistantKind::Mission, &before);
    assert!(app.status.contains("Page d'accueil") && app.status.contains("r pour la lancer"), "{}", app.status);
    assert_eq!(app.missions[app.mission_sel].title, "Page d'accueil");
    assert!(app.pending_mission_dir.is_none());
}

#[test]
fn retour_du_boss_sans_brief_nettoie_le_dossier_vide() {
    let mut app = company_app();
    let before = app.names_for(AssistantKind::Mission);
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('m'));
    let dir = app.pending_mission_dir.clone().unwrap();
    app.assistant_request = None;
    app.after_assistant(AssistantKind::Mission, &before);
    assert_eq!(app.status, "aucun brief écrit : mission non créée");
    assert!(!dir.exists(), "dossier vide supprimé");
    assert!(app.missions.is_empty());
}

#[test]
fn liste_des_missions_et_navigation() {
    let mut app = company_app();
    write_brief(&app.root, "0000000000002-bbbb", "# Plus récente\n");
    write_brief(&app.root, "0000000000001-aaaa", "# Première\n");
    let dir = crate::runtime::missions::dir_of(&app.root, "0000000000001-aaaa");
    crate::runtime::missions::write_output(&dir, "conception.md", "plan").unwrap();
    crate::runtime::missions::write_output(&dir, "rapport.md", "fin").unwrap();
    app.reload();
    press(&mut app, KeyCode::Char('2'));
    let s = screen(&app);
    assert!(s.contains("Missions (2)") && s.contains("Plus récente") && s.contains("Première"), "{s}");
    assert!(s.contains("✔ rapport") && s.contains("brief"));
    assert_eq!(app.missions[0].title, "Plus récente", "la plus récente d'abord");
    press(&mut app, KeyCode::Char('t'));
    assert_eq!(app.mission_sel, 1);
    press(&mut app, KeyCode::Char('t'));
    assert_eq!(app.mission_sel, 0, "boucle");
    press(&mut app, KeyCode::Char('T'));
    assert_eq!(app.mission_sel, 1);
}

#[test]
fn liste_des_missions_vide_indique_la_touche_m() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    assert!(screen(&app).contains("m : cadrer"));
    press(&mut app, KeyCode::Char('r'));
    assert!(app.run.is_none() && app.status.contains("aucune mission"), "{}", app.status);
}

#[test]
fn r_refuse_pendant_un_run_et_sans_entreprise_valide() {
    let mut app = company_app();
    write_brief(&app.root, "0000000000001-aaaa", "# M\n");
    app.reload();
    app.run = Some(run_view(&["a"])); // en cours
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('r'));
    assert!(app.status.contains("déjà en cours"));
    assert_eq!(app.run.as_ref().unwrap().nodes.len(), 1, "le run en cours n'est pas touché");

    let mut app = company_app();
    write_brief(&app.root, "0000000000001-aaaa", "# M\n");
    std::fs::write(app.root.join(".claude/companies/acme.toml"), ACME.replace("agent = \"boss\"", "agent = \"fantome\"")).unwrap();
    app.reload();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('r'));
    assert!(app.run.is_none() && app.status.contains("invalide"));
}

// ---------- point de contrôle (interface) ----------

fn checkpoint_app() -> (App, mpsc::UnboundedReceiver<bool>) {
    let root = tmp_root();
    let mut app = new_app(&root);
    app.run = Some(run_view(&[]));
    let (reply, rx) = mpsc::unbounded_channel();
    app.on_run_event(RunEvent::Checkpoint { plan: "PLAN: créer src/a.txt\n".repeat(60), reply });
    (app, rx)
}

#[test]
fn le_point_de_controle_affiche_le_plan_et_donne_la_main() {
    let (mut app, _rx) = checkpoint_app();
    assert!(app.checkpoint.is_some());
    assert_eq!(app.tab, Tab::Run, "on bascule sur le run");
    let s = screen(&app);
    assert!(s.contains("Plan consolidé") && s.contains("PLAN: créer src/a.txt"));
    assert!(s.contains("Entrée") && s.contains("Échap"));
    // Les autres touches ne déclenchent rien pendant la validation.
    press(&mut app, KeyCode::Char('q'));
    assert!(!app.quit && app.checkpoint.is_some());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.checkpoint.as_ref().unwrap().scroll, 2);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.checkpoint.as_ref().unwrap().scroll, 1);
}

#[test]
fn entree_lance_la_realisation_echap_l_arrete() {
    let (mut app, mut rx) = checkpoint_app();
    press(&mut app, KeyCode::Enter);
    assert_eq!(rx.try_recv().ok(), Some(true));
    assert!(app.checkpoint.is_none() && app.status.contains("réalisation lancée"));

    let (mut app, mut rx) = checkpoint_app();
    press(&mut app, KeyCode::Esc);
    assert_eq!(rx.try_recv().ok(), Some(false));
    assert!(app.checkpoint.is_none() && app.status.contains("refusée"));
}

#[test]
fn e_ouvre_la_revision_du_plan_et_le_plan_modifie_est_relu() {
    let (mut app, mut rx) = checkpoint_app();
    let dir = app.root.join(".essaim/missions/42");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("conception.md"), "PLAN V2").unwrap();
    let company = crate::model::company::Company::parse("name = \"acme\"\n[[seat]]\nid = \"ceo\"\ntitle = \"CEO\"\nagent = \"a\"\n").unwrap();
    app.run_ctx = Some((company, dir));
    press(&mut app, KeyCode::Char('e'));
    let (kind, args) = app.assistant_request.take().expect("conversation demandée");
    assert_eq!(kind, AssistantKind::Plan);
    let sys = &args[args.iter().position(|x| x == "--append-system-prompt").unwrap() + 1];
    assert!(sys.contains(".essaim/missions/42/conception.md"), "chemin relatif à la racine : {sys}");
    assert!(app.checkpoint.is_some() && rx.try_recv().is_err(), "le run attend toujours la validation");
    // Retour de la conversation : le plan affiché est celui du fichier.
    app.after_assistant(AssistantKind::Plan, &[]);
    assert_eq!(app.checkpoint.as_ref().unwrap().plan, "PLAN V2");
    assert!(app.status.contains("révisé"));
    app.after_assistant(AssistantKind::Plan, &[]);
    assert!(app.status.contains("inchangé"));
}

#[test]
fn le_point_de_controle_disparait_si_le_run_se_termine() {
    let (mut app, _rx) = checkpoint_app();
    app.on_run_event(RunEvent::RunFinished { ok: false, cost_usd: 0.0 });
    assert!(app.checkpoint.is_none(), "run annulé pendant l'attente");
    press(&mut app, KeyCode::Char('q'));
    assert!(app.quit, "les touches reviennent à la normale");
}

#[test]
fn le_point_de_controle_sort_l_utilisateur_de_la_session_claude() {
    let (mut app, _rx) = term_app("sleep 30");
    let (reply, _r) = mpsc::unbounded_channel();
    app.run = Some(run_view(&[]));
    app.on_run_event(RunEvent::Checkpoint { plan: "p".into(), reply });
    assert_eq!(app.tab, Tab::Run);
    assert!(!app.term_focus, "les frappes ne partent plus dans la session pendant la validation");
}

// ---------- assistant de création d'entreprise (touche n) ----------

#[test]
fn n_sur_entreprise_demande_l_assistant_avec_agents_et_entreprises() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('n'));
    let (kind, args) = app.assistant_request.take().expect("assistant demandé");
    assert_eq!(kind, AssistantKind::Company);
    assert!(args.iter().any(|a| a == "plan"));
    let sys = args.iter().find(|a| a.contains("company-creation assistant")).unwrap();
    assert!(sys.contains("- boss: desc de boss") && sys.contains("- poete: desc de poete"));
    assert!(sys.contains("not to reuse): acme."));
}

#[test]
fn entreprise_vide_propose_n() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('2'));
    assert!(screen(&app).contains("n : un assistant"));
    press(&mut app, KeyCode::Char('n'));
    let (_, args) = app.assistant_request.take().unwrap();
    assert!(args.iter().any(|a| a.contains("no agent exists yet") && a.contains("not to reuse): none.")));
}

#[test]
fn retour_de_l_assistant_entreprise_cree_valide_invalide_ou_rien() {
    let mut app = company_app();
    let before = app.names_for(AssistantKind::Company);
    assert_eq!(before, vec!["acme"]);

    app.after_assistant(AssistantKind::Company, &before);
    assert_eq!(app.status, "aucune entreprise créée");

    // Entreprise valide + un agent créé en même temps.
    let root = app.root.clone();
    write_agent(&root, "nouvel-agent");
    std::fs::write(root.join(".claude/companies/zeta.toml"), ACME.replace("name = \"acme\"", "name = \"zeta\"")).unwrap();
    app.after_assistant(AssistantKind::Company, &before);
    assert!(app.status.starts_with("entreprise « zeta » créée : 3 postes"), "{}", app.status);
    assert!(app.status.contains("appels par mission") && app.status.contains("m pour cadrer"));
    assert_eq!(app.companies[app.comp_sel].file, "zeta.toml");
    assert!(app.agents.iter().any(|a| a.name == "nouvel-agent"), "les agents créés sont rechargés");

    // Entreprise invalide (deux boss).
    let before = app.names_for(AssistantKind::Company);
    std::fs::write(root.join(".claude/companies/zz-deux-boss.toml"), ACME.replace("reports_to = \"ceo\"\n", "")).unwrap();
    app.after_assistant(AssistantKind::Company, &before);
    assert!(app.status.contains("zz-deux-boss.toml") && app.status.contains("invalide") && app.status.contains("exactement un boss"), "{}", app.status);

    // Entreprise illisible.
    let before = app.names_for(AssistantKind::Company);
    std::fs::write(root.join(".claude/companies/zzz-casse.toml"), "= pas du toml").unwrap();
    app.after_assistant(AssistantKind::Company, &before);
    assert!(app.status.contains("illisible"));
}

#[test]
fn agent_illisible_signale_meme_sans_entreprise() {
    let mut app = company_app();
    let before = app.names_for(AssistantKind::Company);
    std::fs::write(app.root.join(".claude/agents/casse.md"), "pas de frontmatter").unwrap();
    app.after_assistant(AssistantKind::Company, &before);
    assert!(app.status.contains("casse.md") && app.status.contains("illisible"), "{}", app.status);
}

#[test]
fn formulaire_de_poste_ajout_sous_le_poste_selectionne() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down); // cto
    press(&mut app, KeyCode::Char('a'));
    assert!(matches!(app.mode, Mode::EditSeat(_)));
    let s = screen(&app);
    assert!(s.contains("Nouveau poste") && s.contains("niveau N-2"), "{s}");
    type_str(&mut app, "qa");
    press(&mut app, KeyCode::Enter);
    type_str(&mut app, "QA Engineer");
    press(&mut app, KeyCode::Enter);
    ctrl_s(&mut app);
    assert!(matches!(app.mode, Mode::Normal), "{}", app.status);
    let c = Company::load(&app.root.join(".claude/companies/acme.toml")).unwrap();
    let qa = &c.seats[c.index_of("qa").unwrap()];
    assert_eq!(qa.reports_to.as_deref(), Some("cto"));
    assert_eq!(qa.title, "QA Engineer");
    assert!(app.status.contains("enregistré"));
}

#[test]
fn formulaire_de_poste_refuse_un_id_invalide_et_garde_le_formulaire() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('a'));
    type_str(&mut app, "a b");
    press(&mut app, KeyCode::Enter);
    type_str(&mut app, "X");
    ctrl_s(&mut app);
    assert!(matches!(app.mode, Mode::EditSeat(_)));
    assert!(app.status.contains("id de poste invalide"), "{}", app.status);
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));
    let c = Company::load(&app.root.join(".claude/companies/acme.toml")).unwrap();
    assert_eq!(c.seats.len(), 3);
}

#[test]
fn formulaire_de_poste_renommer_met_a_jour_les_subordonnes() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down); // cto
    press(&mut app, KeyCode::Char('e'));
    assert!(screen(&app).contains("Éditer le poste"));
    for _ in 0..3 {
        press(&mut app, KeyCode::Backspace);
    }
    type_str(&mut app, "vp-tech");
    ctrl_s(&mut app);
    assert!(matches!(app.mode, Mode::Normal), "{}", app.status);
    let c = Company::load(&app.root.join(".claude/companies/acme.toml")).unwrap();
    assert!(c.index_of("cto").is_none());
    assert_eq!(c.seats[c.index_of("dev").unwrap()].reports_to.as_deref(), Some("vp-tech"));
    assert_eq!(c.seats[c.index_of("dev").unwrap()].allow_bash, vec!["cargo test"]);
    assert!(app.companies[0].problem.is_none());
}

#[test]
fn le_superieur_ne_peut_pas_etre_un_descendant() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down); // cto
    press(&mut app, KeyCode::Char('e'));
    let Mode::EditSeat(form) = &app.mode else { panic!() };
    let Field::Choice { options, .. } = &form.fields[4] else { panic!() };
    assert_eq!(options, &vec!["ceo".to_string()]);
}

#[test]
fn retirer_un_poste_fait_remonter_ses_subordonnes() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down); // cto
    press(&mut app, KeyCode::Char('x'));
    assert!(screen(&app).contains("Supprimer le poste"));
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(Company::load(&app.root.join(".claude/companies/acme.toml")).unwrap().seats.len(), 3);
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('y'));
    let c = Company::load(&app.root.join(".claude/companies/acme.toml")).unwrap();
    assert_eq!(c.seats.len(), 2);
    assert_eq!(c.seats[c.index_of("dev").unwrap()].reports_to.as_deref(), Some("ceo"));
}

#[test]
fn le_boss_avec_subordonnes_ne_se_retire_pas() {
    let mut app = company_app();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.status.contains("boss"), "{}", app.status);
    assert_eq!(Company::load(&app.root.join(".claude/companies/acme.toml")).unwrap().seats.len(), 3);
}

#[test]
fn ouvrir_une_ancienne_mission() {
    let root = tmp_root();
    let dir = crate::runtime::missions::create(&root, &crate::runtime::missions::new_id(1)).unwrap();
    crate::runtime::missions::write_output(&dir, "brief.md", "# Vieux chantier\nfaire X").unwrap();
    crate::runtime::missions::write_output(&dir, "rapport.md", "tout est fait").unwrap();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Enter);
    let s = screen(&app);
    assert!(s.contains("Vieux chantier") && s.contains("faire X") && s.contains("tout est fait"));
    press(&mut app, KeyCode::Esc);
    assert!(app.mission_view.is_none());
}

#[test]
fn l_aide_suit_l_onglet_courant_et_m_donne_le_manuel_complet() {
    let root = tmp_root();
    let mut app = new_app(&root);
    press(&mut app, KeyCode::Char('3'));
    app.on_key(ctrl_k());
    let s = screen(&app);
    assert!(s.contains("ONGLET 3") && s.contains("annule le run en cours") && !s.contains("ONGLET 1"), "{s}");
    press(&mut app, KeyCode::Char('m'));
    let s = screen(&app);
    assert!(s.contains("man essaim") && s.contains("SYNOPSIS"), "{s}");
}

fn settings_app(dir: &std::path::Path) -> App {
    let mut app = new_app(&tmp_root());
    app.load_settings(Some(dir.join("settings.toml")));
    app
}

#[test]
fn premier_lancement_choix_de_langue_avant_le_menu_et_memorise() {
    let dir = tmp_root();
    let mut app = settings_app(&dir);
    let s = screen(&app);
    assert!(s.contains("Choose your language") && !s.contains("Entreprise"), "{s}");
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(app.settings_menu.is_none());
    assert_eq!(app.lang, crate::tui::settings::Lang::En);
    let s = screen(&app);
    assert!(s.contains("Company") && s.contains("F10 settings"), "{s}");
    // relancé : la langue est relue, pas de menu
    let again = settings_app(&dir);
    assert!(again.settings_menu.is_none());
    assert_eq!(again.lang, crate::tui::settings::Lang::En);
}

#[test]
fn f10_rouvre_les_reglages_et_echap_annule() {
    let dir = tmp_root();
    let mut app = settings_app(&dir);
    press(&mut app, KeyCode::Enter); // français
    press(&mut app, KeyCode::F(10));
    assert!(app.settings_menu.is_some());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Esc);
    assert!(app.settings_menu.is_none());
    assert_eq!(app.lang, crate::tui::settings::Lang::Fr);
    press(&mut app, KeyCode::F(10));
    press(&mut app, KeyCode::Char('2'));
    assert_eq!(app.lang, crate::tui::settings::Lang::En);
    app.on_key(ctrl_k());
    assert!(screen(&app).contains("TAB 1: AGENTS"));
}

#[test]
fn interface_en_anglais_apres_le_choix_de_langue() {
    let dir = tmp_root();
    let mut app = settings_app(&dir);
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('2')); // onglet Company
    let s = screen(&app);
    assert!(s.contains("No company in .claude/companies/") && s.contains("Missions (0)"), "{s}");
    assert!(!s.contains("Aucune entreprise"), "{s}");
    app.on_key(ctrl_k());
    press(&mut app, KeyCode::Char('m'));
    let s = screen(&app);
    assert!(s.contains("man essaim") && s.contains("NAME") && s.contains("SYNOPSIS"), "{s}");
}

// ---------- terminal intégré (aides) ----------

use super::term::TermSession;
use std::time::{Duration, Instant};

fn term_app(script: &str) -> (App, mpsc::UnboundedReceiver<AppEvent>) {
    let root = tmp_root();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(root.clone(), tx.clone());
    let mut t = TermSession::spawn("sh", &["-c".into(), script.into()], &root, (20, 80), 7, tx).unwrap();
    t.origin = Tab::Agents;
    app.term = Some(t);
    app.term_focus = true;
    app.tab = Tab::Terminal;
    // vide les premiers événements pour partir d'un état stable
    while rx.try_recv().is_ok() {}
    (app, rx)
}

fn pump_until(app: &mut App, rx: &mut mpsc::UnboundedReceiver<AppEvent>, what: &str, mut cond: impl FnMut(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        while let Ok(ev) = rx.try_recv() {
            app.on_event(ev);
        }
        if cond(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("délai dépassé : {what}\n{}", screen(app));
}

fn ctrl_s(app: &mut App) {
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
}
