//! Helpers de texte communs (F5 : blancs ; F6 : troncature). HYPOTHÈSE : contrat dérivé des groupes de duplication mesurés.
//!
//! Ajout seulement : aucun site d'appel existant n'est modifié ici (adoption à l'étape 4).

/// F5 : vrai si `s` est vide ou ne contient que des espaces.
/// Équivalent à `s.trim().is_empty()`.
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// F6 : les `max` premiers caractères (et non les octets).
/// Équivalent à `s.chars().take(max).collect::<String>()`.
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn take_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// F6 : tronque à `max` caractères et ajoute `marker` seulement si la coupe a eu lieu.
/// Ex. `marker = "\n[… truncated]"` pour les sites qui gardent le texte entier quand il est court.
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn truncate_marked(s: &str, max: usize, marker: &str) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}{marker}", take_chars(s, max)) }
}

/// F6 : tronque à `width` caractères, `…` final compris, si la coupe a eu lieu.
/// Même règle que `company_view` : `width - 1` caractères puis `…`.
#[allow(dead_code)] // retiré à l'adoption (étape 4)
pub fn truncate_ellipsis(s: &str, width: usize) -> String {
    if s.chars().count() <= width { s.to_string() } else { format!("{}…", take_chars(s, width.saturating_sub(1))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_blank_vide_et_espaces() {
        assert!(is_blank(""));
        assert!(is_blank(" \n\t "));
        assert!(!is_blank(" a "));
    }

    #[test]
    fn take_chars_compte_les_caracteres() {
        assert_eq!(take_chars("héllo", 2), "hé");
        assert_eq!(take_chars("abc", 0), "");
        assert_eq!(take_chars("abc", 10), "abc");
    }

    #[test]
    fn truncate_marked_ajoute_marqueur_seulement_si_coupe() {
        assert_eq!(truncate_marked("abcdef", 3, "…"), "abc…");
        assert_eq!(truncate_marked("abc", 3, "…"), "abc");
        assert_eq!(truncate_marked("éèà", 2, "!"), "éè!");
    }

    #[test]
    fn truncate_ellipsis_respecte_la_largeur() {
        assert_eq!(truncate_ellipsis("abcdef", 4), "abc…");
        assert_eq!(truncate_ellipsis("abc", 4), "abc");
        assert_eq!(truncate_ellipsis("ééééé", 3), "éé…");
        assert_eq!(truncate_ellipsis("abc", 1), "…");
    }
}
