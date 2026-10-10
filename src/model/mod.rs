use std::io::Read;
use std::path::Path;

pub mod agent;
pub mod company;
pub mod ident;
pub mod roles;

/// Taille maximale d'un fichier de configuration du projet lu (agent, entreprise) : 1 Mio.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// Lit un fichier de configuration du projet, borné à `MAX_CONFIG_BYTES` : un fichier démesuré (YAML, TOML)
/// ne doit pas saturer la mémoire. La borne s'applique à la lecture elle-même, pas seulement à la taille annoncée.
pub fn read_config(path: &Path) -> std::io::Result<String> {
    let mut s = String::new();
    std::fs::File::open(path)?.take(MAX_CONFIG_BYTES + 1).read_to_string(&mut s)?;
    if s.len() as u64 > MAX_CONFIG_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("fichier trop volumineux (plus de 1 Mio) : {}", path.display())));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_bornee_a_1_mio() {
        let p = std::env::temp_dir().join(format!("essaim-cfg-{}.toml", uuid::Uuid::new_v4()));
        std::fs::write(&p, "x".repeat(MAX_CONFIG_BYTES as usize + 10)).unwrap();
        assert!(read_config(&p).is_err(), "au-delà de 1 Mio : refusé");
        std::fs::write(&p, "name = \"ok\"").unwrap();
        assert_eq!(read_config(&p).unwrap(), "name = \"ok\"");
        let _ = std::fs::remove_file(&p);
    }
}
