# Fixtures de performance (étape A)

Utilisées par `tests/perf_baseline.rs`. Aucun fichier volumineux n'est versionné : les volumes sont
**générés de façon déterministe** à partir de ces échantillons (même entrée, même flux, à chaque exécution).

| Fichier | Contenu | Usage |
|---|---|---|
| `stream-sample.jsonl` | 6 lignes NDJSON réalistes (`system/init`, `assistant` texte et `tool_use`, `user/tool_result`, `rate_limit_event`, `result`) | P1 : le flux de N lignes est construit en cyclant ces lignes |

Volumes générés dans `perf_baseline.rs` :

- P1 : 5 000 lignes NDJSON (cycle de l'échantillon) ;
- P2 : ≈ 1 Mio de texte avec secrets répétés (`ANTHROPIC_API_KEY=…`, `sk-ant-…`) ;
- P3 : 400 périmètres `src/mK/**` + `docs/fI.md` ;
- P4 : 20 écritures atomiques d'un contenu de ≈ 4 Kio ;
- P5 : 300 fichiers d'agents `.claude/agents/agent-NNNN.md` dans un répertoire temporaire ;
- P6 : une ligne `assistant` d'environ 1 Mio.

Aucune donnée personnelle ni secret réel : les `sk-ant-…` sont factices.
