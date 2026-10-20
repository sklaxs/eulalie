use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use anyhow::Result;

/// Hache un mot de passe avec Argon2id (paramètres par défaut du crate :
/// m=19456 KiB, t=2, p=1 — conforme aux recommandations OWASP).
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash: {e}"))?;
    Ok(hash.to_string())
}

/// Vérifie un mot de passe contre un hash Argon2id stocké.
pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_roundtrip() {
        let h = hash_password("S3cret!mot").unwrap();
        assert!(verify_password("S3cret!mot", &h));
        assert!(!verify_password("mauvais", &h));
    }
}
