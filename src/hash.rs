use md5::{Digest, Md5};

/// Пароль в том виде, в каком его ждёт регистратор XM.
/// Это не MD5 целиком: берётся сумма соседних байт хеша.
pub fn sofia_hash(password: &str) -> String {
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let digest = Md5::digest(password.as_bytes());
    let mut out = String::with_capacity(8);
    for pair in digest.chunks_exact(2) {
        let index = (pair[0] as usize + pair[1] as usize) % 62;
        out.push(CHARS[index] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::sofia_hash;

    #[test]
    fn known_vectors() {
        assert_eq!(sofia_hash(""), "tlJwpbo6");
        assert_eq!(sofia_hash("admin"), "6QNMIQGe");
        assert_eq!(sofia_hash("12345"), "6V0Y4HLF");
    }
}
