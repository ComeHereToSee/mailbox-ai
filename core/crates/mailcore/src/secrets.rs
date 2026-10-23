//! Encrypted credential store.
//!
//! Passwords and OAuth tokens live in `secrets.bin`, encrypted with ChaCha20-Poly1305.
//! The 256-bit master key is kept in the macOS Keychain (one item, so an ad-hoc signed
//! update triggers at most one "allow" prompt); other platforms use `master.key` (0600).

use crate::error::{Error, Result};
use crate::paths::{Paths, write_private};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use parking_lot::Mutex;
use rand::RngCore;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const KEYCHAIN_SERVICE: &str = "com.comeheretosee.mailbox.ai";
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const KEYCHAIN_ACCOUNT: &str = "master-key";

const BLOB_MAGIC: &[u8] = b"MBX1";

pub struct Secrets {
    path: PathBuf,
    cipher: ChaCha20Poly1305,
    data: Mutex<BTreeMap<String, String>>,
}

fn use_keychain() -> bool {
    cfg!(target_os = "macos") && std::env::var("MAILBOX_KEY_FILE").is_err()
}

#[cfg(target_os = "macos")]
fn keychain_get() -> Result<Option<Vec<u8>>> {
    use security_framework::passwords::get_generic_password;
    match get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
        Ok(k) => Ok(Some(k)),
        Err(e) if e.code() == -25300 => Ok(None), // errSecItemNotFound
        Err(e) => Err(Error::Other(format!("无法读取钥匙串: {e}"))),
    }
}

#[cfg(target_os = "macos")]
fn keychain_set(key: &[u8]) -> Result<()> {
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, key)
        .map_err(|e| Error::Other(format!("无法写入钥匙串: {e}")))
}

#[cfg(not(target_os = "macos"))]
fn keychain_get() -> Result<Option<Vec<u8>>> {
    Ok(None)
}

#[cfg(not(target_os = "macos"))]
fn keychain_set(_key: &[u8]) -> Result<()> {
    Ok(())
}

fn load_master_key(paths: &Paths) -> Result<[u8; 32]> {
    let file = paths.master_key_file();
    let existing = if use_keychain() {
        match keychain_get()? {
            Some(k) => Some(k),
            None => std::fs::read(&file).ok(),
        }
    } else {
        std::fs::read(&file).ok()
    };
    if let Some(k) = existing {
        if k.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&k);
            if use_keychain() && file.exists() {
                keychain_set(&key)?;
                let _ = std::fs::remove_file(&file);
            }
            return Ok(key);
        }
    }
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    if use_keychain() {
        keychain_set(&key)?;
    } else {
        write_private(&file, &key)?;
    }
    Ok(key)
}

impl Secrets {
    pub fn open(paths: &Paths) -> Result<Secrets> {
        let key = load_master_key(paths)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let path = paths.secrets();
        let data = match std::fs::read(&path) {
            Ok(bytes) if bytes.len() > 12 => {
                let (nonce, ct) = bytes.split_at(12);
                let plain = cipher
                    .decrypt(Nonce::from_slice(nonce), ct)
                    .map_err(|_| Error::Other("凭据文件无法解密（主密钥不匹配）".into()))?;
                serde_json::from_slice(&plain)?
            }
            _ => BTreeMap::new(),
        };
        Ok(Secrets { path, cipher, data: Mutex::new(data) })
    }

    fn persist(&self, data: &BTreeMap<String, String>) -> Result<()> {
        let plain = serde_json::to_vec(data)?;
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plain.as_ref())
            .map_err(|_| Error::Other("加密失败".into()))?;
        let mut out = nonce.to_vec();
        out.extend(ct);
        write_private(&self.path, &out)?;
        Ok(())
    }

    /// Encrypts data with the master key (`MBX1` + nonce + ciphertext).
    pub fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = self.cipher.encrypt(Nonce::from_slice(&nonce), plain).map_err(|_| Error::Other("加密失败".into()))?;
        let mut out = Vec::with_capacity(ct.len() + 16);
        out.extend_from_slice(BLOB_MAGIC);
        out.extend_from_slice(&nonce);
        out.extend(ct);
        Ok(out)
    }

    /// Reverses [`Secrets::seal`]; data without the marker is returned as is (older files).
    pub fn open_sealed(&self, data: Vec<u8>) -> Result<Vec<u8>> {
        if !data.starts_with(BLOB_MAGIC) || data.len() < BLOB_MAGIC.len() + 12 {
            return Ok(data);
        }
        let (nonce, ct) = data[BLOB_MAGIC.len()..].split_at(12);
        self.cipher.decrypt(Nonce::from_slice(nonce), ct).map_err(|_| Error::Other("本地邮件无法解密".into()))
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.data.lock().get(key).cloned()
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        let mut data = self.data.lock();
        data.insert(key.to_string(), value.to_string());
        self.persist(&data)
    }

    pub fn remove(&self, key: &str) -> Result<()> {
        let mut data = self.data.lock();
        if data.remove(key).is_some() {
            self.persist(&data)?;
        }
        Ok(())
    }

    pub fn remove_prefix(&self, prefix: &str) -> Result<()> {
        let mut data = self.data.lock();
        let before = data.len();
        data.retain(|k, _| !k.starts_with(prefix));
        if data.len() != before {
            self.persist(&data)?;
        }
        Ok(())
    }
}

pub fn password_key(account_id: &str) -> String {
    format!("account:{account_id}:password")
}

pub fn smtp_password_key(account_id: &str) -> String {
    format!("account:{account_id}:smtp_password")
}

pub fn oauth_key(account_id: &str) -> String {
    format!("account:{account_id}:oauth")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_reopen() {
        unsafe { std::env::set_var("MAILBOX_KEY_FILE", "1") };
        let dir = crate::store::tempfile_dir::TempDir::new();
        let paths = Paths::new(dir.path()).unwrap();
        let s = Secrets::open(&paths).unwrap();
        s.set("account:a:password", "授权码123").unwrap();
        s.set("account:b:password", "x").unwrap();
        drop(s);
        let raw = std::fs::read(paths.secrets()).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("授权码"));
        let s = Secrets::open(&paths).unwrap();
        assert_eq!(s.get("account:a:password").as_deref(), Some("授权码123"));
        s.remove_prefix("account:a:").unwrap();
        assert!(s.get("account:a:password").is_none());
        assert!(s.get("account:b:password").is_some());
        let sealed = s.seal(b"Subject: hi\r\n\r\nbody").unwrap();
        assert!(sealed.starts_with(b"MBX1"));
        assert_eq!(s.open_sealed(sealed).unwrap(), b"Subject: hi\r\n\r\nbody");
        assert_eq!(s.open_sealed(b"plain".to_vec()).unwrap(), b"plain");
    }
}
