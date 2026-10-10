//! Copie de texte vers le presse-papiers : outil système si présent, sinon séquence OSC 52 (terminaux modernes, SSH).

use std::io::Write;
use std::process::{Command, Stdio};

/// Copie `text` ; renvoie la méthode utilisée (pour le message de statut).
pub fn copy(text: &str) -> Result<&'static str, String> {
    for (bin, args) in [("pbcopy", &[][..]), ("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"]), ("clip.exe", &[])] {
        let Ok(mut child) = Command::new(bin).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { continue };
        let written = child.stdin.take().map(|mut i| i.write_all(text.as_bytes()).is_ok()).unwrap_or(false);
        if child.wait().map(|s| s.success()).unwrap_or(false) && written {
            return Ok(bin);
        }
    }
    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes()).and_then(|_| out.flush()).map_err(|e| e.to_string())?;
    Ok("OSC 52")
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_standard() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64("é".as_bytes()), "w6k=");
    }
}
