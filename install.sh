#!/usr/bin/env bash
# Installe essaim comme commande système (binaire + page de manuel), sur macOS et Linux.
#
# Depuis n'importe où (télécharge le binaire précompilé de la dernière release) :
#   curl -fsSL --proto '=https' --tlsv1.2 https://raw.githubusercontent.com/VincentChaye/essaim/main/install.sh | bash
#
# Depuis un clone du dépôt (compile avec cargo) :
#   ./install.sh
#
# Options :
#   --prefix <dossier>   destination (défaut : ~/.local ; /usr/local demande sudo si besoin)
#   --version <vX.Y.Z>   version à télécharger (défaut : la dernière)
#   --from-source        force la compilation locale (nécessite Rust)
#   --download           force le téléchargement même depuis un clone
#   --no-modify-path     ne touche pas au fichier de config du shell
#   --uninstall          retire ce que ce script a installé
#
# Variable : ESSAIM_REPO=<owner>/<repo> pour télécharger depuis un autre dépôt GitHub.

set -euo pipefail

REPO="${ESSAIM_REPO:-VincentChaye/essaim}"
PREFIX="$HOME/.local"
VERSION="latest"
MODE="auto"
MODIFY_PATH=1
UNINSTALL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix attend un dossier}"; shift 2 ;;
    --version) VERSION="${2:?--version attend un tag}"; shift 2 ;;
    --from-source) MODE="source"; shift ;;
    --download) MODE="download"; shift ;;
    --no-modify-path) MODIFY_PATH=0; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]:-$0}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) printf 'option inconnue : %s\n' "$1" >&2; exit 2 ;;
  esac
done

BIN_DIR="$PREFIX/bin"
MAN_DIR="$PREFIX/share/man/man1"

die() { printf '\033[31m✖ %s\033[0m\n' "$*" >&2; exit 1; }

# Exécute avec sudo seulement si la destination n'est pas modifiable par l'utilisateur.
run() {
  local target="$1"; shift
  local probe="$target"
  while [ ! -e "$probe" ]; do probe="$(dirname "$probe")"; done
  if [ -w "$probe" ]; then "$@"; else sudo "$@"; fi
}

if [ "$UNINSTALL" = 1 ]; then
  run "$BIN_DIR" rm -f "$BIN_DIR/essaim"
  run "$MAN_DIR" rm -f "$MAN_DIR/essaim.1"
  echo "essaim retiré de $PREFIX (les lignes PATH ajoutées à votre shell sont conservées)"
  exit 0
fi

# --- Source locale ? ---------------------------------------------------------
ROOT=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
  cand="$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  [ -f "$cand/Cargo.toml" ] && ROOT="$cand"
fi
if [ "$MODE" = "auto" ]; then
  if [ -n "$ROOT" ]; then MODE="source"; else MODE="download"; fi
fi
[ "$MODE" = "source" ] && [ -z "$ROOT" ] && die "--from-source doit être lancé depuis un clone du dépôt"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

if [ "$MODE" = "source" ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  fi
  command -v cargo >/dev/null 2>&1 || die "cargo introuvable. Installez Rust (https://rustup.rs) ou utilisez --download"
  echo "compilation (cargo build --release)…"
  (cd "$ROOT" && cargo build --release --quiet)
  cp "$ROOT/target/release/essaim" "$STAGE/essaim"
  cp "$ROOT/man/essaim.1" "$STAGE/essaim.1"
else
  case "$(uname -s)" in
    Darwin) os="apple-darwin" ;;
    Linux)  os="unknown-linux-gnu" ;;
    *) die "système non supporté : $(uname -s) (essaim est Unix uniquement ; sous Windows, utilisez WSL)" ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *) die "architecture non supportée : $(uname -m)" ;;
  esac
  target="$arch-$os"
  asset="essaim-$target.tar.gz"
  if [ "$VERSION" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
  else
    base="https://github.com/$REPO/releases/download/$VERSION"
  fi
  command -v curl >/dev/null 2>&1 || die "curl est requis"
  echo "téléchargement de $asset ($VERSION)…"
  # HTTPS uniquement (pas de repli HTTP par redirection), TLS 1.2 minimum.
  curl -fsSL --proto '=https' --tlsv1.2 "$base/$asset" -o "$STAGE/$asset" || die "téléchargement impossible : $base/$asset"
  if curl -fsSL --proto '=https' --tlsv1.2 "$base/$asset.sha256" -o "$STAGE/$asset.sha256" 2>/dev/null; then
    expected="$(awk '{print $1}' "$STAGE/$asset.sha256")"
    if command -v sha256sum >/dev/null 2>&1; then actual="$(sha256sum "$STAGE/$asset" | awk '{print $1}')"
    else actual="$(shasum -a 256 "$STAGE/$asset" | awk '{print $1}')"; fi
    [ "$expected" = "$actual" ] || die "somme de contrôle invalide pour $asset"
  else
    die "somme de contrôle introuvable ($asset.sha256) : installation annulée"
  fi
  tar -xzf "$STAGE/$asset" -C "$STAGE"
  [ -f "$STAGE/essaim" ] || die "archive inattendue : binaire essaim absent"
fi

run "$BIN_DIR" mkdir -p "$BIN_DIR"
run "$BIN_DIR" install -m 755 "$STAGE/essaim" "$BIN_DIR/essaim"
echo "installé : $BIN_DIR/essaim"
if [ -f "$STAGE/essaim.1" ]; then
  run "$MAN_DIR" mkdir -p "$MAN_DIR"
  run "$MAN_DIR" install -m 644 "$STAGE/essaim.1" "$MAN_DIR/essaim.1"
  echo "installé : $MAN_DIR/essaim.1"
fi

# --- PATH : ajouté automatiquement au shell de l'utilisateur ----------------------
add_line() { # fichier, ligne
  local f="$1" line="$2"
  mkdir -p "$(dirname "$f")"; touch "$f"
  grep -qsF "$line" "$f" || printf '\n# essaim\n%s\n' "$line" >> "$f"
  echo "PATH mis à jour dans $f"
}
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    if [ "$MODIFY_PATH" = 1 ]; then
      case "$(basename "${SHELL:-}")" in
        zsh)  add_line "${ZDOTDIR:-$HOME}/.zshrc" "export PATH=\"$BIN_DIR:\$PATH\"" ;;
        bash) if [ "$(uname -s)" = Darwin ]; then rc="$HOME/.bash_profile"; else rc="$HOME/.bashrc"; fi
              add_line "$rc" "export PATH=\"$BIN_DIR:\$PATH\"" ;;
        fish) add_line "$HOME/.config/fish/config.fish" "fish_add_path \"$BIN_DIR\"" ;;
        *)    add_line "$HOME/.profile" "export PATH=\"$BIN_DIR:\$PATH\"" ;;
      esac
      echo "→ ouvrez un nouveau terminal (ou 'exec \$SHELL') pour utiliser la commande."
    else
      printf '⚠ %s n'"'"'est pas dans votre PATH : export PATH="%s:$PATH"\n' "$BIN_DIR" "$BIN_DIR"
    fi ;;
esac
if [ -f "$STAGE/essaim.1" ] && ! man -w essaim >/dev/null 2>&1 && [ "$PREFIX" != "/usr/local" ] && [ "$PREFIX" != "/usr" ]; then
  [ "$MODIFY_PATH" = 1 ] && add_line "${ZDOTDIR:-$HOME}/.zshrc" "export MANPATH=\"$PREFIX/share/man:\${MANPATH:-}\"" >/dev/null 2>&1 || true
fi
command -v claude >/dev/null 2>&1 || echo "⚠ Claude Code ('claude') n'est pas installé : essaim en a besoin (https://claude.com/claude-code)."
echo "Lancez : essaim   ·   Manuel : man essaim"
