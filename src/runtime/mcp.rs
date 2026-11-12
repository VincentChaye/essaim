//! Serveur MCP d'essaim, côté agent : `essaim mcp`, lancé par chaque `claude` d'un poste.
//!
//! MCP (Model Context Protocol) est du JSON-RPC 2.0 ; en transport stdio, un message par ligne. `claude`
//! lance ce processus, lui demande ses outils (`tools/list`) puis les appelle (`tools/call`) pendant que
//! l'agent travaille. Chaque appel est relayé au processus essaim principal par une socket Unix
//! (`runtime::bus`) : c'est lui qui pose la question à l'humain et renvoie sa réponse. L'agent attend la
//! réponse **dans sa session**, sans être arrêté ni relancé.
//!
//! Écrit à la main sur `serde_json` : quatre méthodes suffisent, et aucune dépendance n'entre dans le binaire.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Nom du serveur dans la configuration MCP : les outils s'appellent `mcp__essaim__<outil>`.
pub const SERVER: &str = "essaim";
/// Outil de demande à l'humain, tel que le modèle le voit.
pub const ASK_TOOL: &str = "ask_human";
/// Outil qui reçoit les demandes de permission (`--permission-prompt-tool`) ; le modèle ne le voit pas.
pub const PERMISSION_TOOL: &str = "permission";
/// Variable d'environnement qui porte le jeton de la socket (jamais en argument : `ps` le montrerait).
pub const TOKEN_ENV: &str = "ESSAIM_BUS_TOKEN";

/// Version du protocole annoncée si le client n'en propose pas.
const PROTOCOL: &str = "2025-06-18";

/// Relais d'un appel d'outil : (outil, arguments) → (texte du résultat, erreur ?).
pub type Forward = Arc<dyn Fn(&str, &Value) -> (String, bool) + Send + Sync>;

/// Outils exposés. Les descriptions sont relues à chaque tour de l'agent : elles restent courtes.
pub fn tools(permission: bool) -> Vec<Value> {
    let mut out = vec![json!({
        "name": ASK_TOOL,
        "description": "Ask the human who ordered the mission, at any moment, and wait for the answer. Use it to refine what they want (ambiguity, choice between valid options, priority, success criterion) BEFORE building on a guess, and when only they can act (credential, decision, action outside the project). Not for facts you can look up yourself in the project or on the web. Group related questions in one call.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "kind": {"type": "string", "enum": ["question", "action", "blocked"], "description": "question = refine the task; action = something only the human can do; blocked = you cannot go on without them"},
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 4,
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": {"type": "string", "description": "one short, self-contained question or request"},
                            "options": {"type": "array", "items": {"type": "string"}, "description": "2 to 4 concrete answers to pick from, when it is a choice"},
                            "recommended": {"type": "integer", "description": "index of the option you would pick"}
                        },
                        "required": ["text"]
                    }
                }
            },
            "required": ["kind", "questions"]
        }
    })];
    if permission {
        out.push(json!({
            "name": PERMISSION_TOOL,
            "description": "Permission prompt handler.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": true}
        }));
    }
    out
}

/// Réponse à un message qui n'est pas un appel d'outil. `None` : notification (aucune réponse attendue).
pub fn respond(msg: &Value, tools: &[Value]) -> Option<Value> {
    let id = msg.get("id").filter(|i| !i.is_null())?.clone();
    let ok = |result: Value| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    match msg["method"].as_str().unwrap_or_default() {
        "initialize" => ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": tools})),
        m => Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {m}")}})),
    }
}

fn write_line<W: Write>(out: &Mutex<W>, v: &Value) {
    // Sortie fermée = `claude` est parti : il n'y a plus personne à qui répondre.
    let mut w = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = writeln!(w, "{v}");
    let _ = w.flush();
}

/// Boucle du serveur : lit les messages jusqu'à la fin de l'entrée. Un appel d'outil part dans son propre
/// fil, parce qu'il peut attendre l'humain longtemps et que `claude` en envoie parfois plusieurs à la fois.
/// Rend les fils des appels encore en cours.
pub fn serve<R: BufRead, W: Write + Send + 'static>(input: R, output: W, tools: Vec<Value>, forward: Forward) -> Vec<JoinHandle<()>> {
    let out = Arc::new(Mutex::new(output));
    let mut calls = vec![];
    for line in input.lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { continue };
        if msg["method"] == "tools/call" && msg.get("id").is_some_and(|i| !i.is_null()) {
            let (out, forward, id) = (out.clone(), forward.clone(), msg["id"].clone());
            let name = msg["params"]["name"].as_str().unwrap_or_default().to_string();
            let args = msg["params"]["arguments"].clone();
            calls.push(std::thread::spawn(move || {
                let (text, is_error) = forward(&name, &args);
                write_line(&out, &json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}], "isError": is_error}}));
            }));
        } else if let Some(reply) = respond(&msg, &tools) {
            write_line(&out, &reply);
        }
    }
    calls
}

/// Une requête vers le bus d'essaim : une connexion, une ligne envoyée, une ligne lue (sans délai :
/// l'humain répond quand il veut).
fn ask_bus(socket: &Path, token: &str, node: &str, seat: &str, tool: &str, args: &Value) -> Result<(String, bool)> {
    let mut stream = UnixStream::connect(socket).context("essaim is not reachable")?;
    writeln!(stream, "{}", json!({"token": token, "node": node, "seat": seat, "tool": tool, "args": args}))?;
    stream.flush()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let v: Value = serde_json::from_str(line.trim()).context("essaim closed the connection without answering")?;
    Ok((v["text"].as_str().unwrap_or_default().to_string(), v["is_error"].as_bool().unwrap_or(false)))
}

/// `essaim mcp` : sert stdin/stdout jusqu'à ce que `claude` ferme l'entrée, puis sort sans attendre les
/// appels encore en vol (leur demandeur n'existe plus ; le bus voit la connexion se fermer).
pub fn run(socket: PathBuf, node: String, seat: String, permission: bool) -> Result<()> {
    let token = std::env::var(TOKEN_ENV).unwrap_or_default();
    let forward: Forward = Arc::new(move |tool, args| match ask_bus(&socket, &token, &node, &seat, tool, args) {
        Ok(r) => r,
        // Sans réponse d'essaim, une demande de permission est refusée : jamais d'accord par défaut.
        Err(e) if tool == PERMISSION_TOOL => (json!({"behavior": "deny", "message": format!("{e:#}")}).to_string(), false),
        Err(e) => (format!("{e:#}. No human can be asked right now: for a question, go on with your recommended option and state the assumption in your report; otherwise report status blocked with what is needed."), true),
    });
    drop(serve(std::io::stdin().lock(), std::io::stdout(), tools(permission), forward));
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sortie en mémoire partagée avec le test.
    #[derive(Clone, Default)]
    struct Out(Arc<Mutex<Vec<u8>>>);
    impl Write for Out {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn dialogue(input: &str, permission: bool, forward: Forward) -> Vec<Value> {
        let out = Out::default();
        for h in serve(std::io::Cursor::new(input.to_string()), out.clone(), tools(permission), forward) {
            h.join().unwrap();
        }
        let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn poignee_de_main_et_liste_des_outils() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#,
            "\n",
            "pas du json\n",
        );
        let r = dialogue(input, false, Arc::new(|_, _| (String::new(), false)));
        assert_eq!(r.len(), 4, "une réponse par requête, aucune pour la notification ni pour la ligne illisible : {r:?}");
        assert_eq!(r[0]["result"]["protocolVersion"], "2025-03-26", "version du client reprise");
        assert_eq!(r[0]["result"]["serverInfo"]["name"], "essaim");
        let names: Vec<&str> = r[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["ask_human"], "l'outil de permission n'est listé qu'en profil protégé");
        assert_eq!(r[2]["result"], json!({}));
        assert_eq!(r[3]["error"]["code"], -32601, "méthode inconnue : erreur, pas de silence");
    }

    #[test]
    fn outil_de_permission_liste_en_profil_protege() {
        let r = dialogue("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n", true, Arc::new(|_, _| (String::new(), false)));
        let names: Vec<&str> = r[0]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["ask_human", "permission"]);
    }

    #[test]
    fn appel_d_outil_relaye_et_rendu_en_texte() {
        let seen = Arc::new(Mutex::new(vec![]));
        let s2 = seen.clone();
        let forward: Forward = Arc::new(move |tool, args| {
            s2.lock().unwrap().push((tool.to_string(), args.clone()));
            ("Q: couleur ?\nA: bleu".to_string(), false)
        });
        let input = "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"ask_human\",\"arguments\":{\"kind\":\"question\",\"questions\":[{\"text\":\"couleur ?\"}]}}}\n";
        let r = dialogue(input, false, forward);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["id"], 7);
        assert_eq!(r[0]["result"]["content"][0]["text"], "Q: couleur ?\nA: bleu");
        assert_eq!(r[0]["result"]["isError"], false);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "ask_human");
        assert_eq!(seen[0].1["questions"][0]["text"], "couleur ?", "arguments transmis tels quels");
    }

    /// Un appel qui attend l'humain ne bloque pas les autres messages : `claude` peut envoyer deux appels à la fois.
    #[test]
    fn un_appel_en_attente_ne_bloque_pas_le_suivant() {
        let forward: Forward = Arc::new(|_, args| {
            if args["lent"] == true {
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            (args["nom"].as_str().unwrap_or_default().to_string(), false)
        });
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"ask_human\",\"arguments\":{\"lent\":true,\"nom\":\"lent\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"ask_human\",\"arguments\":{\"nom\":\"rapide\"}}}\n",
        );
        let r = dialogue(input, false, forward);
        let ordre: Vec<i64> = r.iter().map(|x| x["id"].as_i64().unwrap()).collect();
        assert_eq!(ordre, vec![2, 1], "le second appel répond pendant que le premier attend");
    }
}
