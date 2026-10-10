//! Helpers de widgets ratatui communs (F7). HYPOTHÈSE : contrat dérivé des groupes de duplication mesurés.
//!
//! Ajout seulement : aucun site d'appel existant n'est modifié ici (adoption à l'étape 4).

use ratatui::{
    style::{Color, Style},
    widgets::Block,
};

/// F7 : cadre bordé à titre, bordure par défaut.
/// Équivalent à `Block::bordered().title(t)`, utilisé dans `ui.rs` et `company_view.rs`.
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn panel(title: &str) -> Block<'static> {
    Block::bordered().title(title.to_string())
}

/// F7 : cadre bordé à titre, bordure colorée.
/// Équivalent à `panel(t).border_style(Style::new().fg(color))`.
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn panel_colored(title: &str, color: Color) -> Block<'static> {
    panel(title).border_style(Style::new().fg(color))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    fn rendu(bloc: Block<'static>) -> Buffer {
        let zone = Rect::new(0, 0, 12, 3);
        let mut buf = Buffer::empty(zone);
        bloc.render(zone, &mut buf);
        buf
    }

    #[test]
    fn panel_dessine_bordure_et_titre() {
        let buf = rendu(panel("Ti"));
        assert_eq!(buf[(0u16, 0u16)].symbol(), "┌");
        let ligne: String = (0..12u16).map(|x| buf[(x, 0u16)].symbol()).collect();
        assert!(ligne.contains("Ti"), "titre absent : {ligne:?}");
    }

    #[test]
    fn panel_colored_applique_la_couleur_de_bordure() {
        let buf = rendu(panel_colored("T", Color::Cyan));
        assert_eq!(buf[(0u16, 0u16)].fg, Color::Cyan);
    }
}
