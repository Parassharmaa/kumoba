use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::path::Path;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewClaims {
    pub sandbox_id: String,
    pub port: u16,
    pub exp_ms: u64,
}

pub struct PreviewKey(Vec<u8>);

impl PreviewKey {
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() == 32 => return Ok(Self(bytes)),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut key = vec![0u8; 32];
        getrandom(&mut key)?;
        let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
        std::io::Write::write_all(&mut file, &key)?;
        std::fs::set_permissions(
            file.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(Self(key))
    }

    #[cfg(test)]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    fn mac(&self, payload: &str) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.0).expect("HMAC accepts any key length");
        mac.update(payload.as_bytes());
        mac
    }

    pub fn sign(&self, claims: &PreviewClaims) -> String {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims serialize"));
        let signature = URL_SAFE_NO_PAD.encode(self.mac(&payload).finalize().into_bytes());
        format!("{payload}.{signature}")
    }

    pub fn verify(&self, token: &str, now_ms: u64) -> Option<PreviewClaims> {
        let (payload, signature) = token.split_once('.')?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        self.mac(payload).verify_slice(&signature).ok()?;
        let claims: PreviewClaims =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        (claims.exp_ms > now_ms).then_some(claims)
    }
}

fn getrandom(buffer: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(exp_ms: u64) -> PreviewClaims {
        PreviewClaims {
            sandbox_id: "s1".into(),
            port: 6081,
            exp_ms,
        }
    }

    #[test]
    fn signed_token_round_trips_until_expiry() {
        let key = PreviewKey::from_bytes(&[7; 32]);
        let token = key.sign(&claims(2_000));
        assert_eq!(key.verify(&token, 1_000), Some(claims(2_000)));
        assert_eq!(key.verify(&token, 2_000), None);
    }

    #[test]
    fn tampered_or_foreign_tokens_are_rejected() {
        let key = PreviewKey::from_bytes(&[7; 32]);
        let token = key.sign(&claims(2_000));
        let (payload, signature) = token.split_once('.').unwrap();
        let forged = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&PreviewClaims {
                port: 22,
                ..claims(2_000)
            })
            .unwrap(),
        );
        assert_eq!(key.verify(&format!("{forged}.{signature}"), 1_000), None);
        assert_eq!(PreviewKey::from_bytes(&[8; 32]).verify(&token, 1_000), None);
        assert_eq!(key.verify(payload, 1_000), None);
    }
}
