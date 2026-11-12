# Harnais de mesure du coût agentique

Méthode unique, figée avant toute modification du contenu envoyé au modèle. À réutiliser telle quelle pour la mesure « après ».

## Principe
- Le binaire `essaim` est lancé (`company run … --yes`) avec `ESSAIM_CLAUDE_BIN` pointant vers un faux `claude` (script shell,
  `tests/common/mod.rs`). Aucun appel réel, aucun réseau, aucune horloge dans les résultats.
- Le faux enregistre chaque appel (prompt, fiche de poste `--append-system-prompt`, schéma `--json-schema`, `--agents`, modèle, sortie).
- `max_parallel = 1` dans toutes les entreprises : l'ordre des appels est déterministe.

## Formule des jetons synthétiques (figée)
- « octets » = longueur UTF-8 des chaînes (`str::len`) ; jetons ≈ octets / 4 : approximation synthétique, **pas** le tokenizer du modèle. Elle ne sert qu'à comparer avant/après avec la même méthode.
- jetons d'entrée d'un appel = ceil(octets(prompt + fiche de poste + schéma + sous-agents) / 4)
- jetons de sortie = ceil(octets(sortie structurée) / 4)
- cache = ceil(préfixe commun en octets de la fiche de poste avec celle de l'appel précédent / 4), borné aux jetons d'entrée de l'appel ; premier appel : préfixe avec la chaîne vide (cache 0)
- part de cache = cache / jetons d'entrée (total mission)
- appels = nombre de lancements de `claude` (comptés dans le faux, un fichier `calls/<n>` par lancement)

## Durée (hors comparaison)
- durée = temps mural de `company run` (horloge du harnais, processus `essaim` entier, faux `claude` compris).
- **Méthode retenue : min de 2 exécutions** par mission (`min(d1, d2)` dans `campagne()`), écrite dans `avant.json` (`duree_methode`).
- La durée est **indicative et hors comparaison** : aucun test ni décision ne la compare ; seuls les compteurs (appels, jetons, cache) sont comparés, et doivent être identiques entre les deux exécutions.
- Médiane de 5 non retenue : elle obligerait à réécrire `avant.json` (baseline figée) ; à réserver à une décision explicite.

## Missions types
| id | entreprise | brief | mode du faux |
|----|------------|-------|--------------|
| M1 | 3 postes | court | nominal |
| M2 | 7 postes | détaillé (critères) | nominal |
| M3 | 7 postes dont 2 de réalisation | détaillé | nominal |
| M4 | 7 postes, revue rejetée, relance demandée par l'humain, sorties volumineuses | détaillé | review_redo,big |

`avant.json` a été mesuré sur l'ancien moteur (conception par toute la hiérarchie, puis réalisation des seuls postes ⚒ :
M1 et M2, sans poste ⚒, s'arrêtaient au plan). `apres.json` est mesuré sur le moteur actuel : un appel du boss pour le
plan, puis une descente pour toute entreprise. Seule M3 compare donc le même travail (21 → 13 appels).
M4 n'utilise pas `--yes` : le harnais répond `1` (« relancer une descente corrective ») à la revue rejetée.

## Relance
```
cargo test --test mesure -- --ignored --nocapture mesure_ecrire_avant   # écrit tests/mesure/avant.json
cargo test --test mesure -- --ignored --nocapture mesure_ecrire_apres   # écrit tests/mesure/apres.json
```
`cargo test` seul exécute `mesure_reproductible` (deux exécutions par mission → compteurs identiques, sinon échec ; durée = min).
Les deux `mesure_ecrire_*` sont `#[ignore]` : ils ne tournent que sur demande explicite. Relancer `mesure_ecrire_avant` réécrit `avant.json` : ne le faire que pour une décision explicite (baseline figée).
