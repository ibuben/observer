use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::Value;

use crate::error::Error;

pub const HEADER_LEN: usize = 20;

#[derive(Debug, Clone)]
pub struct Packet {
    #[allow(dead_code)]
    pub version: u8,
    pub session: u32,
    #[allow(dead_code)]
    pub sequence: u32,
    pub msg: u16,
    pub payload: Vec<u8>,
}

pub fn encode(session: u32, sequence: u32, msg: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(0xFF);
    out.push(0);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&session.to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&msg.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn decode_header(buf: &[u8]) -> Result<(u8, u32, u32, u16, u32), Error> {
    if buf.len() < HEADER_LEN || buf[0] != 0xFF {
        return Err(Error::Protocol(format!(
            "заголовок {HEADER_LEN} байт, получено {}",
            buf.len()
        )));
    }
    let version = buf[1];
    let session = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    let sequence = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let msg = u16::from_le_bytes(buf[14..16].try_into().unwrap());
    let len = u32::from_le_bytes(buf[16..20].try_into().unwrap());
    Ok((version, session, sequence, msg, len))
}

pub fn read_packet(stream: &mut TcpStream) -> Result<Packet, Error> {
    let mut header = [0u8; HEADER_LEN];
    stream.read_exact(&mut header).map_err(map_io)?;
    let (version, session, sequence, msg, len) = decode_header(&header)?;
    if len > 16 * 1024 * 1024 {
        return Err(Error::Protocol(format!(
            "слишком большой пакет: {len} байт"
        )));
    }
    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        stream.read_exact(&mut payload).map_err(map_io)?;
    }
    Ok(Packet {
        version,
        session,
        sequence,
        msg,
        payload,
    })
}

pub fn write_packet(
    stream: &mut TcpStream,
    session: u32,
    sequence: u32,
    msg: u16,
    payload: &[u8],
) -> Result<(), Error> {
    let bytes = encode(session, sequence, msg, payload);
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

pub fn json_payload(value: &Value) -> Result<Vec<u8>, Error> {
    let mut payload = serde_json::to_vec(value)
        .map_err(|err| Error::Protocol(format!("не удалось собрать JSON: {err}")))?;
    payload.push(b'\n');
    payload.push(0);
    Ok(payload)
}

pub fn parse_json_payload(payload: &[u8]) -> Result<Value, Error> {
    let start = payload
        .iter()
        .position(|byte| *byte == b'{')
        .ok_or_else(|| Error::Protocol("в ответе нет JSON".into()))?;
    let end = payload
        .iter()
        .rposition(|byte| *byte == b'}')
        .ok_or_else(|| Error::Protocol("JSON обрывается".into()))?;
    if end < start {
        return Err(Error::Protocol("JSON обрывается".into()));
    }
    let slice = &payload[start..=end];
    match serde_json::from_slice(slice) {
        Ok(value) => Ok(value),
        Err(_) => {
            let text = String::from_utf8_lossy(slice);
            if let Ok(value) = serde_json::from_str(&text) {
                return Ok(value);
            }
            let cleaned: String = text
                .chars()
                .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\r' | '\t'))
                .collect();
            serde_json::from_str(&cleaned)
                .map_err(|err| Error::Protocol(format!("неразборчивый JSON: {err}")))
        }
    }
}

pub fn set_timeouts(stream: &TcpStream, timeout: Duration) -> Result<(), Error> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_nodelay(true)?;
    Ok(())
}

fn map_io(err: std::io::Error) -> Error {
    match err.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Error::Timeout,
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionAborted => Error::Ended,
        _ => Error::Io(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn header_roundtrip() {
        let payload = b"{}";
        let bytes = encode(0x11, 7, 1000, payload);
        assert_eq!(bytes.len(), HEADER_LEN + 2);
        let (_version, session, sequence, msg, len) = decode_header(&bytes).unwrap();
        assert_eq!(session, 0x11);
        assert_eq!(sequence, 7);
        assert_eq!(msg, 1000);
        assert_eq!(len, 2);
    }

    #[test]
    fn json_ignores_tail() {
        let mut payload = json_payload(&json!({"Ret": 100, "SessionID": "0x0000002A"})).unwrap();
        payload.push(0);
        let value = parse_json_payload(&payload).unwrap();
        assert_eq!(value["Ret"], 100);
        assert_eq!(value["SessionID"], "0x0000002A");
    }
}
