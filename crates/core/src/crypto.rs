use std::io::{self, ErrorKind};

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const PBKDF2_ROUNDS: u32 = 100_000;

pub const CHUNK: u64 = 64 * 1024;
pub const OVERHEAD: u64 = 28;
pub const ENC_QUERY: &str = "castify-enc=1";
pub const HDR_ENC: &str = "X-Castify-Enc";
pub const ENC_ID: &str = "aes-256-gcm-v1";
pub const HDR_SALT: &str = "X-Castify-Salt";
pub const HDR_PLAIN: &str = "X-Castify-Plain-Length";
pub const HDR_CHUNK: &str = "X-Castify-Chunk";
pub const HDR_MIME: &str = "X-Castify-Mime";
pub const HDR_STREAM: &str = "X-Castify-Stream";

#[derive(Clone)]
pub struct MediaKey {
    key: [u8; 32],
    pub salt: [u8; 16],
}

impl MediaKey {
    pub fn generate(passphrase: &str) -> io::Result<Self> {
        let mut salt = [0u8; 16];
        getrandom::getrandom(&mut salt)
            .map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))?;
        Self::open(passphrase, salt)
    }

    pub fn open(passphrase: &str, salt: [u8; 16]) -> io::Result<Self> {
        if passphrase.trim().is_empty() {
            return Err(io::Error::new(ErrorKind::InvalidInput, "empty passphrase"));
        }
        let mut key = [0u8; 32];
        pbkdf2_hmac_sha256(passphrase.trim().as_bytes(), &salt, PBKDF2_ROUNDS, &mut key);
        Ok(Self { key, salt })
    }

    pub fn encrypt_chunk(&self, index: u64, plain: &[u8]) -> io::Result<Vec<u8>> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|_| io::Error::new(ErrorKind::Other, "key"))?;
        let nonce_bytes = nonce_for(&self.salt, index);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let aad = index.to_be_bytes();
        let ct = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plain,
                    aad: &aad,
                },
            )
            .map_err(|_| io::Error::new(ErrorKind::Other, "encrypt"))?;
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    pub fn decrypt_chunk(&self, index: u64, frame: &[u8]) -> io::Result<Vec<u8>> {
        if frame.len() < OVERHEAD as usize {
            return Err(io::Error::new(ErrorKind::InvalidData, "short frame"));
        }
        let expect = nonce_for(&self.salt, index);
        if frame[..12] != expect {
            return Err(io::Error::new(ErrorKind::InvalidData, "bad nonce"));
        }
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|_| io::Error::new(ErrorKind::Other, "key"))?;
        let nonce = Nonce::from_slice(&expect);
        let aad = index.to_be_bytes();
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &frame[12..],
                    aad: &aad,
                },
            )
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "decrypt failed"))
    }
}

/// 16 random bytes, hex encoded, for unguessable URLs.
pub fn random_token() -> io::Result<String> {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))?;
    Ok(hex_encode(&buf))
}

fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], rounds: u32, out: &mut [u8]) {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(password).expect("hmac accepts any key length");
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let mut u = [0u8; 32];
    u.copy_from_slice(&mac.finalize().into_bytes());
    let mut block = u;
    for _ in 1..rounds.max(1) {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(password).expect("hmac accepts any key length");
        mac.update(&u);
        u.copy_from_slice(&mac.finalize().into_bytes());
        for (left, right) in block.iter_mut().zip(u.iter()) {
            *left ^= *right;
        }
    }
    let n = out.len().min(block.len());
    out[..n].copy_from_slice(&block[..n]);
}

fn nonce_for(salt: &[u8; 16], index: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..8].copy_from_slice(&salt[..8]);
    nonce[8..].copy_from_slice(&(index as u32).to_be_bytes());
    nonce
}

pub fn cipher_total(plain_total: u64, chunk: u64) -> u64 {
    if plain_total == 0 || chunk == 0 {
        return 0;
    }
    let n = (plain_total + chunk - 1) / chunk;
    let full = n - 1;
    let last_plain = plain_total - full * chunk;
    full * (chunk + OVERHEAD) + last_plain + OVERHEAD
}

pub fn chunk_plain_len(index: u64, chunk: u64, plain_total: u64) -> u64 {
    let start = index.saturating_mul(chunk);
    if start >= plain_total {
        0
    } else {
        (plain_total - start).min(chunk)
    }
}

pub fn chunk_cipher_start(index: u64, chunk: u64) -> u64 {
    index.saturating_mul(chunk + OVERHEAD)
}

pub fn read_cipher_range<F>(
    key: &MediaKey,
    chunk: u64,
    plain_total: u64,
    mut c0: u64,
    mut c1: u64,
    mut read_plain: F,
) -> io::Result<Vec<u8>>
where
    F: FnMut(u64, usize) -> io::Result<Vec<u8>>,
{
    let total = cipher_total(plain_total, chunk);
    if total == 0 || c1 < c0 {
        return Ok(Vec::new());
    }
    c0 = c0.min(total - 1);
    c1 = c1.min(total - 1);
    let stride = chunk + OVERHEAD;
    let last_index = (plain_total - 1) / chunk;
    let first = (c0 / stride).min(last_index);
    let last = (c1 / stride).min(last_index);
    let mut out = Vec::new();
    for index in first..=last {
        let plen = chunk_plain_len(index, chunk, plain_total) as usize;
        let plain = read_plain(index * chunk, plen)?;
        if plain.len() != plen {
            return Err(io::Error::new(ErrorKind::UnexpectedEof, "short plain read"));
        }
        let frame = key.encrypt_chunk(index, &plain)?;
        let start = chunk_cipher_start(index, chunk);
        let frame_end = start + frame.len() as u64 - 1;
        let from = c0.max(start);
        let to = c1.min(frame_end);
        if to >= from {
            let a = (from - start) as usize;
            let b = (to - start) as usize + 1;
            out.extend_from_slice(&frame[a..b]);
        }
    }
    Ok(out)
}

pub fn read_plain_range<F>(
    key: &MediaKey,
    chunk: u64,
    plain_total: u64,
    p0: u64,
    p1: u64,
    mut read_cipher: F,
) -> io::Result<Vec<u8>>
where
    F: FnMut(u64, u64) -> io::Result<Vec<u8>>,
{
    if plain_total == 0 || p1 < p0 {
        return Ok(Vec::new());
    }
    let p0 = p0.min(plain_total - 1);
    let p1 = p1.min(plain_total - 1);
    let first = p0 / chunk;
    let last = p1 / chunk;
    let mut out = Vec::new();
    for index in first..=last {
        let plen = chunk_plain_len(index, chunk, plain_total);
        let cstart = chunk_cipher_start(index, chunk);
        let cend = cstart + plen + OVERHEAD - 1;
        let frame = read_cipher(cstart, cend)?;
        let plain = key.decrypt_chunk(index, &frame)?;
        let base = index * chunk;
        let from = p0.max(base);
        let to = p1.min(base + plen - 1);
        let a = (from - base) as usize;
        let b = (to - base) as usize + 1;
        out.extend_from_slice(&plain[a..b]);
    }
    Ok(out)
}

pub fn pack_stream_frame(key: &MediaKey, index: u64, plain: &[u8]) -> io::Result<Vec<u8>> {
    let frame = key.encrypt_chunk(index, plain)?;
    let mut out = Vec::with_capacity(4 + frame.len());
    out.extend_from_slice(&(frame.len() as u32).to_be_bytes());
    out.extend_from_slice(&frame);
    Ok(out)
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

pub fn hex_decode_16(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    let bytes = text.as_bytes();
    for i in 0..16 {
        let hi = hex_val(bytes[i * 2])?;
        let lo = hex_val(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_across_chunk_boundary() {
        let key = MediaKey::generate("same-secret").unwrap();
        let plain: Vec<u8> = (0..20_000u32).map(|n| (n % 251) as u8).collect();
        let chunk = 8_192u64;
        let cipher = read_cipher_range(&key, chunk, plain.len() as u64, 0, u64::MAX, |off, len| {
            Ok(plain[off as usize..off as usize + len].to_vec())
        })
        .unwrap();
        assert_eq!(cipher.len() as u64, cipher_total(plain.len() as u64, chunk));
        let back = read_plain_range(&key, chunk, plain.len() as u64, 8000, 9000, |c0, c1| {
            Ok(cipher[c0 as usize..=c1 as usize].to_vec())
        })
        .unwrap();
        assert_eq!(back, plain[8000..=9000]);
    }

    #[test]
    fn pbkdf2_sha256_matches_one_round_vector() {
        let mut out = [0u8; 32];
        pbkdf2_hmac_sha256(b"password", b"salt", 1, &mut out);
        assert_eq!(
            hex_encode(&out),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
    }

    #[test]
    fn wrong_passphrase_fails() {
        let key = MediaKey::generate("one").unwrap();
        let frame = key.encrypt_chunk(0, b"photo-bytes").unwrap();
        let other = MediaKey::open("two", key.salt).unwrap();
        assert!(other.decrypt_chunk(0, &frame).is_err());
    }
}
