# essaim

TUI en Rust pour concevoir et exécuter des structures multi-agents avec Claude Code.
Tout passe par le CLI `claude` et **votre abonnement** : aucune clé API n'est utilisée.
Les variables `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`
et `CLAUDE_CODE_USE_FOUNDRY` sont retirées de chaque processus `claude` lancé par essaim. Un nœud `-p` dont la
session n'est pas sur l'abonnement est interrompu (voir [Limites connues](#limites-connues)).

## Installation

```sh
cd essaim
cargo build --release          # binaire : target/release/essaim
cargo install --path .         # optionnel : installe `essaim` dans ~/.cargo/bin
```

Prérequis :
- Rust, édition 2024 (donc Rust 1.85 ou plus récent, déduit de `Cargo.toml`).
- `claude` dans le PATH (ou désigné par `ESSAIM_CLAUDE_BIN`), connecté : `claude auth status` → `claude.ai`.
- Unix uniquement (voir [Limites connues](#limites-connues)).

## Installation comme commande système

Une ligne, sur macOS ou Linux (x86_64 / arm64), sans Rust : télécharge le binaire de la dernière release,
vérifie sa somme SHA-256, l'installe dans `~/.local/bin`, installe la page de manuel et ajoute le dossier au `PATH`
de votre shell (zsh, bash, fish).

```sh
curl -fsSL --proto '=https' --tlsv1.2 https://raw.githubusercontent.com/VincentChaye/essaim/main/install.sh | bash
```

Depuis un clone du dépôt, `./install.sh` compile avec cargo à la place. Options : `--prefix /usr/local` (sudo si
besoin), `--version vX.Y.Z`, `--from-source`, `--no-modify-path`, `--uninstall`.

Ensuite `essaim` se lance depuis n'importe quel dossier (c'est la racine du projet) et `man essaim` ouvre le manuel.
Sous Windows : utilisez WSL.

### Publier une release (mainteneur)

Le workflow `.github/workflows/release.yml` compile les 4 cibles (Linux et macOS, x86_64 et arm64) et publie les
archives `essaim-<cible>.tar.gz` + `.sha256` quand on pousse un tag :

```sh
git tag v0.1.0 && git push origin v0.1.0
```


## Reprendre une mission interrompue

Quand la conception d'une mission est menée à terme (sans échec ni annulation), son plan est marqué validé
(`.essaim/missions/<id>/conception.ok`). Si le run est ensuite annulé ou la réalisation refusée, la reprise repart de ce
plan : la conception n'est pas rejouée (aucun appel reconsommé), le point de contrôle puis la réalisation se relancent.

- TUI, onglet Entreprise : `r` reprend automatiquement une mission « plan validé · reprenable » ; `R` recommence à zéro.
- CLI : `essaim company run <e.toml> --mission <id> --resume` (erreur claire si rien n'est reprenable).

Limite : la reprise a lieu au niveau des phases. Une conception annulée en cours est rejouée en entier, et une
réalisation annulée repart du début du plan (les fichiers déjà modifiés restent en place : relisez-les).

## Modifier le plan avant la réalisation

Au point de contrôle (onglet Run), `e` ouvre une conversation avec le boss de l'entreprise (session Claude Code en mode
plan, onglet 6). Il lit le brief et le plan consolidé, discute des changements avec vous, puis réécrit
`.essaim/missions/<id>/conception.md`. En quittant (`/exit`), le plan relu s'affiche : `Entrée` lance la réalisation avec
la version révisée, `Échap` arrête, `e` rouvre la conversation. Le run reste en attente pendant la conversation.
La réalisation lit toujours `conception.md` au moment de la validation : une retouche manuelle du fichier est donc
aussi prise en compte. En CLI (`company run`), répondre `e` au point de contrôle fait la même chose : `claude` s'ouvre dans le terminal, puis le plan relu est réaffiché (`o` valide, `N` arrête, `e` recommence). Avec `--yes`, le plan n'est pas modifiable.

## Démarrage rapide

```sh
mkdir mon-projet && cd mon-projet
essaim init        # crée des agents et l'entreprise .claude/companies/acme-studio.toml
essaim             # ouvre la TUI
```

Dans la TUI : `2` (Entreprise) → `m` pour cadrer une mission avec le boss → `r` pour la lancer → l'onglet Run affiche l'avancement.

## Commandes

| Commande | Rôle |
|---|---|
| `essaim` | TUI (dossier courant = racine du projet) |
| `essaim init` | crée des agents et une entreprise d'exemple (sans écraser) |
| `essaim agents` | liste les agents |
| `essaim company show <e.toml>` | affiche l'organigramme (niveaux N, N-1…) et l'estimation d'appels |
| `essaim company check <e.toml>` | valide l'organigramme |
| `essaim company run <e.toml> (--brief "…" \| --mission <id> [--resume]) [--yes]` | exécute une mission (conception → plan → réalisation) ; `--resume` reprend une mission interrompue |

- `--root <dossier>` (défaut : dossier courant) est accepté par `init`, `agents`, `company check` et
  `company run`. `company show` ne l'accepte pas.
- `--brief @fichier` lit le brief dans un fichier. `--brief "…"` crée un nouveau dossier
  `.essaim/missions/<id>/` ; `--mission <id>` reprend un brief existant (`brief.md`).
- `--yes` est requis si l'estimation dépasse **30 appels** (seuil fixe). Il valide aussi le point de contrôle :
  la réalisation démarre sans invite. Sans `--yes`, essaim demande « Lancer la réalisation (les postes ⚒ vont modifier le projet) ? [o/N] » sur stdin.
- Codes de sortie : `0` = succès ; `1` = erreur (lecture, validation, `--yes` manquant) ou nœud/poste en échec.
  Une réalisation refusée sort aussi en `0` (voir [Limites connues](#limites-connues)).

## Touches

`Tab` / `Maj+Tab`, `F1`–`F5` ou `1`–`5` changent d'onglet · `Ctrl+K` ouvre l'aide de l'onglet courant (`m` : manuel complet au format `man`, navigation façon `less`) · `F10` ouvre les réglages (langue français/English, choisie au premier lancement ; l'interface suit ce choix, les consignes envoyées aux modèles sont en anglais — moins de tokens — avec la langue choisie en rappel et mémorisée dans `~/.config/essaim/settings.toml` ou équivalent de ta plateforme) · `q` ou `Ctrl+C` quitte (annule le run en cours).

- **Agents** : `n` créer **avec l'assistant** · `N` créer avec un formulaire · `e` ou `Entrée` éditer (formulaire) · `D` supprimer (confirmation `o` ou `y`).
  Formulaire : `Tab` champ suivant, `←` `→` choisir le modèle, `Ctrl+S` enregistrer, `Échap` annuler.

- **Entreprise** : `n` créer une entreprise **avec l'assistant** · `m` cadrer une mission avec le boss · `r` (ou `R`) lancer la mission choisie · `t`/`T` mission suivante/précédente · `Entrée` ouvrir une mission (brief, plan, rapport — anciennes comprises) · `c`/`C` entreprise suivante/précédente · `E` éditer le TOML (`$VISUAL` / `$EDITOR`).
- **Run** : `↑↓` choisir un nœud · `s` annuler le run en cours · `h` historique (`Entrée` rouvre un run, `D` le supprime).
- **Sessions** : `a` ou `Entrée` attacher (dans l'onglet Claude) · `s` arrêter · `x` supprimer (confirmation `o` ou `y`) · `r` rafraîchir.

### Onglet Claude : terminal intégré (`5`)

`n` (assistant) et `a` (attacher une session) ouvrent **un vrai Claude Code dans l'onglet 6**, sans quitter
l'outil. essaim possède le pseudo-terminal : il interprète l'affichage de Claude et lui réinjecte tes
touches (flèches, Entrée, Échap, Ctrl+…, collage…). Tout fonctionne comme dans un terminal normal :
questionnaires, validation du plan, demandes de permission, commandes `/…`.

- **Changer d'onglet sans quitter la session** (raccourcis interceptés avant d'atteindre Claude) :
  `F1`–`F6` · `Alt`+`1`–`6` (sur AZERTY : `Alt`+`&` `é` `"` `'` `(` `§`) · `Ctrl+PageUp` / `Ctrl+PageDown`.
  La session continue en arrière-plan (pastille ● sur l'onglet) ; en revenant sur l'onglet 6, le focus
  lui est rendu automatiquement.
  `Alt` demande que le terminal envoie Option/Alt comme « Meta » (iTerm2 : *Left Option key = Esc+* ;
  Terminal.app : *Utiliser Option comme touche Meta*). Sinon, `F1`–`F6` (avec `Fn` sur Mac) fonctionnent partout.
- `Ctrl+\` : bascule en touches de navigation sur l'onglet lui-même ; `Entrée` ou `i` reprend la session, `x` la ferme.
- Tant que le focus est dans la session, **toutes** les touches vont à Claude (y compris `q` et `Ctrl+C`).
- `/exit` dans la session la termine. Si elle a duré plus de 2 s et s'est terminée sans erreur, l'onglet se ferme et
  tu reviens là où tu étais. Sinon (sortie rapide ou code non nul), l'écran est conservé avec un bandeau ; une touche le ferme.
- Une seule session intégrée à la fois.

### Créer avec l'assistant (`n` dans Agents ou Entreprise)

Une session Claude Code **en mode plan** (`--permission-mode plan`) s'ouvre dans l'onglet 5. Le modèle te pose quelques questions,
te propose un plan, puis, une fois le plan validé, écrit le fichier :

- **Agent** : plan = nom, description, modèle, outils minimaux, grandes lignes du prompt → `.claude/agents/<nom>.md`.
- **Entreprise** : plan = organigramme commenté (postes, intitulés, niveaux, agents, qui peut modifier le projet,
  coût d'une mission) → `.claude/companies/<nom>.toml` **et** les agents manquants `.claude/agents/*.md`, écrits en un seul lot.
  Il réutilise tes agents existants. La consigne lui demande de n'en écraser aucun ; essaim ne le vérifie pas.

Claude Code demande la confirmation de l'écriture (comportement de Claude Code, non vérifié ici).
Quand la session se termine, la liste est rechargée, l'élément créé est sélectionné et, pour une entreprise,
sa validité est indiquée.

## Entreprise : organigramme et missions

Une **entreprise** (`.claude/companies/<nom>.toml`) place des agents dans un organigramme libre : chaque
*poste* a un intitulé réel (CEO, CTO, Engineering Manager, Backend Engineer…), un agent, et un supérieur
(`reports_to`). Le niveau se déduit de la profondeur : `N` (le boss), `N-1` (VP), `N-2`… Un poste `builds = true`
(⚒) est une feuille de réalisation : le seul type de poste qui modifie le projet. Il doit être une feuille
(sans subordonnés) et ne peut pas être le boss.

**Une mission** se déroule en trois temps :
1. **Brief** (`m`) : tu cadres la mission en conversation avec le boss (mode plan) ; il écrit `brief.md`.
   En ligne de commande, `--brief` tient lieu de ce cadrage.
2. **Conception** (`r`) : le boss découpe par domaine et délègue à ses subordonnés directs, qui spécialisent et
   délèguent à leur tour jusqu'aux workers (texte, lecture seule) ; les rapports remontent et sont synthétisés à
   chaque niveau. Le rapport du boss est le **plan consolidé**.
3. **Réalisation** : après validation du plan (Entrée / Échap dans la TUI ; invite `[o/N]` ou `--yes` en ligne de commande),
   la branche des postes ⚒ met en place dans le projet. Chaque délégation porte un périmètre de fichiers inclus dans celui
   du supérieur ; les périmètres qui se chevauchent sont exécutés l'un après l'autre. Un poste ⚒ peut lire
   (`Read`, `Grep`, `Glob`) et écrire (`Edit`, `Write`) uniquement dans son périmètre, et lancer `Bash` seulement
   pour les commandes de `allow_bash` (règles `Bash(<commande>:*)`) ; le reste est refusé (`--permission-mode dontAsk`).
   La portée exacte de ces règles relève de Claude Code et n'est pas vérifiée ici.

Livrables : `.essaim/missions/<id>/{brief,conception,rapport}.md`. `rapport.md` n'existe que si la réalisation
a été lancée et s'est terminée. Garde-fous dans le TOML : `max_calls` (plafond d'appels `claude` par mission,
partagé entre les phases, défaut 40), `max_parallel` (défaut 2), `checkpoint` (défaut `true`). L'estimation
d'appels est affichée avant le lancement.

```toml
name = "acme-studio"
max_calls = 40
checkpoint = true

[[seat]]
id = "ceo"
title = "CEO"
agent = "boss"

[[seat]]
id = "dev-api"
title = "Backend Engineer"
agent = "developer"
reports_to = "ceo"
builds = true
allow_bash = ["cargo test"]   # règles Bash(cargo test:*) pour ce poste
```

`essaim init` crée une entreprise d'exemple complète (9 postes).

## Formats

**Agent** : `.claude/agents/<nom>.md` (le format standard de Claude Code : frontmatter `name`, `description`,
`model`, `tools` séparés par des virgules, puis le prompt). Les agents sont cherchés dans le projet, puis dans
`~/.claude/agents/` ; le projet prime en cas de doublon. Un fichier qui ne se lit pas est ignoré en silence par
`agents` et `company run` ; la TUI le signale après une création avec l'assistant.

**Entreprise** : `.claude/companies/<nom>.toml`. Champs du poste : `id` (lettres, chiffres, `-`, `_`), `title`,
`agent` (obligatoires), `reports_to` (absent = le boss, un seul par entreprise), `expertise` (facultatif : domaine
présenté au supérieur pour déléguer ; absent, seul l'intitulé est présenté), `builds`, `allow_bash`.

Les runs lancés depuis la TUI sont enregistrés dans `.essaim/runs/<id>/run.json` (ajoutez `.essaim/` à `.gitignore`).

## Limites connues

- **Code de sortie 0 sur une réalisation refusée.** Au point de contrôle, un refus met `outcome.ok = conception.ok`
  (`src/runtime/company_exec.rs:577-578`) : si la conception a réussi, la mission est « ok ». `essaim company run`
  ne sort en `1` que si `!ok` (`src/main.rs:342-344`). Une réalisation refusée (Échap dans la TUI ; en ligne de commande,
  réponse autre que `o`/`oui`/`y`/`yes`, ou stdin fermé sans `--yes`) sort donc en `0`.
- **Pas de `--version`** : la commande `Cli` ne déclare pas de `version` (`src/main.rs:16`).
- **Pas de version minimale de `claude`** : aucune vérification de version. Les options utilisées
  (`--bg`, `--agents`, `--json-schema`, `--permission-mode`, `agents --json --all`) doivent exister dans votre version.
- **Unix uniquement** : `kill` est lancé par son nom (`src/runtime/claude_proc.rs:233`), le terminal intégré utilise
  un pseudo-terminal (`portable-pty`), et le code ne contient aucune branche Windows.
- **Vérification d'authentification** : une seule vérification réelle, sur le flux `-p`
  (`src/runtime/claude_proc.rs:221-239` : refus si `apiKeySource` n'est pas `none`). Elle couvre les appels locaux
  et `essaim company run`. Elle ne couvre ni les sessions `--bg` (`src/runtime/sessions.rs`), ni le
  terminal intégré (`src/tui/term.rs`), ni les appels `claude agents`, `stop` et `rm`.
  La barrière de suppression des variables est appliquée à deux endroits : `claude_command()`
  (`src/runtime/claude_proc.rs:99-105`) et `TermSession::spawn` (`src/tui/term.rs:78-80`).
- **Environnement hérité** : chaque `claude` lancé hérite de tout l'environnement d'essaim (aucun `env_clear`),
  sauf les cinq variables de `BILLING_ENV_VARS` (`src/runtime/claude_proc.rs:88-94`). Le terminal intégré ajoute
  `TERM=xterm-256color` et `COLORTERM=truecolor` (`src/tui/term.rs:75-76`). `ESSAIM_CLAUDE_BIN` remplace le binaire
  `claude` (`src/runtime/claude_proc.rs:100`, `src/tui/app.rs:649`). `VISUAL` ou `EDITOR` désigne l'éditeur, lancé sans shell
  (`src/tui/mod.rs:24-27`). Essaim ne retire pas les réglages d'API placés dans les fichiers de configuration de
  `claude` : cela n'est pas vérifié ici, et seul le contrôle `apiKeySource` (flux `-p`) le détecterait.
- **Bash des postes ⚒** : la portée des règles `Bash(<commande>:*)` relève de Claude Code, non vérifiée ici.
- Les entreprises se modifient dans un éditeur externe (`e` / `E`) ; l'onglet Entreprise propose aussi un formulaire de poste (`a` / `e` / `x`), qui réécrit le TOML et en supprime donc les commentaires.
- La délégation dynamique coûte jusqu'à ~2 appels par poste interne et par phase : surveille `max_calls` et ton quota.
- Les « coûts » affichés sont notionnels (calculés par `claude`), non facturés avec l'abonnement.
