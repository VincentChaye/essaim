# essaim

TUI en Rust pour concevoir et exécuter des structures multi-agents avec Claude Code.
Tout passe par le CLI `claude` et **votre abonnement** : aucune clé API n'est utilisée.
Les variables `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` et toute variable dont le nom commence par `CLAUDE_CODE_USE_`
(par exemple `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, `CLAUDE_CODE_USE_FOUNDRY`) sont retirées de chaque
processus `claude` lancé par essaim. Le préfixe est reconnu tel quel, en majuscules. Un nœud `-p` dont la
session n'est pas sur l'abonnement est interrompu (voir [Limites connues](#limites-connues)).

## Installation

```sh
cd essaim
cargo build --release          # binaire : target/release/essaim
cargo install --path .         # optionnel : installe `essaim` dans ~/.cargo/bin
```

Prérequis :
- Rust, édition 2024 (`Cargo.toml` : `edition = "2024"`, qui exige Rust 1.85 ou plus récent ; aucune `rust-version` n'est déclarée).
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
(`.essaim/missions/<id>/conception.ok`, avec un `conception.md` non vide). Si le run est ensuite annulé ou la réalisation refusée, la reprise repart de ce
plan : la conception n'est pas rejouée (aucun appel reconsommé), le point de contrôle puis la réalisation se relancent.

- TUI, onglet Entreprise : `r` reprend automatiquement une mission « plan validé · reprenable » ; `R` recommence à zéro.
- CLI : `essaim company run <e.toml> --mission <id> --resume` (erreur claire si rien n'est reprenable).

Limite : la reprise a lieu au niveau des phases. Une conception annulée en cours est rejouée en entier, et une
réalisation annulée repart du début du plan (les fichiers déjà modifiés restent en place : relisez-les).

## Modifier le plan avant la réalisation

Au point de contrôle (onglet Run), `e` ouvre une conversation avec le boss de l'entreprise (session Claude Code en mode
plan, dans l'onglet 5). Il lit le brief et le plan consolidé, discute des changements avec vous, puis réécrit
`.essaim/missions/<id>/conception.md`. En quittant (`/exit`), le plan relu s'affiche : `Entrée` lance la réalisation avec
la version révisée, `Échap` arrête, `e` rouvre la conversation. Le run reste en attente pendant la conversation.
La réalisation lit toujours `conception.md` au moment de la validation : une retouche manuelle du fichier est donc
aussi prise en compte. En CLI (`company run`), répondre `e` au point de contrôle fait la même chose : `claude` s'ouvre dans le terminal, puis le plan relu est réaffiché (`o` valide, `N` ou toute autre réponse arrête, `e` recommence). Avec `--yes`, le plan n'est pas modifiable.

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
| `essaim company run <e.toml> (--brief "…" \| --brief @fichier \| --mission <id> [--resume]) [--yes]` | exécute une mission (conception → plan → réalisation) ; `--resume` reprend une mission interrompue |

- `--root <dossier>` (défaut : dossier courant) est accepté par `init`, `agents`, `company check` et
  `company run`. `company show` ne l'accepte pas.
- `--brief @fichier` lit le brief dans un fichier. `--brief "…"` crée un nouveau dossier
  `.essaim/missions/<id>/` ; `--mission <id>` reprend un brief existant (`brief.md`). `--brief` et `--mission` ne se combinent pas ; `--resume` exige `--mission`.
- `--yes` est requis si l'estimation dépasse **30 appels** (seuil fixe). Il valide aussi le point de contrôle :
  la réalisation démarre sans invite. Sans `--yes`, essaim demande « Lancer la réalisation (les postes ⚒ vont modifier le projet) ? [o/N/e = modifier le plan avec le boss] » sur stdin.
- Codes de sortie : `0` = succès ; `1` = erreur (lecture, validation, `--yes` manquant) ou nœud/poste en échec.
  Une réalisation refusée sort aussi en `0` (voir [Limites connues](#limites-connues)).

Exemples qui ne lancent pas `claude` (copiables tels quels, à la racine d'un projet) :

```sh
essaim init
essaim agents
essaim company check .claude/companies/acme-studio.toml
essaim company show .claude/companies/acme-studio.toml
```

Exemple de mission (lance `claude` et consomme votre abonnement) :

```sh
printf '%s\n' "Ajouter une page d'accueil" > brief.md
essaim company run .claude/companies/acme-studio.toml --brief @brief.md
```

Reprise (remplacez `<id>` par le nom d'un dossier de `.essaim/missions/`) :

```sh
essaim company run .claude/companies/acme-studio.toml --mission <id> --resume
```

## Touches

**Cinq onglets**, dans cet ordre : `1` **Agents** · `2` **Entreprise** · `3` **Run** · `4` **Sessions** · `5` **Claude** (session Claude Code intégrée).

### Partout (hors session Claude)

- `Tab` / `Maj+Tab` : onglet suivant / précédent. `1`–`5` : onglet numéroté.
- `F1`–`F5` ou `Alt`+`1`–`5` : onglet numéroté, y compris dans la session Claude. Sur AZERTY, `Alt`+ `&` `é` `"` `'` `(` pour `1` à `5`.
- `Ctrl+PgUp` / `Ctrl+PgDn` : onglet précédent / suivant.
- `Ctrl+K` : ouvre ou ferme l'aide de l'onglet courant. Dans l'aide, `m` bascule vers le manuel complet au format `man` (navigation façon `less` : `↑` `↓` ou `j` `k`, `Espace` / `b`, `g` / `G`).
- `F10` : réglages. Langue français/English, choisie au premier lancement ; l'interface suit ce choix. Les consignes envoyées aux modèles sont en anglais (moins de tokens), avec la langue choisie en rappel. Le choix est mémorisé dans `settings.toml` (voir [Formats](#formats)).
- `q` ou `Ctrl+C` : quitte. Si un run est en cours, essaim demande confirmation (`o` ou `y`) ; le run est alors annulé.

### 1 · Agents

- `n` créer **avec l'assistant** · `N` créer avec un formulaire · `D` supprimer (confirmation `o` ou `y`).
- `e` ou `Entrée` : fenêtre de choix du mode d'édition. `1` ou `s` = édition simple (formulaire ou `$EDITOR`), `2` ou `m` = avec le modèle (conversation). `←` `→` ou `Tab` changent, `Entrée` valide, `Échap` annule.
- Formulaire : `Tab` champ suivant, `←` `→` choisir le modèle, `Ctrl+S` enregistrer, `Échap` annuler.

### 2 · Entreprise

L'onglet a trois zones : **Entreprises**, **Missions** et **organigramme**.

- `←` `→` (ou `h` `l`) passent d'une zone à l'autre (Entreprises ↔ Missions). `↓` va à l'organigramme, `↑` revient aux Missions depuis l'organigramme.
- `Entrée` entre dans la zone : les flèches n'agissent alors que dedans. `Échap` en sort.
- Zone Entreprises (après `Entrée`) : `↑` `↓` changent d'entreprise ; `Entrée` passe aux Missions.
- Zone Missions (après `Entrée`) : `↑` `↓` choisissent la mission ; `Entrée` ou `o` ouvre la mission (brief, plan, rapport, anciennes comprises) ; `Espace` (ou `←` `→`) déplie ou replie son résumé.
- Zone organigramme (après `Entrée`) : `↑` supérieur · `↓` premier subordonné · `←` `→` voisin du même niveau · `Home` ou `g` premier poste (le boss). `a` ajoute un poste sous le poste choisi · `e` édite le poste · `x` retire le poste (confirmation ; ses subordonnés remontent d'un cran). Le boss ne part que seul.
- `e` dans une zone entrée : édite le poste, l'entreprise ou le brief selon la zone. Une fenêtre propose l'édition simple (formulaire de poste, ou `$EDITOR` pour un fichier) ou l'édition avec le modèle. Les formulaires de poste réécrivent le TOML, qui perd alors ses commentaires.
- Hors zone : `n` créer une entreprise **avec l'assistant** · `m` cadrer une mission avec le boss · `c` / `C` entreprise suivante / précédente · `t` / `T` mission suivante / précédente.
- `r` lance la mission choisie. Si sa conception est menée à terme, elle reprend à la réalisation sans rejouer la conception. Sinon, ou si un plan ou un rapport existe déjà, une confirmation est demandée. `R` relance depuis le début, après confirmation si un plan ou un rapport existe.

### 3 · Run

- `↑` `↓` choisissent un nœud (son journal s'affiche) · `s` annule le run en cours (confirmation, si le run n'est pas terminé).
- `h` bascule l'historique. Dans l'historique, `↑` `↓` choisissent, `Entrée` rouvre un run, `D` le supprime (confirmation).
- Point de contrôle (plan consolidé, affiché à la fin de la conception quand `checkpoint = true`) : `Entrée` lance la réalisation · `Échap` arrête la mission sur le plan · `e` ouvre la conversation avec le boss (voir [Modifier le plan](#modifier-le-plan-avant-la-réalisation)) · `↑` `↓` ou `j` `k`, `PgUp` / `PgDn` font défiler le plan.

### 4 · Sessions

Sessions background de Claude Code.

- `↑` `↓` choisissent une session · `a` ou `Entrée` l'attache dans l'onglet 5 · `s` arrête · `x` supprime (confirmation `o` ou `y`) · `r` rafraîchit la liste.

### 5 · Claude : terminal intégré

`n` (assistant, dans Agents ou Entreprise) et `a` (attacher une session, dans Sessions) ouvrent **un vrai Claude Code dans l'onglet 5**, sans quitter
l'outil. essaim possède le pseudo-terminal : il interprète l'affichage de Claude et lui réinjecte tes
touches (flèches, Entrée, Échap, Ctrl+…, collage…). Tout fonctionne comme dans un terminal normal :
questionnaires, validation du plan, demandes de permission, commandes `/…`.

- **Changer d'onglet sans quitter la session** (raccourcis interceptés avant d'atteindre Claude) :
  `F1`–`F5` · `Alt`+`1`–`5` (sur AZERTY : `Alt`+`&` `é` `"` `'` `(`) · `Ctrl+PgUp` / `Ctrl+PgDn`.
  `Ctrl+K` et `F10` sont aussi interceptés.
  La session continue en arrière-plan (pastille ● sur l'onglet) ; en revenant sur l'onglet 5, le focus
  lui est rendu automatiquement.
  `Alt` demande que le terminal envoie Option/Alt comme « Meta » (iTerm2 : *Left Option key = Esc+* ;
  Terminal.app : *Utiliser Option comme touche Meta*). Sinon, `F1`–`F5` (avec `Fn` sur Mac) fonctionnent partout.
- `Ctrl+\` : rend les touches de navigation à essaim (la session continue). `Entrée` ou `i` reprend la session, `x` la ferme (confirmation).
- Tant que le focus est dans la session, **toutes** les autres touches vont à Claude (y compris `q` et `Ctrl+C`).
- `/exit` dans la session la termine. Si elle a duré plus de 2 s et s'est terminée sans erreur, l'onglet se ferme et
  tu reviens là où tu étais. Sinon (sortie rapide ou code non nul), l'écran est conservé avec un bandeau ; une touche le ferme.
- Une seule session intégrée à la fois : si une session est déjà ouverte, essaim le signale et n'en lance pas une seconde.

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
3. **Réalisation** : après validation du plan (Entrée / Échap dans la TUI ; invite `[o/N/e]` ou `--yes` en ligne de commande),
   la branche des postes ⚒ met en place dans le projet. Chaque délégation porte un périmètre de fichiers inclus dans celui
   du supérieur ; les périmètres qui se chevauchent sont exécutés l'un après l'autre. Un poste ⚒ peut lire
   (`Read`, `Grep`, `Glob`) et écrire (`Edit`, `Write`) uniquement dans son périmètre, et lancer `Bash` seulement
   pour les commandes de `allow_bash` (règles `Bash(<commande>:*)`) ; le reste est refusé (`--permission-mode dontAsk`).
   La portée exacte de ces règles relève de Claude Code et n'est pas vérifiée ici.

### Blocages : corriger, remonter, puis décider avec le CEO

Un poste n'est « fait » que s'il l'affirme **et** que rien ne l'a empêché de finir : chaque appel de travail ou de
rapport rend un statut structuré (`done` / `blocked` / `failed`) avec ses blocages (`tests`, `permission`,
`prerequisite`, `bug`, `scope`, `other`) et ce qu'il faudrait pour les lever. Un refus de permission (`Bash(git status)`
refusé…) est toujours un blocage, même si l'agent se déclare « done ». Un poste bloqué n'est plus ✔ mais ⚠.

1. **Correction dans la branche** : le supérieur reçoit l'échec, écrit un plan de correction (cause, étapes) et réassigne
   des tâches correctives, éventuellement à un autre subordonné (celui qui a l'outil manquant, l'équipe spécialisée).
   Au plus `max_fix_rounds` tours (défaut 2), toujours bornés par `max_calls`.
2. **Remontée** : s'il n'a personne de compétent, il remonte le blocage (`blocked`) à son propre supérieur, qui peut
   rediriger vers une autre équipe de l'entreprise, et ainsi de suite jusqu'au CEO.
3. **CEO → vous** : si le CEO n'y arrive pas non plus, le run se met en pause (« Mission bloquée ») :
   `e` ouvre une session avec le CEO (il explique le blocage, propose un plan, peut créer ou ajuster un poste, un agent
   ou une permission, et écrit votre décision dans `.essaim/missions/<id>/resolution.md`) · `Entrée` relance la correction
   avec cette décision · `Échap` arrête la mission. En CLI : `[r / e / N]` ; avec `--yes`, la mission s'arrête
   (jamais de poursuite automatique sur un blocage).

Lots de réalisation : si un lot reste bloqué, les suivants ne démarrent pas. Une conception bloquée ne lance pas la
réalisation. Le compte rendu affiche « ⚠ bloquée » et la liste des blocages.

**Outillage selon la taille** : dans une petite entreprise sans équipe spécialisée, les postes ⚒ ont eux-mêmes `git`
(lecture, `add`, `commit`), `wc`, `grep`, `diff` dans `allow_bash` ; avec une équipe spécialisée (release, ops), seul ce
poste les a et les managers lui routent le besoin (la fiche de poste indique à chaque manager qui a quels outils).


**Point de contrôle** (`checkpoint`, défaut `true`). Quand il est actif, le plan consolidé est affiché avant toute
réalisation et demande validation. En ligne de commande, sans `--yes`, essaim pose la question
« Lancer la réalisation (les postes ⚒ vont modifier le projet) ? [o/N/e = modifier le plan avec le boss] » ; `o`, `oui`, `y` ou `yes`
lance la réalisation, toute autre réponse arrête la mission sur le plan.
Avec `checkpoint = false`, il n'y a ni plan à valider ni invite : la réalisation démarre dès la fin de la conception.
Un avertissement d'exécution signalera ce cas : **À CONFIRMER** (texte exact et canal, stderr ou bandeau TUI, à reprendre de la décision CTO ; il n'est pas encore présent dans le code).

Livrables : `.essaim/missions/<id>/{brief,conception,rapport}.md`. `rapport.md` n'existe que si la réalisation
a été lancée et s'est terminée. Garde-fous dans le TOML : `max_calls` (plafond d'appels `claude` par mission,
partagé entre les phases, défaut 40), `max_parallel` (défaut 2), `checkpoint` (défaut `true`), `max_fix_rounds` (défaut 2). L'estimation
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

**Réglages** : `settings.toml` (langue) est stocké dans le dossier de configuration de la plateforme (par exemple `~/.config/essaim/` sous Linux).
La variable `ESSAIM_CONFIG_DIR` remplace ce dossier.

## Limites connues

- **Code de sortie 0 sur une réalisation refusée.** Au point de contrôle, un refus met `outcome.ok = conception.ok`
  (fonction `run_mission`, module `runtime::company_exec`) : si la conception a réussi, la mission est « ok ». `essaim company run`
  ne sort en `1` que si la mission n'est pas `ok` (fonction `main`, branche `company run`). Une réalisation refusée (Échap dans la TUI ; en ligne de commande,
  réponse autre que `o`/`oui`/`y`/`yes`, ou stdin fermé sans `--yes`) sort donc en `0`.
- **Pas de `--version`** : la structure `Cli` (module `main`) ne déclare pas de `version`.
- **Pas de version minimale de `claude`** : aucune vérification de version. Les options utilisées
  (`--bg`, `--agents`, `--json-schema`, `--permission-mode`, `agents --json --all`) doivent exister dans votre version.
- **Unix uniquement** : `kill` est lancé par son nom (fonction `spawn_with`, module `runtime::claude_proc`), le terminal intégré utilise
  un pseudo-terminal (`portable-pty`, module `tui::term`), et le code ne contient aucune branche Windows.
- **Vérification d'authentification** : une seule vérification réelle, sur le flux `-p` (fonction `spawn_with`,
  module `runtime::claude_proc` : refus si `apiKeySource` n'est pas `none`). Elle couvre les appels locaux
  et `essaim company run`. Elle ne couvre ni les sessions `--bg` (module `runtime::sessions`), ni le
  terminal intégré (module `tui::term`), ni les appels `claude agents`, `stop` et `rm`.
  La suppression des variables de facturation est appliquée à deux endroits : `claude_command()` (module `runtime::claude_proc`)
  et `TermSession::spawn` (module `tui::term`). Les noms retirés sont `BILLING_ENV_VARS` (liste) et `BILLING_ENV_PREFIX` (préfixe `CLAUDE_CODE_USE_`).
- **Environnement hérité** : chaque `claude` lancé hérite de tout l'environnement d'essaim (aucun `env_clear`),
  sauf les variables de facturation décrites ci-dessus. Le terminal intégré ajoute
  `TERM=xterm-256color` et `COLORTERM=truecolor` (fonction `TermSession::spawn`). `ESSAIM_CLAUDE_BIN` remplace le binaire
  `claude` (fonction `claude_command`, et lancement de la session intégrée dans le module `tui::app`). `VISUAL` ou `EDITOR` désigne l'éditeur, lancé sans shell
  (fonction `open_in_editor`, module `tui`). Essaim ne retire pas les réglages d'API placés dans les fichiers de configuration de
  `claude` : cela n'est pas vérifié ici, et seul le contrôle `apiKeySource` (flux `-p`) le détecterait.
- **Bash des postes ⚒** : la portée des règles `Bash(<commande>:*)` relève de Claude Code, non vérifiée ici.
- Les entreprises se modifient dans un éditeur externe (`e` dans une zone entrée, option « simple ») ; l'onglet Entreprise propose aussi un formulaire de poste (`a` / `e` / `x`, dans l'organigramme), qui réécrit le TOML et en supprime donc les commentaires.
- La délégation dynamique coûte jusqu'à ~2 appels par poste interne et par phase : surveille `max_calls` et ton quota.
- Les « coûts » affichés sont notionnels (calculés par `claude`), non facturés avec l'abonnement.
