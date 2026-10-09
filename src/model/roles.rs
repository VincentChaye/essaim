//! Niveaux hiérarchiques (N, N-1, N-2…) et intitulés de poste usuels pour chacun.

/// `N` pour la racine, puis `N-1`, `N-2`…
pub fn level_label(depth: usize) -> String {
    if depth == 0 { "N".into() } else { format!("N-{depth}") }
}

/// Intitulés suggérés selon la profondeur et le fait que le poste ait ou non des subordonnés.
pub fn title_suggestions(depth: usize, is_leaf: bool) -> &'static [&'static str] {
    if depth == 0 {
        return &["CEO", "Président", "Directeur général", "Fondateur"];
    }
    if is_leaf {
        return &[
            "Senior Engineer",
            "Backend Engineer",
            "Frontend Engineer",
            "DevOps Engineer",
            "QA Engineer",
            "Security Analyst",
            "UX Designer",
            "Data Analyst",
            "Technical Writer",
        ];
    }
    match depth {
        1 => &["CTO", "CISO", "CFO", "COO", "CPO", "VP Engineering", "VP Design", "VP Security", "VP Product", "VP Operations"],
        2 => &["Director of Engineering", "Head of Security", "Design Director", "Head of Product", "Head of Data"],
        _ => &["Engineering Manager", "Team Lead", "Product Manager", "Design Manager", "Security Manager", "QA Manager"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn niveaux() {
        assert_eq!(level_label(0), "N");
        assert_eq!(level_label(3), "N-3");
    }

    #[test]
    fn suggestions_par_niveau() {
        assert!(title_suggestions(0, false).contains(&"CEO"));
        assert!(title_suggestions(1, false).contains(&"CTO"));
        assert!(title_suggestions(3, false).contains(&"Engineering Manager"));
        assert!(title_suggestions(4, true).contains(&"Backend Engineer"));
        assert!(title_suggestions(2, false).contains(&"Head of Security"));
    }
}
