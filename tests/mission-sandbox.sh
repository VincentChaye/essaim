#!/usr/bin/env bash
# Lance une vraie mission (abonnement Claude, modèle haiku) dans .essaim-sandbox/, un projet jetable
# local au dépôt avec son propre .claude/. Usage : tests/mission-sandbox.sh [--keep]
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="$PWD/target/release/essaim"
[ -x "$BIN" ] || { echo "compilez d'abord : cargo build --release" >&2; exit 1; }
SB="$PWD/.essaim-sandbox"
[ "${1:-}" = "--keep" ] || rm -rf "$SB"
mkdir -p "$SB" && cd "$SB"
"$BIN" init >/dev/null
sed -i '' 's/^model:.*/model: haiku/' .claude/agents/*.md
cat > .claude/companies/mini.toml <<'T'
name = "mini"
max_calls = 12
max_parallel = 2
checkpoint = false

[[seat]]
id = "boss"
title = "CEO"
agent = "boss"

[[seat]]
id = "tech"
title = "CTO"
agent = "vp-tech"
reports_to = "boss"
expertise = "code et architecture"

[[seat]]
id = "dev"
title = "Backend Engineer"
agent = "developer"
reports_to = "tech"
builds = true
allow_bash = ["python3 hello.py"]
T
echo 'Crée un fichier hello.py à la racine qui affiche "Bonjour essaim", puis exécute-le pour vérifier. Aucune autre modification.' > brief.md
"$BIN" company check .claude/companies/mini.toml
"$BIN" company run .claude/companies/mini.toml --brief brief.md --yes
echo "--- hello.py ---"; cat hello.py
