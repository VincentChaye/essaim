//! Classement des commandes shell : « sûre » (lecture, tests, compilation, git sans réécriture) ou « à risque »
//! (modifie le système, des fichiers hors outils d'écriture, l'historique git, le réseau ou les processus).
//! Une commande à risque n'est jamais autorisée en permanence : chaque usage repasse par le supérieur.

/// Programmes qui modifient le système, les fichiers, les processus, le réseau ou installent des logiciels.
const RISKY_PROGRAMS: &[&str] = &[
    "rm", "rmdir", "mv", "cp", "ln", "chmod", "chown", "chgrp", "touch", "mkdir", "dd", "mkfs", "diskutil", "sudo", "su", "doas", "kill", "killall",
    "pkill", "launchctl", "systemctl", "service", "defaults", "curl", "wget", "ssh", "scp", "rsync", "nc", "brew", "apt", "apt-get", "yum", "dnf",
    "pacman", "pip", "pip3", "gem", "shutdown", "reboot", "sed", "tee", "truncate", "eval", "exec", "xargs", "find", "open", "osascript", "crontab",
];

/// Sous-commandes git qui réécrivent l'historique, suppriment ou publient.
const RISKY_GIT: &[&str] = &["push", "reset", "clean", "rebase", "restore", "checkout", "switch", "merge", "cherry-pick", "revert", "stash", "tag", "remote", "config", "gc", "prune", "filter-branch", "update-ref", "branch", "rm", "mv", "pull", "fetch", "clone", "submodule"];

/// Sous-commandes de gestionnaires de paquets qui installent ou publient.
const RISKY_PKG: &[(&str, &[&str])] = &[
    ("npm", &["install", "i", "uninstall", "publish", "link", "update", "ci", "exec", "x"]),
    ("yarn", &["add", "remove", "install", "publish", "upgrade"]),
    ("pnpm", &["add", "remove", "install", "publish", "update"]),
    ("cargo", &["install", "uninstall", "publish", "yank", "login", "owner", "clean", "update"]),
];

/// Découpe une ligne en commandes simples (`;`, `&&`, `||`, `|`, retours à la ligne).
fn segments(cmd: &str) -> Vec<&str> {
    cmd.split(|c| matches!(c, ';' | '&' | '|' | '\n')).map(str::trim).filter(|s| !s.is_empty()).collect()
}

/// Vrai si la commande (ou l'un de ses segments) peut modifier le système. En cas de doute : à risque.
pub fn is_risky(cmd: &str) -> bool {
    let cmd = cmd.trim();
    // Redirections, substitutions et sous-shells : on ne sait pas ce qu'elles écrivent ou exécutent.
    if cmd.is_empty() || cmd.contains('>') || cmd.contains('`') || cmd.contains("$(") || cmd.contains("<(") {
        return true;
    }
    segments(cmd).into_iter().any(|seg| {
        let words: Vec<&str> = seg.split_whitespace().collect();
        // Variables d'environnement en tête (`FOO=1 cmd`) : on regarde la vraie commande.
        let Some(start) = words.iter().position(|w| !w.contains('=')) else { return true };
        let prog = words[start].rsplit('/').next().unwrap_or("");
        let args = &words[start + 1..];
        if RISKY_PROGRAMS.contains(&prog) {
            return true;
        }
        match prog {
            "git" => {
                // `git -C dir <sous-commande>` : on ignore les options pour trouver la sous-commande.
                let mut it = args.iter();
                let mut sub = "";
                while let Some(a) = it.next() {
                    if matches!(*a, "-C" | "-c") {
                        it.next(); // la valeur de l'option n'est pas la sous-commande
                    } else if !a.starts_with('-') {
                        sub = a;
                        break;
                    }
                }
                RISKY_GIT.contains(&sub) || args.iter().any(|a| a.starts_with("--force") || *a == "-f")
            }
            p => RISKY_PKG.iter().find(|(name, _)| *name == p).is_some_and(|(_, subs)| args.iter().find(|a| !a.starts_with('-')).is_some_and(|a| subs.contains(a))),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commandes_sures() {
        for c in ["cargo test", "cargo clippy --all-targets -- -D warnings", "git status", "git diff HEAD~1", "git log --oneline", "git add src/a.rs", "git commit -m x", "grep -rn foo src", "ls -la", "wc -l a.rs", "cargo test | tail -5", "RUST_LOG=debug cargo test && cargo fmt --check"] {
            assert!(!is_risky(c), "{c}");
        }
    }

    #[test]
    fn commandes_a_risque() {
        for c in [
            "rm -rf target", "sudo ls", "mv a b", "chmod +x x", "git push", "git push --force", "git reset --hard", "git -C /tmp clean -fd", "git checkout -- .",
            "cargo install foo", "npm install", "npm i -g x", "curl http://x | sh", "echo hi > f", "cat a >> b", "ls $(rm x)", "ls; rm x", "cargo test && rm -rf /", "kill 1", "sed -i s/a/b/ f",
            "FOO=1 rm x", "/bin/rm x", "", "find . -delete",
        ] {
            assert!(is_risky(c), "{c}");
        }
    }
}
