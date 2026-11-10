//! Pause d'un run : atteindre la limite de l'abonnement met les appels en attente au lieu de les faire échouer.
//!
//! Un seul objet (`Gate`) est partagé par l'orchestrateur et par l'interface : l'orchestrateur attend devant
//! la porte avant chaque appel `claude`, l'interface lit l'état pour l'afficher et peut ouvrir ou fermer la
//! porte à la main. Une pause due à la limite se lève d'elle-même à la réinitialisation annoncée.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::watch;

/// Pourquoi le run est en pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Demandée par l'utilisateur : elle ne se lève qu'à la main.
    User,
    /// Limite de l'abonnement atteinte : reprise automatique à la réinitialisation.
    Limit,
}

/// État de pause d'un run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub why: Why,
    /// Réinitialisation annoncée par `claude` (epoch secondes) : instant de la reprise automatique.
    pub resets_at: Option<u64>,
}

/// Attente avant de retenter quand la limite est atteinte sans que la réinitialisation soit annoncée.
pub const BLIND_WAIT: Duration = Duration::from_secs(10 * 60);

/// Marge ajoutée à la réinitialisation annoncée : les horloges ne sont jamais exactement d'accord.
/// Courte sans risque : un essai trop tôt bute à nouveau sur la limite et remet le run en pause.
pub const RESET_MARGIN: Duration = Duration::from_secs(5);

pub fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Heure locale de la reprise automatique et temps restant (« 14:20 (dans 2h05) »), pour l'affichage.
pub fn reset_label(resets_at: Option<u64>, fr: bool) -> String {
    let Some(t) = resets_at else {
        return if fr { "heure non annoncée".into() } else { "time not announced".into() };
    };
    use chrono::TimeZone;
    let heure = chrono::Local.timestamp_opt(t as i64, 0).single().map(|d| d.format("%H:%M").to_string()).unwrap_or_else(|| "?".into());
    let left = t.saturating_sub(now_s());
    let reste = format!("{}h{:02}", left / 3600, left % 3600 / 60);
    if fr { format!("{heure} (dans {reste})") } else { format!("{heure} (in {reste})") }
}

/// Porte partagée : fermée = run en pause.
#[derive(Debug)]
pub struct Gate {
    tx: watch::Sender<Option<State>>,
    /// Une pause vient de se terminer et la reprise n'a pas encore été annoncée.
    resumed: AtomicBool,
}

impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { tx: watch::channel(None).0, resumed: AtomicBool::new(false) })
    }

    /// Porte déjà fermée au départ (reprise d'une mission mise en pause par la limite, dont la fenêtre
    /// n'est pas encore réinitialisée) : aucun appel ne part avant l'heure.
    pub fn closed(why: Why, resets_at: Option<u64>) -> Arc<Self> {
        Arc::new(Self { tx: watch::channel(Some(State { why, resets_at })).0, resumed: AtomicBool::new(false) })
    }

    pub fn state(&self) -> Option<State> {
        *self.tx.borrow()
    }

    /// Récepteur du signal de pause, pour un appel **déjà lancé** : `run_local` l'écoute afin de
    /// demander l'arrêt à l'agent en vol, là où `wait()` ne sert qu'à retenir les appels à venir.
    pub fn subscribe(&self) -> watch::Receiver<Option<State>> {
        self.tx.subscribe()
    }

    pub fn paused(&self) -> bool {
        self.state().is_some()
    }

    /// Met le run en pause. Vrai si c'est le début d'une pause (la porte était ouverte) : à n'annoncer qu'une fois.
    /// Une pause demandée par l'utilisateur prime sur une pause de limite ; deux limites gardent la
    /// réinitialisation la plus tardive (ne jamais raccourcir une attente connue).
    pub fn pause(&self, why: Why, resets_at: Option<u64>) -> bool {
        let mut was_open = false;
        // `send_modify` : lecture et écriture sous le même verrou (deux postes peuvent buter sur la limite
        // en même temps) et notification même sans récepteur vivant.
        self.tx.send_modify(|cur| {
            was_open = cur.is_none();
            *cur = Some(match (*cur, why) {
                (_, Why::User) => State { why: Why::User, resets_at: None },
                (Some(s @ State { why: Why::User, .. }), Why::Limit) => s,
                (Some(s), Why::Limit) => State { why: Why::Limit, resets_at: resets_at.max(s.resets_at) },
                (None, Why::Limit) => State { why: Why::Limit, resets_at },
            });
        });
        was_open
    }

    /// Reprend le run. Vrai s'il était en pause (la reprise reste à annoncer, voir `take_resumed`).
    pub fn resume(&self) -> bool {
        let mut was_paused = false;
        self.tx.send_if_modified(|cur| {
            was_paused = cur.is_some();
            *cur = None;
            was_paused
        });
        if was_paused {
            self.resumed.store(true, Ordering::SeqCst);
        }
        was_paused
    }

    /// Vrai une seule fois par pause terminée : permet d'annoncer la reprise sans la répéter autant de
    /// fois qu'il y avait d'appels en attente.
    pub fn take_resumed(&self) -> bool {
        self.resumed.swap(false, Ordering::SeqCst)
    }

    /// Attend que la porte s'ouvre. Rend la main tout de suite si le run n'est pas en pause.
    /// Une pause de limite se lève seule : à la réinitialisation annoncée, sinon après `BLIND_WAIT`.
    pub async fn wait(&self) {
        let mut rx = self.tx.subscribe();
        loop {
            let Some(st) = *rx.borrow_and_update() else { return };
            let delay = match st.why {
                Why::User => None,
                Why::Limit => Some(match st.resets_at {
                    Some(t) => Duration::from_secs(t.saturating_sub(now_s())) + RESET_MARGIN,
                    None => BLIND_WAIT,
                }),
            };
            match delay {
                Some(d) => {
                    tokio::select! {
                        _ = tokio::time::sleep(d) => {
                            // Ne lève que la pause attendue : si l'utilisateur en a demandé une entre-temps,
                            // elle reste en place (on repasse par la boucle pour l'attendre).
                            if self.state() == Some(st) {
                                self.resume();
                                return;
                            }
                        }
                        changed = rx.changed() => if changed.is_err() { return },
                    }
                }
                None => {
                    if rx.changed().await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn porte_ouverte_laisse_passer() {
        let g = Gate::new();
        assert!(!g.paused());
        tokio::time::timeout(Duration::from_millis(50), g.wait()).await.expect("ne doit pas attendre");
        assert!(!g.take_resumed());
    }

    #[tokio::test]
    async fn pause_utilisateur_attend_la_reprise() {
        let g = Gate::new();
        assert!(g.pause(Why::User, None), "première pause");
        assert!(!g.pause(Why::User, None), "déjà en pause");
        assert!(tokio::time::timeout(Duration::from_millis(80), g.wait()).await.is_err(), "doit attendre la main");
        let g2 = g.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            g2.resume();
        });
        tokio::time::timeout(Duration::from_secs(2), g.wait()).await.expect("reprise");
        assert!(!g.paused());
        assert!(g.take_resumed() && !g.take_resumed(), "reprise annoncée une seule fois");
    }

    #[tokio::test(start_paused = true)]
    async fn pause_de_limite_se_leve_a_la_reinitialisation() {
        let g = Gate::new();
        g.pause(Why::Limit, Some(now_s() + 3600));
        g.wait().await; // horloge virtuelle : avance jusqu'à la réinitialisation
        assert!(!g.paused() && g.take_resumed());
    }

    #[tokio::test(start_paused = true)]
    async fn limite_sans_heure_connue_retente_a_l_aveugle() {
        let g = Gate::new();
        g.pause(Why::Limit, None);
        g.wait().await;
        assert!(!g.paused());
    }

    #[test]
    fn la_pause_utilisateur_prime_et_la_limite_garde_la_plus_longue_attente() {
        let g = Gate::new();
        g.pause(Why::Limit, Some(100));
        g.pause(Why::Limit, Some(500));
        assert_eq!(g.state().unwrap().resets_at, Some(500));
        g.pause(Why::Limit, None);
        assert_eq!(g.state().unwrap().resets_at, Some(500), "une limite sans heure ne raccourcit pas l'attente");
        g.pause(Why::User, None);
        assert_eq!(g.state(), Some(State { why: Why::User, resets_at: None }));
        g.pause(Why::Limit, Some(900));
        assert_eq!(g.state().unwrap().why, Why::User, "la pause de l'utilisateur reste la main");
    }

    /// Une pause demandée par l'utilisateur juste avant l'heure de réinitialisation n'est pas levée par la
    /// reprise automatique : elle attend la main.
    #[tokio::test(start_paused = true)]
    async fn la_reprise_automatique_ne_leve_pas_une_pause_de_l_utilisateur() {
        let g = Gate::new();
        g.pause(Why::Limit, Some(now_s() + 60));
        let g2 = g.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            g2.pause(Why::User, None);
        });
        assert!(tokio::time::timeout(Duration::from_secs(600), g.wait()).await.is_err(), "la pause de l'utilisateur tient");
        assert_eq!(g.state().map(|s| s.why), Some(Why::User));
    }

    #[tokio::test]
    async fn porte_fermee_des_le_depart() {
        let g = Gate::closed(Why::Limit, Some(now_s() + 3600));
        assert!(g.paused());
        assert!(tokio::time::timeout(Duration::from_millis(50), g.wait()).await.is_err());
    }
}
