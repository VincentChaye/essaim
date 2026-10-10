//! Helpers de mise en page communs (F8). HYPOTHÈSE : contrat dérivé des groupes de duplication mesurés
//! (découpage horizontal répété dans `ui.rs` et `company_view.rs`). Le « lookup.rs » du rapport n'est pas dans le périmètre.
//!
//! Ajout seulement : aucun site d'appel existant n'est modifié ici (adoption à l'étape 4).

use ratatui::layout::{Constraint, Layout, Rect};

/// F8 : découpe `zone` en deux colonnes, `left_pct` % (borné à 100) puis le reste.
/// Équivalent à `Layout::horizontal([Percentage(l), Percentage(100 - l)]).areas(zone)`.
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn split_h(zone: Rect, left_pct: u16) -> [Rect; 2] {
    let left = left_pct.min(100);
    Layout::horizontal([Constraint::Percentage(left), Constraint::Percentage(100 - left)]).areas(zone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_h_repartit_selon_le_pourcentage() {
        let [gauche, droite] = split_h(Rect::new(0, 0, 100, 10), 35);
        assert_eq!((gauche.x, gauche.width), (0, 35));
        assert_eq!((droite.x, droite.width), (35, 65));
        assert_eq!(gauche.height, 10);
    }

    #[test]
    fn split_h_borne_le_pourcentage_a_100() {
        let [gauche, droite] = split_h(Rect::new(0, 0, 80, 4), 150);
        assert_eq!(gauche.width, 80);
        assert_eq!(droite.width, 0);
    }
}
