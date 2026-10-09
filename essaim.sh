#!/usr/bin/env bash
# Lance essaim : prépare l'environnement, compile si besoin, puis démarre.
#
# Usage :
#   essaim.sh                      TUI dans le dossier courant
#   essaim.sh ~/mon-projet         TUI dans ce dossier
#   essaim.sh ~/mon-projet init    crée agents + entreprise d'exemple
#   essaim.sh . agents             n'importe quelle commande essaim
#   essaim.sh --rebuild            force la recompilation
#
# Astuce : ln -s "$PWD/essaim.sh" /usr/local/bin/essaim-go   (lancement depuis partout)

set -euo pipefail

# --- Dossier du script, même appelé via un lien symbolique -------------------
src="${BASH_SOURCE[0]}"
while [ -L "$src" ]; do
  dir="$(cd -P "$(dirname "$src")" && pwd)"
  src="$(readlink "$src")"
  [[ "$src" != /* ]] && src="$dir/$src"
done
ROOT="$(cd -P "$(dirname "$src")" && pwd)"
BIN="$ROOT/target/release/essaim"

die() { printf '\033[31m✖ %s\033[0m\n' "$*" >&2; exit 1; }
info() { printf '\033[2m%s\033[0m\n' "$*" >&2; }
warn() { printf '\033[33m⚠ %s\033[0m\n' "$*" >&2; }

# --- Options -----------------------------------------------------------------
REBUILD=0
if [ "${1:-}" = "--rebuild" ]; then REBUILD=1; shift; fi

# --- Dossier de travail (1er argument s'il s'agit d'un dossier) ---------------
WORKDIR="$PWD"
if [ $# -gt 0 ] && [ -d "$1" ]; then
  WORKDIR="$(cd "$1" && pwd)"
  shift
fi

# --- Rust --------------------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
  # shellcheck disable=SC1091
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
fi
command -v cargo >/dev/null 2>&1 || die "cargo introuvable. Installez Rust : https://rustup.rs"

# --- Claude Code / abonnement -------------------------------------------------
command -v claude >/dev/null 2>&1 || die "la commande 'claude' est introuvable (Claude Code n'est pas installé ou pas dans le PATH)"
# Même environnement épuré qu'essaim : une clé API ne doit pas masquer l'état de l'abonnement.
AUTH="$(env -u ANTHROPIC_API_KEY -u ANTHROPIC_AUTH_TOKEN -u CLAUDE_CODE_USE_BEDROCK \
  -u CLAUDE_CODE_USE_VERTEX -u CLAUDE_CODE_USE_FOUNDRY claude auth status 2>/dev/null || true)"
grep -q '"loggedIn": true' <<<"$AUTH" || die "Claude Code n'est pas connecté. Lancez 'claude' puis '/login'."
grep -q '"authMethod": "claude.ai"' <<<"$AUTH" || warn "la connexion n'est pas celle de l'abonnement (claude.ai) : vérifiez 'claude auth status'"
for v in ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN; do
  if [ -n "${!v:-}" ]; then
    warn "$v est défini : essaim l'ignore (il ne consomme que l'abonnement)"
  fi
done

# --- Compilation si nécessaire --------------------------------------------------
needs_build=$REBUILD
if [ ! -x "$BIN" ]; then
  needs_build=1
elif [ -n "$(find "$ROOT/src" "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" -newer "$BIN" -print -quit 2>/dev/null)" ]; then
  needs_build=1
fi
if [ "$needs_build" = 1 ]; then
  info "compilation (cargo build --release)…"
  LOG="$(mktemp)"
  if ! (cd "$ROOT" && cargo build --release >"$LOG" 2>&1); then
    cat "$LOG" >&2
    rm -f "$LOG"
    die "la compilation a échoué"
  fi
  rm -f "$LOG"
fi

# --- Lancement ------------------------------------------------------------------
cd "$WORKDIR"
if [ $# -eq 0 ] && [ ! -d .claude/agents ]; then
  info "projet vide ($WORKDIR) : lancez '$(basename "$0") $WORKDIR init' pour créer des exemples."
fi
exec "$BIN" "$@"
