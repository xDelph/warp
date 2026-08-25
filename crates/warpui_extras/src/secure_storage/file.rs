//! File-based [`SecureStorage`] for debug builds on platforms that normally
//! use an OS keychain.  Avoids repeated keychain ACL prompts when the app's
//! code signature changes on every rebuild (ad-hoc signing).

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use super::Error;

/// File-based secure storage.  Values are stored in a single file inside
/// the provided directory using a simple `key<TAB>base64value` line format.
/// The file is created with 0600 permissions so only the owner can read it.
pub struct SecureStorage {
    service_name: String,
    storage_dir: PathBuf,
}

impl SecureStorage {
    pub fn new(service_name: &str, storage_dir: PathBuf) -> Self {
        Self {
            service_name: service_name.to_owned(),
            storage_dir,
        }
    }

    fn file_path(&self) -> PathBuf {
        let safe_name = self.service_name.replace(['/', '\\', ':'], "_");
        self.storage_dir.join(format!("{safe_name}.secure_storage"))
    }

    fn read_all(&self) -> Result<HashMap<String, String>, Error> {
        let path = self.file_path();
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let mut contents = String::new();
        OpenOptions::new()
            .read(true)
            .open(&path)
            .map_err(|err| Error::Unknown(anyhow::anyhow!(err)))?
            .read_to_string(&mut contents)
            .map_err(|err| Error::Unknown(anyhow::anyhow!(err)))?;
        let mut map = HashMap::new();
        for line in contents.lines() {
            let Some((key, value_b64)) = line.split_once('\t') else {
                continue;
            };
            let bytes = base64_decode(value_b64);
            let value = String::from_utf8(bytes)
                .map_err(|err| Error::DecodeError(err.utf8_error()))?;
            map.insert(key.to_string(), value);
        }
        Ok(map)
    }

    fn write_all(&self, map: &HashMap<String, String>) -> Result<(), Error> {
        let path = self.file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| Error::Unknown(anyhow::anyhow!(err)))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|err| Error::Unknown(anyhow::anyhow!(err)))?;
        for (key, value) in map {
            writeln!(file, "{key}\t{}", base64_encode(value.as_bytes()))
                .map_err(|err| Error::Unknown(anyhow::anyhow!(err)))?;
        }
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        Ok(())
    }
}

impl super::SecureStorage for SecureStorage {
    fn write_value(&self, key: &str, value: &str) -> Result<(), Error> {
        let mut map = self.read_all()?;
        map.insert(key.to_string(), value.to_string());
        self.write_all(&map)
    }

    fn read_value(&self, key: &str) -> Result<String, Error> {
        let map = self.read_all()?;
        map.get(key).cloned().ok_or(Error::NotFound)
    }

    fn remove_value(&self, key: &str) -> Result<(), Error> {
        let mut map = self.read_all()?;
        map.remove(key);
        self.write_all(&map)
    }
}

/// Minimal base64 encoder (standard alphabet, no padding needed for our use).
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Minimal base64 decoder.
fn base64_decode(s: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    fn val(c: u8) -> Option<u32> {
        ALPHABET.iter().position(|&a| a == c).map(|p| p as u32)
    }
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=' && c != b'\n' && c != b'\r').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let v0 = val(chunk[0]).unwrap_or(0);
        let v1 = val(chunk.get(1).copied().unwrap_or(0)).unwrap_or(0);
        let v2 = chunk.get(2).and_then(|c| val(*c)).unwrap_or(0);
        let v3 = chunk.get(3).and_then(|c| val(*c)).unwrap_or(0);
        let quad = (v0 << 18) | (v1 << 12) | (v2 << 6) | v3;
        out.push((quad >> 16) as u8);
        if chunk.len() > 2 && chunk[2] != b'=' {
            out.push((quad >> 8) as u8);
        }
        if chunk.len() > 3 && chunk[3] != b'=' {
            out.push(quad as u8);
        }
    }
    out
}
