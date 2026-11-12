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
besoin), `--version vX.Y.Z`, `--from-source`, `--no-modify-path`, `--profile dedicated|protected`, `--uninstall`.

Ensuite `essaim` se lance depuis n'importe quel dossier (c'est la racine du projet) et `man essaim` ouvre le manuel.
Sous Windows : utilisez WSL.

### Profil de sécurité (question posée à l'installation)

Une fois le binaire en place, l'installeur demande sur quelle machine essaim tourne. La réponse fixe les droits de
**tous** les agents de **toutes** les entreprises :

| Profil | Pour quelle machine | Droits des agents | Demandes à l'humain |
|---|---|---|---|
| `dedicated` (appareil dédié) | sans données personnelles : machine virtuelle, poste réservé aux agents | tous : toute commande, tout fichier, le réseau, sans jamais demander (`--permission-mode bypassPermissions`). Rien n'empêche matériellement un agent d'agir hors du dossier du projet. | questions et actions seulement |
| `protected` (protégé) | votre machine personnelle | les commandes tournent dans le **bac à sable de Claude Code**, tenu par le système (Seatbelt sous macOS, bubblewrap sous Linux) : écriture limitée au dossier du projet, pas de lecture de votre dossier personnel, réseau limité aux registres de paquets | en plus : tout ce qui sort du cadre (commande hors bac à sable, autre site web, fichier hors du projet) |

- `./install.sh --profile dedicated` (ou `protected`) répond d'avance. Sans terminal (installation automatisée), la
  question est posée au premier lancement de la TUI.
- Le choix se change à tout moment : `essaim profile` (explication et question), `essaim profile dedicated`, ou `F10`
  dans la TUI. Il est écrit dans `settings.toml` (voir [Formats](#formats)).
- Tant qu'aucun profil n'est choisi, c'est `protected` qui s'applique.
- Profil protégé sous Linux : `bubblewrap` et `socat` doivent être installés (l'installeur le vérifie). Si le bac à
  sable ne peut pas démarrer, l'agent ne lance aucune commande : il n'y a jamais de repli silencieux hors bac à sable.
- Limite du profil protégé : le bac à sable couvre les commandes shell et leurs processus enfants. Les outils de
  fichier et le web passent par les permissions de Claude Code, donc par vos réponses.
- Coût du profil protégé : Claude Code décrit le bac à sable dans le prompt système de chaque appel, soit environ
  2 000 jetons de plus par appel.

### Publier une release (mainteneur)

Le workflow `.github/workflows/release.yml` compile les 4 cibles (Linux et macOS, x86_64 et arm64) et publie les
archives `essaim-<cible>.tar.gz` + `.sha256` quand on pousse un tag :

```sh
git tag v0.1.0 && git push origin v0.1.0
```


## Reprendre une mission interrompue

Quand le boss a fini d'écrire le plan d'une mission (sans échec ni annulation), ce plan est marqué validé
(`.essaim/missions/<id>/conception.ok`, avec un `conception.md` non vide). Si le run est ensuite annulé ou la réalisation refusée, la reprise repart de ce
plan : il n'est pas réécrit (aucun appel reconsommé), le point de contrôle puis la réalisation se relancent.

- TUI, onglet Entreprise : `r` reprend automatiquement une mission « plan validé · reprenable » ; `R` recommence à zéro.
- CLI : `essaim company run <e.toml> --mission <id> --resume` (erreur claire si rien n'est reprenable).

Limite : la reprise a lieu au niveau des phases. Un plan annulé en cours d'écriture est réécrit en entier, et une
réalisation annulée repart du début du plan (les fichiers déjà modifiés restent en place : relisez-les).
Quitter essaim alors qu'un agent attend votre réponse annule le run de la même façon.

## Mettre un run en pause (et la limite d'abonnement)

Un run peut s'arrêter en pause au lieu d'échouer, et reprendre plus tard — même après avoir quitté essaim.

- **Limite d'abonnement atteinte** : le run se met en pause tout seul. Le message de limite n'est pas traité comme une
  erreur du poste : aucun poste n'est marqué en échec, l'appel refusé ne compte pas dans le plafond d'appels, et il est
  rejoué tel quel à la reprise. L'onglet Run et la barre d'onglets affichent `⏸` avec l'heure de reprise, lue dans le
  message de `claude` ; le run repart seul à cette heure (sans heure annoncée, il retente au bout de 10 minutes).
- **À la main** : `p` dans l'onglet Run met en pause ou reprend. Aucun nouvel appel ne part, et les agents **déjà
  en vol sont arrêtés** : un prompt prioritaire leur est poussé dans leur session (« n'entreprends plus rien, termine
  proprement l'action en cours, rends un bilan court »). Mesuré sur `claude` 2.1.316 : l'agent finit son action en
  cours puis rend son bilan en quelques secondes. S'il ne répond pas dans les 45 s, son processus est tué. Dans les
  deux cas le poste **n'échoue pas** et l'appel ne consomme pas le plafond : à la reprise, l'agent repart sur sa
  propre session (`--resume`, contexte conservé) avec son bilan en main, et ne refait pas ce qu'il avait terminé.
- **Quitter pendant une pause** : `q` le propose sans parler d'annulation. La pause est écrite dans la mission
  (`.essaim/missions/<id>/pause`, avec l'heure de reprise) : la mission apparaît « ⏸ en pause » dans la liste et `r` la
  reprend, dans cette session ou une autre, sans confirmation. Si la fenêtre d'abonnement n'est pas encore
  réinitialisée, le run attend l'heure annoncée avant son premier appel au lieu de se faire refuser.
- **`s` (annuler)** efface la pause : la mission ne sera pas reprise.
- **CLI** : `company run` annonce la pause et la reprise sur la sortie d'erreur (`⏸` / `▶`) et attend de lui-même.
  `Ctrl+C` pendant la pause garde la mission reprenable (`--mission <id> --resume`).

La pause **à la main** arrête les agents en vol de façon reprenable (ci-dessus). La pause de **limite
d'abonnement**, elle, laisse les appels déjà partis aller au bout : ils sont déjà lancés, et s'ils butent eux aussi
sur la limite ils repassent par le rejeu, qui ne consomme pas d'appel — les interrompre ne gagnerait rien et ferait
perdre leur travail. Dans les deux cas on ne perd pas la phase en cours. En revanche, quitter essaim puis reprendre
rejoue la phase en cours (voir la limite ci-dessus).

## Modifier le plan avant la réalisation

Au point de contrôle (onglet Run), `e` ouvre une conversation avec le boss de l'entreprise (session Claude Code en mode
plan, dans l'onglet 5). Il lit le brief et le plan, discute des changements avec vous, puis réécrit
`.essaim/missions/<id>/conception.md`. En quittant (`/exit`), le plan relu s'affiche : `Entrée` lance la réalisation avec
la version révisée, `Échap` arrête, `e` rouvre la conversation. Le run reste en attente pendant la conversation.
La réalisation lit toujours `conception.md` au moment de la validation : une retouche manuelle du fichier est donc
aussi prise en compte. En CLI (`company run`), répondre `e` au point de contrôle fait la même chose : `claude` s'ouvre dans le terminal, puis le plan relu est réaffiché (`o` valide, `N` ou toute autre réponse arrête, `e` recommence). Avec `--yes`, le plan n'est pas modifiable.

## Vos agents et vos entreprises, partout

Agents et entreprises sont **à vous, pas au projet** : ils sont rangés dans votre dossier personnel et
suivent donc tous vos dossiers.

| | Lu depuis | Écrit par essaim |
|---|---|---|
| `~/.claude/agents/<nom>.md` | n'importe quel dossier | oui : toute création va là |
| `~/.claude/companies/<nom>.toml` | n'importe quel dossier | oui : toute création va là |
| `<projet>/.claude/agents/`, `<projet>/.claude/companies/` | ce projet seul | seulement si le fichier y est déjà |

- Un agent ou une entreprise créé dans la TUI (`n`, `N`) ou par un assistant est écrit dans `~/.claude/` :
  vous le retrouvez dans tous vos projets, sans rien recopier.
- Un fichier **déjà** présent dans le projet est modifié sur place, jamais dupliqué ; à nom de fichier
  égal, c'est lui qui est chargé (et lui seul) quand on travaille dans ce projet.
- Dans la TUI, une entreprise suivie de `~` est la vôtre ; le détail d'un agent indique son fichier et
  sa portée (« toutes vos sessions » ou « ce projet »).
- En ligne de commande, l'entreprise se désigne **par son nom** : `essaim company run acme-studio`
  (un chemin de fichier reste accepté).
- Les **missions** et l'**historique** restent dans le projet (`.essaim/`) : ils parlent de ce projet.

## Démarrage rapide

```sh
essaim init        # crée vos agents et l'entreprise ~/.claude/companies/acme-studio.toml
mkdir mon-projet && cd mon-projet
essaim             # ouvre la TUI : vos agents et entreprises sont déjà là
```

Dans la TUI : `2` (Entreprise) → `m` pour cadrer une mission avec le boss → `r` pour la lancer → l'onglet Run affiche l'avancement.

## Commandes

| Commande | Rôle |
|---|---|
| `essaim` | TUI (dossier courant = racine du projet) |
| `essaim init` | crée des agents et une entreprise d'exemple dans `~/.claude` (sans écraser) ; `--root <projet>` les crée dans ce projet seul |
| `essaim agents` | liste les agents (`~` = agent personnel, disponible partout) |
| `essaim company show <entreprise>` | affiche l'organigramme (niveaux N, N-1…) et l'estimation d'appels |
| `essaim company check <entreprise>` | valide l'organigramme |
| `essaim company run <entreprise> (--brief "…" \| --brief @fichier \| --mission <id> [--resume]) [--yes]` | exécute une mission (plan du boss → validation → réalisation) ; `--resume` reprend une mission interrompue |
| `essaim profile [dedicated\|protected]` | affiche ou change le [profil de sécurité](#profil-de-sécurité-question-posée-à-linstallation) ; sans argument, explique les deux profils et pose la question |
| `essaim conso` (alias `essaim usage`) | ouvre le [tableau de bord de consommation](#tableau-de-bord-de-consommation) dans le navigateur |

- `<entreprise>` est un **nom** (`acme-studio`, avec ou sans `.toml`) cherché dans le projet puis dans
  `~/.claude/companies/`, ou un chemin de fichier.
- `--root <dossier>` (défaut : dossier courant) est accepté par `init`, `agents`, `company check` et
  `company run`. `company show` ne l'accepte pas. Pour `init`, il change le dossier de création
  (ce projet au lieu de `~/.claude`) ; pour les autres, il fixe la racine du projet.
- `--brief @fichier` lit le brief dans un fichier. `--brief "…"` crée un nouveau dossier
  `.essaim/missions/<id>/` ; `--mission <id>` reprend un brief existant (`brief.md`). `--brief` et `--mission` ne se combinent pas ; `--resume` exige `--mission`.
- `--yes` est requis si l'estimation dépasse **30 appels** (seuil fixe). Il valide aussi le point de contrôle :
  la réalisation démarre sans invite. Sans `--yes`, essaim demande « Lancer la réalisation (les postes vont modifier le projet) ? [o/N/e = modifier le plan avec le boss] » sur stdin.
- Pendant le run, les **demandes des agents** (question, action, autorisation, poste bloqué, revue) s'affichent sur la
  sortie d'erreur et la réponse se lit sur stdin : un numéro d'option ou un texte libre. Avec `--yes`, personne ne
  répond : voir [Sans humain](#sans-humain---yes).
- Codes de sortie : `0` = succès ; `1` = erreur (lecture, validation, `--yes` manquant) ou poste en échec ou bloqué.
  Une réalisation refusée sort aussi en `0` (voir [Limites connues](#limites-connues)).

Exemples qui ne lancent pas `claude` (copiables tels quels, à la racine d'un projet) :

```sh
essaim init
essaim agents
essaim company check acme-studio
essaim company show acme-studio
```

Exemple de mission (lance `claude` et consomme votre abonnement) :

```sh
printf '%s\n' "Ajouter une page d'accueil" > brief.md
essaim company run acme-studio --brief @brief.md
```

Reprise (remplacez `<id>` par le nom d'un dossier de `.essaim/missions/`) :

```sh
essaim company run acme-studio --mission <id> --resume
```

## Touches

**Cinq onglets**, dans cet ordre : `1` **Agents** · `2` **Entreprise** · `3` **Run** · `4` **Sessions** · `5` **Claude** (session Claude Code intégrée).

### Partout (hors session Claude)

- `Tab` / `Maj+Tab` : onglet suivant / précédent. `1`–`5` : onglet numéroté.
- `F1`–`F5` ou `Alt`+`1`–`5` : onglet numéroté, y compris dans la session Claude. Sur AZERTY, `Alt`+ `&` `é` `"` `'` `(` pour `1` à `5`.
- `Ctrl+PgUp` / `Ctrl+PgDn` : onglet précédent / suivant.
- `Ctrl+K` : ouvre ou ferme l'aide de l'onglet courant. Dans l'aide, `m` bascule vers le manuel complet au format `man` (navigation façon `less` : `↑` `↓` ou `j` `k`, `Espace` / `b`, `g` / `G`).
- `F9` : ouvre le [tableau de bord de consommation](#tableau-de-bord-de-consommation) dans le navigateur (page locale). Le serveur démarre au premier appui et vit jusqu'à la fermeture d'essaim ; les appuis suivants rouvrent l'onglet.
- `F10` : réglages, en deux pages : la langue (français/English) puis le profil de sécurité, avec son explication. Les deux sont demandés au premier lancement s'ils n'ont jamais été choisis ; l'interface suit la langue choisie. Les consignes envoyées aux modèles sont en anglais (moins de tokens), avec la langue choisie en rappel. Le choix est mémorisé dans `settings.toml` (voir [Formats](#formats)).
- `q` ou `Ctrl+C` : quitte. Si un run est en cours, essaim demande confirmation (`o` ou `y`) ; le run est alors annulé. Si le run est en pause, rien n'est perdu : la mission reste en pause et se reprend plus tard.

### 1 · Agents

- `n` créer **avec l'assistant** · `N` créer avec un formulaire · `D` supprimer (confirmation `o` ou `y`).
- Un agent créé est écrit dans `~/.claude/agents/` : il vous suit dans tous vos dossiers. Le détail indique son fichier et sa portée (« toutes vos sessions » ou « ce projet »).
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
- Dans la liste, une entreprise suivie de `~` est la vôtre (`~/.claude/companies/`), donc disponible depuis tous vos dossiers ; sans `~`, elle est propre au projet. Le titre de l'organigramme le rappelle.
- Hors zone : `n` créer une entreprise **avec l'assistant** · `m` cadrer une mission avec le boss · `c` / `C` entreprise suivante / précédente · `t` / `T` mission suivante / précédente.
- `r` lance la mission choisie. Si son plan est déjà écrit, elle reprend à la réalisation sans le réécrire. Une mission « ⏸ en pause » se reprend sans confirmation. Sinon, ou si un plan ou un rapport existe déjà, une confirmation est demandée. `R` relance depuis le début, après confirmation si un plan ou un rapport existe.

### 3 · Run

- `↑` `↓` choisissent un nœud (son journal s'affiche) · `p` met en pause ou reprend · `s` annule le run en cours (confirmation, si le run n'est pas terminé).
- Pause (voulue ou due à la limite d'abonnement) : l'en-tête affiche `⏸` et l'heure de reprise. Voir [Mettre un run en pause](#mettre-un-run-en-pause-et-la-limite-dabonnement).
- `h` bascule l'historique. Dans l'historique, `↑` `↓` choisissent, `Entrée` rouvre un run, `D` le supprime (confirmation).
- **Demandes des agents** : quand un agent s'adresse à vous, l'onglet s'ouvre sur sa demande et la barre d'onglets affiche `✋` avec le nombre de demandes en attente. `↑` `↓` ou un chiffre choisissent une option, taper du texte écrit une réponse libre, `Entrée` envoie. `Échap` met la demande de côté (l'agent attend, les autres postes continuent) et `d` la rouvre. Le délai `node_timeout_s` ne court pas pendant qu'un agent vous attend.
- Point de contrôle (plan du boss, affiché avant la réalisation quand `checkpoint = true`) : `Entrée` lance la réalisation · `Échap` arrête la mission sur le plan · `e` ouvre la conversation avec le boss (voir [Modifier le plan](#modifier-le-plan-avant-la-réalisation)) · `↑` `↓` ou `j` `k`, `PgUp` / `PgDn` font défiler le plan.

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

- **Agent** : plan = nom, description, modèle, outils minimaux, grandes lignes du prompt → `~/.claude/agents/<nom>.md`.
- **Entreprise** : plan = organigramme commenté (postes, intitulés, niveaux, agents, qui peut modifier le projet,
  coût d'une mission) → `~/.claude/companies/<nom>.toml` **et** les agents manquants `~/.claude/agents/*.md`, écrits en un seul lot.
  Il réutilise tes agents existants. La consigne lui demande de n'en écraser aucun ; essaim ne le vérifie pas.

Claude Code demande la confirmation de l'écriture (comportement de Claude Code, non vérifié ici).
Quand la session se termine, la liste est rechargée, l'élément créé est sélectionné et, pour une entreprise,
sa validité est indiquée.

## Entreprise : organigramme et missions

Une **entreprise** (`~/.claude/companies/<nom>.toml`, ou `.claude/companies/` dans le projet) place des agents dans un organigramme libre : chaque
*poste* a un intitulé réel (CEO, CTO, Engineering Manager, Backend Engineer…), un agent, et un supérieur
(`reports_to`). Le niveau se déduit de la profondeur : `N` (le boss), `N-1` (VP), `N-2`… Un poste `builds = true`
(⚒) est une feuille de réalisation : son rôle est de modifier le projet. Il doit être une feuille
(sans subordonnés) et ne peut pas être le boss. Une feuille sans ⚒ analyse et conseille. Ce partage des rôles tient
par la fiche de poste de chaque agent, pas par ses droits : tous les postes ont les mêmes outils (voir
[Droits des agents](#droits-des-agents)).

**Une mission** se déroule en trois temps :
1. **Brief** (`m`) : tu cadres la mission en conversation avec le boss (mode plan) ; il écrit `brief.md`.
   En ligne de commande, `--brief` tient lieu de ce cadrage.
2. **Plan** (`r`) : le boss écrit le plan **seul, en un appel**. Il lit le projet, cherche sur le web si besoin et
   **vous pose ses questions avant de rédiger** (voir [Les agents vous parlent](#les-agents-vous-parlent)). Le plan
   est enregistré dans `conception.md`.
3. **Réalisation** : après validation du plan (Entrée / Échap dans la TUI ; invite `[o/N/e]` ou `--yes` en ligne de commande),
   **une seule descente** de l'organigramme. Chaque supérieur découpe sa tâche et la délègue à ses subordonnés directs,
   les feuilles travaillent, puis chaque supérieur synthétise les résultats de sa branche. Le boss relit le résultat
   contre l'objectif et rend le rapport (`rapport.md`).

La réalisation a lieu pour toute entreprise, avec ou sans poste ⚒.

### Droits des agents

Tous les postes, pour tous leurs appels, ont les mêmes outils : `Read`, `Grep`, `Glob`, `Edit`, `Write`, `Bash`,
`WebSearch`, `WebFetch` (plus `Agent` pour un poste qui a des `helpers`). La ligne `tools:` d'un fichier d'agent ne
restreint rien quand l'agent travaille dans une entreprise. Ce que ces outils peuvent atteindre dépend du
[profil de sécurité](#profil-de-sécurité-question-posée-à-linstallation) :

- **Appareil dédié** : tout, sans demande.
- **Protégé** : librement dans le dossier du projet (fichiers et commandes dans le bac à sable). Une commande à
  relancer hors du bac à sable, un domaine réseau hors de la liste contacté par une commande, une page web à lire
  (`WebFetch`) ou une écriture hors du projet deviennent une demande d'autorisation : *autoriser une fois*, *autoriser
  pour la mission* (la même demande d'un autre poste est alors accordée sans vous déranger ; un site accordé l'est pour
  les commandes comme pour la lecture de pages) ou *refuser*. Une commande qui reste dans le bac à sable n'est jamais
  soumise, `WebSearch` et `ask_human` non plus. L'outil `Read` refuse les fichiers hors du projet.

Dans les deux profils, la fiche de poste dit à chaque agent de rester dans le dossier du projet.

### Les agents vous parlent

Chaque agent dispose d'un outil `ask_human` (serveur MCP intégré à essaim), utilisable **à tout moment** de son appel :
pendant le plan, une délégation, le travail, un rapport ou la revue. L'appel bloque jusqu'à votre réponse, puis
l'agent continue **dans la même session** : aucun appel `claude` supplémentaire. Les autres postes continuent pendant ce temps.

- Une demande est d'une de ces natures : `question` (affiner la tâche), `action` (quelque chose que vous seul pouvez
  faire : fournir un secret, brancher un appareil, créer un compte) ou `blocked`. Elle porte 1 à 4 questions, chacune
  avec des options proposées et, si l'agent en a une, l'option qu'il recommande. Vous choisissez une option ou
  écrivez une réponse libre.
- La règle donnée à chaque poste : un **fait** (ce que contient le projet, ce que dit une documentation) se cherche
  soi-même, dans le projet ou sur le web, sans rien demander ; une **intention** (ambiguïté, choix entre plusieurs
  options valables, priorité, critère de réussite) vous est demandée dès qu'elle apparaît, avant de construire dessus.
  Un poste ne demande jamais rien à son supérieur en cours de tâche.
- Chaque question et sa réponse sont ajoutées à `.essaim/missions/<id>/notes/decisions.md`. Les postes lisent ce
  fichier avant de poser une question, pour ne pas reposer la même.

### Information : des pointeurs, pas des copies

Le supérieur distribue l'information : chaque affectation porte la tâche, un périmètre de fichiers (`scope`) et une
liste `context` de **pointeurs** (fichiers ou dossiers du projet, notes de la mission, URL) plutôt que des copies. Le
subordonné lit lui-même ce qu'on lui indique, et va chercher seul ce qui lui manque, dans le projet ou sur le web.

Les livrables longs vont dans le dossier partagé `.essaim/missions/<id>/notes/` (`notes/<poste>.md`) : le poste ne
remonte qu'un résumé court avec le chemin. Les périmètres ne sont plus des règles de permission : ils servent à
l'ordonnancement, deux affectations dont les périmètres se chevauchent étant exécutées l'une après l'autre.

### Missions liées

Des missions peuvent dépendre les unes des autres, même si elles sont lancées par des entreprises différentes.
Onglet Entreprise, zone Missions : `a` coche les missions à réussir **avant** la mission sélectionnée (les cycles sont
refusés). Les liens sont stockés dans `.essaim/missions/<id>/after`.

- Une mission dont un prérequis n'est pas réussi est **grisée** (« en attente ») et ne se lance pas (`r`/`R` expliquent
  quoi réussir d'abord ; `essaim company run --mission` refuse aussi). Chaque mission affiche `↳ après ✔/✖ …` et
  `↦ débloque …` : c'est l'ordre de lancement.
- Une mission est **réussie** quand son run se termine sans échec ni blocage, avec sa réalisation faite si l'entreprise
  en a une (un plan refusé au point de contrôle n'est pas un succès). Marque `.essaim/missions/<id>/reussie`.
- À la réussite, essaim propose de lancer la mission débloquée (`o` la lance avec son entreprise habituelle). En CLI, la
  commande à lancer est affichée.
- `v` marque ou démarque une mission comme réussie à la main (utile pour une mission terminée avant l'existence des liens).

### Blocages : aucune boucle, c'est vous qui décidez

Un poste n'est « fait » que s'il l'affirme **et** que rien ne l'a empêché de finir : chaque appel de travail ou de
rapport rend un statut structuré (`done` / `blocked` / `failed`) avec ses blocages (`tests`, `permission`,
`prerequisite`, `bug`, `scope`, `other`) et ce qu'il faudrait pour les lever. Un poste bloqué n'est plus ✔ mais ⚠.

essaim ne relance **jamais** un poste de lui-même : il n'y a plus de tours de correction ni de remontée d'échelon en
échelon.

- La consigne d'une feuille : au plus deux tentatives sur la même erreur, puis `ask_human` avec le diagnostic. Jamais
  de contournement.
- Un poste dont le travail finit `blocked` ou `failed` (délai dépassé compris), ou dont la délégation échoue, produit une demande
  « poste bloqué » avec trois réponses : **relancer ce poste** (une réponse libre le relance avec votre texte, dans sa
  session, contexte conservé), **continuer sans lui**, ou **arrêter la mission**. Chaque nouvel essai est donc une
  décision de votre part.
- Lots de réalisation : si un lot n'aboutit pas, les suivants ne démarrent pas, sauf si vous avez répondu de
  continuer sans le poste en cause. Un plan qui n'aboutit pas ne lance pas la réalisation.

Le compte rendu affiche « ⚠ bloquée » et la liste des blocages.

#### Sans humain (`--yes`)

Avec `--yes`, ou quand l'entrée standard est fermée, personne ne répond :

- une `question` reçoit l'option recommandée par l'agent ; l'hypothèse est notée dans `decisions.md` (« no human
  available ») ;
- une `action`, un blocage ou une revue non acceptée restent sans réponse : le poste finit `blocked`, la mission sort
  en `1` avec la liste de ce qui est demandé ;
- une demande d'autorisation (profil protégé) est refusée, et l'agent en est informé.


**Point de contrôle** (`checkpoint`, défaut `true`). Quand il est actif, le plan du boss est affiché avant toute
réalisation et demande validation. En ligne de commande, sans `--yes`, essaim pose la question
« Lancer la réalisation (les postes vont modifier le projet) ? [o/N/e = modifier le plan avec le boss] » ; `o`, `oui`, `y` ou `yes`
lance la réalisation, toute autre réponse arrête la mission sur le plan.
Avec `checkpoint = false`, il n'y a ni plan à valider ni invite : la réalisation démarre dès que le plan est écrit.

### Modèles et assistants légers

Le modèle d'un poste est celui de son agent, ou son champ `model` s'il est défini. Les postes qui **produisent le travail**
(feuilles : développeurs, analystes) et ceux qui **arbitrent** (managers, CEO) sont en `sonnet` par défaut (`opus` pour
l'architecture difficile) : un travail raté vous coûte une demande et une relance, plus cher que le surcoût du modèle.

**Le modèle se choisit selon la tâche.** À chaque délégation, le CEO (et chaque manager) peut fixer un `model`
(`haiku` / `sonnet` / `opus`) pour le poste qu'il mobilise, selon ce que la tâche exige vraiment : `haiku` pour le
mécanique ou le simple, `sonnet` pour de l'analyse ou du code qui demande du jugement, `opus` pour l'architecture
difficile. Sur une feuille, c'est son modèle ; sur un manager, c'est le défaut des feuilles de sa branche, que les
niveaux inférieurs peuvent affiner. Le choix s'affiche dans le journal (`Backend Engineer · builds ⚒ [sonnet]`). Un
`model` écrit dans le `.toml` d'un poste est **épinglé** : aucun délégant ne le change ; les managers gardent leur modèle.

Pour économiser tokens et temps, un poste peut déclarer `helpers = "haiku"` : il dispose alors d'un sous-agent
(`essaim-assistant`) au modèle moindre, pour les tâches **mécaniques et entièrement spécifiées** (lister ou compter des
fichiers, lancer une commande et rendre sa sortie brute, extraire des lignes). L'assistant ne décide rien et n'a pas
les outils d'écriture de fichiers (`Read`, `Grep`, `Glob`, `Bash`) ; le poste reste responsable et vérifie ce qu'il reçoit.
Ses tokens comptent dans le coût, mais pas dans `max_calls`.

### Un boss qui coordonne et vérifie, sans relancer de lui-même

Les supérieurs (boss, managers) coordonnent : leur fiche de poste leur dit de découper, déléguer et synthétiser, et de
laisser la réalisation aux feuilles. Ils ont les mêmes outils que les autres et s'en servent pour **lire le projet et
vérifier** (lancer les tests, relire un diff) plutôt que de croire les rapports.

Après les rapports de sa branche, le boss fait une **revue** : le résultat atteint-il l'objectif et les critères du
brief ? La revue **constate** (`accepted`, ou la liste des écarts) et ne relance rien. Si elle n'est pas acceptée, les
écarts vous sont présentés avec trois réponses : **relancer une descente corrective** limitée à ces écarts (votre texte
libre s'y ajoute comme consigne), **accepter le résultat tel quel**, ou **arrêter**. La revue coûte un appel, déjà
compté dans l'estimation ; `review = false` dans le TOML la supprime (`max_review_rounds = 0`, l'ancien champ, vaut
la même chose).

Livrables : `.essaim/missions/<id>/{brief,conception,rapport}.md` et le dossier `notes/`. `rapport.md` n'existe que si
la réalisation a été lancée et s'est terminée. Garde-fous dans le TOML : `max_calls` (plafond d'appels `claude` par
mission, défaut 40), `max_parallel` (défaut 2), `checkpoint` (défaut `true`), `review` (défaut `true`),
`commit_steps` (défaut `false` : `true` demande aux feuilles de clore chaque étape par un commit). L'estimation
d'appels est affichée avant le lancement : 1 pour le plan, 2 par poste interne, 1 par feuille, 1 pour la revue.

Les anciens champs `allow_bash` (poste) et `max_fix_rounds` (entreprise) sont encore lus, pour ne pas casser vos
fichiers, et ignorés.

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
helpers = "haiku"             # assistants légers (modèle moindre) pour les tâches mécaniques de ce poste
```

`essaim init` crée une entreprise d'exemple complète (9 postes) dans `~/.claude/companies/`.

## Jetons : coût pondéré, et non volume brut

Le chiffre affiché partout (onglet Run, tableau de bord, compte rendu, `run.json`) est un **coût pondéré**, en
équivalents jetons d'entrée :

| nature | poids | pourquoi |
|---|---|---|
| entrée neuve | ×1 | référence |
| **sortie** | **×5** | son prix réel ; deuxième poste de coût d'une feuille qui écrit du code |
| **cache lu** | **×0,1** | son prix réel, et de loin la plus grosse quantité |
| cache écrit | ×1,25 | son prix réel |

Pourquoi ce n'est pas la simple somme des quatre : un poste qui enchaîne les tours d'outils renvoie tout son
contexte à chaque tour, et ce contexte est servi par le cache de prompt. Additionner les quatre à parité donne donc
un chiffre spectaculaire — « plus d'un million de jetons en une minute » — qui vaut plusieurs fois le coût réel. Le
volume brut reste affiché **en second** (« volume … dont … lus en cache »), et le tableau de bord a une case pour le
mettre au premier plan.

### D'où vient vraiment le coût d'une feuille

Mesuré sur une session réelle (découpe d'un fichier de 1 533 lignes, modèle `haiku`) :

| | |
|---|---|
| requêtes au modèle | **97** |
| contexte : première → dernière requête | 9 k → **252 k** (médiane 164 k) |
| somme des contextes facturés | **15,7 M** |
| résultats d'outils, **en tout** | **42 846** jetons (médiane 49, max 6 196) |
| sortie produite | 122 342 jetons (60 `Edit`, 5 `Write`) |

Une requête facture **tout son contexte, à chaque fois**. Le coût d'une session vaut donc *nombre de tours ×
contexte moyen*, et comme le contexte grossit à mesure que l'agent travaille, c'est **quadratique dans la longueur
de la session** : deux fois plus longue coûte environ quatre fois plus.

Ce n'est pas un gaspillage qu'on pourrait supprimer. Les résultats d'outils ne pèsent que 43 k au total — l'agent
passait toutes ses sorties par `grep`/`tail`. Ce qui remplit le contexte, c'est **le travail de l'agent lui-même** :
88,7 % de sa sortie sont des **arguments d'outils** (les `Edit` à 47,5 %, les `Write` à 29,8 %), c'est-à-dire le code
qu'il écrit ; 11,3 % seulement sont de la prose. Un `Edit` coûte d'ailleurs **double**, puisqu'il transporte
`old_string` *et* `new_string` : déplacer du code le fait payer deux à trois fois.

Le vrai levier est donc de **raccourcir les sessions** : la même tâche découpée en 5 fois 20 tours coûte ~3,5 M au
lieu de 15,7 M, soit 4 à 5 fois moins. C'est ce que font les étapes successives, ci-dessous.

### Une affectation = une étape, une session neuve

Quand un supérieur confie **plusieurs affectations au même subordonné**, ce ne sont pas une tâche fusionnée mais des
**étapes successives** : le poste les exécute l'une après l'autre, et **chaque étape est un appel `claude` neuf**.
Son contexte ne porte donc que l'étape en cours, au lieu d'accumuler tout le travail précédent.

- Chaque étape est un nœud distinct dans le journal et dans le coût (`dev1/realisation/work@1`, `@2`…).
- Chaque étape sait où elle en est (« Step 2 of 3 ») et reçoit la consigne de **ne pas refaire ni auditer** les
  précédentes, ni commencer les suivantes — sans quoi une session neuve relit le projet pour « vérifier ».
- Une étape qui n'aboutit pas **arrête les suivantes** : on ne construit pas sur une base cassée.
- Une étape qui n'aboutit pas vous est soumise (relancer, continuer sans elle, arrêter).
- Avec `commit_steps = true` dans le TOML de l'entreprise, un poste ⚒ doit **clore chaque étape par un commit** qui
  ne couvre qu'elle.

Le prompt de délégation le dit au supérieur : « one assignment = ONE step, small enough to end in a single coherent
commit ». Et le prompt de réalisation ajoute une règle de coût : pour **déplacer** du code, `git mv` puis des
modifications ciblées — **jamais** réécrire un fichier entier, puisqu'un `Edit` transporte l'ancien et le nouveau
texte, et que tout cela est relu à chaque tour suivant.

> **Conséquence sur `max_calls`** : une feuille qui découpe son travail en trois étapes fait trois appels au lieu
> d'un. L'estimation affichée avant lancement ne compte qu'un appel par feuille, comme avant : elle reste un
> majorant du *flux de base*, pas des étapes. Si une mission s'arrête sur « call cap reached », relevez `max_calls`
> (défaut 40).

### Un poste ne voit que la spec de son supérieur

Le brief de la mission ne va **qu'au boss**. C'est l'intérêt de la hiérarchie : le boss digère la demande et n'en
descend que la part utile, chaque supérieur affinant pour ses subordonnés. Un poste reçoit donc pour seule
référence la tâche écrite par son supérieur, et sa fiche de poste le lui dit (« your supervisor's task is your whole
spec. Do not widen it »). Symétriquement, chaque délégant est averti que ce qu'il écrit est tout ce que son
subordonné aura, et qu'une tâche courte et complète est ce qui garde le travail focalisé.

Deux effets : le contexte d'un poste reste petit (le brief était relu à **chaque tour d'outil**, pas une seule
fois), et un poste ne peut plus élargir son périmètre en s'autorisant lui-même au nom de l'objectif général.

Mesure de la refonte « plan du boss, puis une seule descente » sur le harnais de coût (`tests/mesure/`, faux `claude`,
jetons d'entrée composés par essaim ; `avant.json` → `apres.json`) :

| mission | appels avant → après | jetons d'entrée avant → après | |
|---|---|---|---|
| M3 — 7 postes, plan + réalisation | 21 → **13** | 23 329 → **16 570** | même travail, −38 % d'appels |
| M1 — 3 postes | 6 → 7 | 5 540 → 8 259 | avant : conception seule ; après : plan **et** réalisation |
| M2 — 7 postes | 12 → 13 | 13 277 → 16 499 | idem |
| M4 — 7 postes, revue rejetée | 21 → 24 | 43 617 → 53 249 | la relance demandée par l'humain refait une descente complète |

Seule M3 compare le même travail des deux côtés. M1 et M2 n'avaient pas de poste ⚒ : l'ancien moteur s'arrêtait au
plan, le nouveau fait aussi la réalisation. Le prix à payer est une fiche de poste plus longue (règles de demande à
l'humain, notes partagées) : 150 à 250 jetons d'entrée de plus par appel sur ce harnais.

### Garde-fous contre un poste qui s'emballe

Une feuille est une boucle agentique libre : elle peut enchaîner des dizaines de tours, son contexte grossissant à
chaque lecture de fichier. Trois garde-fous :

- **`node_timeout_s`** (entreprise, défaut **900** s) : délai maximal d'un appel. Au-delà, l'appel est arrêté.
- **`max_budget_usd`** (entreprise, surchargeable par poste ; absent par défaut) : plafond de dépense estimée par
  appel. La dépense des assistants légers y compte. C'est une estimation côté client : laissez de la marge.
- **Environnement isolé** : chaque appel part avec `--strict-mcp-config` et `--setting-sources project,local`, donc
  sans réglages utilisateur et avec un seul serveur MCP, celui d'essaim (`ask_human`, et `permission` en profil protégé). Sans cela, chaque poste héritait des plugins, skills et du `model` de la
  machine — prompt système mesuré à 9,3 k là où essaim n'en produit que ~1,5 k, **relu à chaque tour**. Vos agents
  ne sont pas concernés : essaim lit lui-même leurs `.md` et les injecte par `--agents`.

Enfin, quand la sortie structurée d'un appel est inexploitable, le second essai **reprend la session** (`--resume`)
au lieu de repartir à neuf : on redemande le format, pas le travail. Un poste à 250 k de contexte qui rate sa
validation doublait sa facture.

## Tableau de bord de consommation

`essaim conso` (ou `F9` dans la TUI) sert une page web locale qui répond à « où partent mes jetons ? ».

```sh
essaim conso              # ouvre le navigateur sur http://127.0.0.1:8787/?k=…
essaim conso --no-open    # affiche seulement l'URL
essaim conso --port 9000  # port imposé (sinon 8787, puis le suivant s'il est pris)
```

La page a deux vues, choisies en haut à gauche : **Consommation** (tout ce qui suit) et **Mission**, le
suivi du run. Quand une mission tourne, l'onglet *Mission* porte un point vert et un bandeau cliquable
apparaît en tête de la vue Consommation (« voir le suivi → »).

La vue **Mission** montre : l'organigramme de
l'entreprise, poste par poste, avec l'état de chacun (en attente, en cours, terminé, bloqué, échec), ses
jetons qui montent en direct, le total de la mission, le nombre d'appels sur le plafond et la durée. La
page se rafraîchit alors toutes les 2 secondes au lieu de 5. Ça marche pour un run lancé depuis la TUI
comme depuis `essaim company run`. Quand la mission est finie, la carte reste en « Dernière mission ». Sans run suivi, la vue explique comment en lancer un.

La page montre aussi, pour la fenêtre choisie (6 h, 24 h, 7 j, 30 j, tout) :

- les jetons dans le temps, par nature (**entrée neuve**, **sortie**, **cache lu**) — une case décompte le
  cache lu, qui pèse souvent 90 % du total et écrase tout le reste ;
- **qui consomme** : les sessions les plus coûteuses, avec leur dossier de travail, leur modèle et, pour
  les appels lancés par essaim, le **poste** correspondant ;
- les **terminaux `claude` ouverts** (interactifs et agents de fond), leur dossier et ce que chacun a
  déjà consommé ;
- la répartition par dossier de travail et par modèle, la table de toutes les sessions (triable), et les
  postes et missions d'essaim du projet courant.

D'où viennent les chiffres : des transcriptions de Claude Code (`~/.claude/projects/<dossier>/<session>.jsonl`),
donc **toute** votre consommation, essaim ou non ; de `claude agents --json --all` pour les sessions
vivantes ; de `.essaim/runs/<id>/live.json` pour la mission en cours (écrit au plus une fois par seconde
pendant le run) et de `.essaim/runs/` pour l'historique des postes. Un message recopié par une reprise de session (`--resume`)
n'est compté qu'une fois.

Confidentialité : la page est servie sur `127.0.0.1` uniquement, l'URL porte une clé tirée au hasard à
chaque lancement, l'en-tête `Host` doit désigner la boucle locale et le serveur ne sait que lire. Seuls
des compteurs, des chemins de dossiers et des titres de session circulent — jamais le contenu des
conversations.

## Formats

**Agent** : `<nom>.md` (le format standard de Claude Code : frontmatter `name`, `description`,
`model`, `tools` séparés par des virgules, puis le prompt). Les agents sont cherchés dans
`<projet>/.claude/agents/`, puis dans `~/.claude/agents/` ; le projet prime en cas de doublon, et les
créations vont dans `~/.claude/agents/` (voir
[Vos agents et vos entreprises, partout](#vos-agents-et-vos-entreprises-partout)). Un fichier qui ne se lit
pas est ignoré en silence par `agents` et `company run` ; la TUI le signale après une création avec l'assistant.

**Entreprise** : `<nom>.toml`, dans `~/.claude/companies/` ou `<projet>/.claude/companies/` (mêmes règles). Champs du poste : `id` (lettres, chiffres, `-`, `_`), `title`,
`agent` (obligatoires), `reports_to` (absent = le boss, un seul par entreprise), `expertise` (facultatif : domaine
présenté au supérieur pour déléguer ; absent, seul l'intitulé est présenté), `builds`, `model`, `helpers`, `max_budget_usd`.

Les runs lancés depuis la TUI sont enregistrés dans `.essaim/runs/<id>/run.json` (ajoutez `.essaim/` à `.gitignore`).

**Réglages** : `settings.toml` est stocké dans le dossier de configuration de la plateforme (par exemple `~/.config/essaim/` sous Linux).
La variable `ESSAIM_CONFIG_DIR` remplace ce dossier.

```toml
lang = "fr"
profile = "protected"            # ou "dedicated"
# Profil protégé seulement ; absents = les listes par défaut ci-dessous.
sandbox_read = ["~/.cargo", "~/.rustup"]          # dossiers hors projet lisibles par les commandes
sandbox_write = ["~/.cargo/registry"]             # dossiers hors projet inscriptibles
sandbox_domains = ["crates.io", "*.crates.io"]    # domaines joignables sans demande
```

Listes par défaut du profil protégé : lecture de `~/.cargo`, `~/.rustup`, `~/.nvm`, `~/.npm`, `~/.volta`, `~/.bun`,
`~/.deno`, `~/.pyenv`, `~/.local/bin`, `~/.local/share/uv`, `~/go` (sans eux, `cargo`, `node` ou `python` installés
dans le dossier personnel ne démarrent pas) ; écriture dans `~/.cargo/registry`, `~/.cargo/git`, `~/.npm/_cacache` ;
domaines `crates.io`, `*.crates.io`, `registry.npmjs.org`, `pypi.org`, `files.pythonhosted.org`.

## Limites connues

- **Code de sortie 0 sur une réalisation refusée.** Au point de contrôle, un refus met `outcome.ok = conception.ok`
  (fonction `run_mission`, module `runtime::company_exec`) : si la conception a réussi, la mission est « ok ». `essaim company run`
  ne sort en `1` que si la mission n'est pas `ok` (fonction `main`, branche `company run`). Une réalisation refusée (Échap dans la TUI ; en ligne de commande,
  réponse autre que `o`/`oui`/`y`/`yes`, ou stdin fermé sans `--yes`) sort donc en `0`.
- **Pas de `--version`** : la structure `Cli` (module `main`) ne déclare pas de `version`.
- **Pas de version minimale de `claude`** : aucune vérification de version. Les options utilisées
  (`--bg`, `--agents`, `--json-schema`, `--permission-mode`, `--mcp-config`, `--permission-prompt-tool`, `--settings`,
  `agents --json --all`) doivent exister dans votre version. Le comportement décrit ici a été mesuré sur `claude` 2.1.322.
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
- **Profil appareil dédié** : rien n'empêche matériellement un agent d'agir hors du dossier du projet. essaim ne
  protège plus lui-même `.claude/` et `.git/hooks` en écriture (les périmètres ne sont plus des règles de permission).
- **Profil protégé** : l'isolation est celle du bac à sable de Claude Code et ne couvre que les commandes shell.
- **Demandes en attente** : un poste qui vous attend garde sa place dans `max_parallel`, et quitter essaim annule le run.
- Les entreprises se modifient dans un éditeur externe (`e` dans une zone entrée, option « simple ») ; l'onglet Entreprise propose aussi un formulaire de poste (`a` / `e` / `x`, dans l'organigramme), qui réécrit le TOML et en supprime donc les commentaires.
- La délégation coûte 2 appels par poste interne (délégation puis rapport) : surveille `max_calls` et ton quota.
- Les « coûts » affichés sont notionnels (calculés par `claude`), non facturés avec l'abonnement.
