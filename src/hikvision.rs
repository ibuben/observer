//! Hikvision по локальному или белому IP: ISAPI (HTTP) и видео по RTSP.
//! Пароль остаётся в запросе внутри программы и не попадает в командную строку.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use md5::{Digest, Md5};

use crate::error::Error;
use crate::frame::Codec;

const HANDSHAKE: Duration = Duration::from_secs(8);
const MEDIA_GAP: Duration = Duration::from_secs(20);

#[derive(Clone, Debug)]
pub struct HikInfo {
    pub name: String,
    pub model: String,
    pub serial: String,
    pub firmware: String,
    pub channels: Vec<String>,
    pub door: bool,
}

pub struct HikNal {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub codec: Codec,
}

struct Challenge {
    realm: String,
    nonce: String,
    qop: Option<String>,
    opaque: Option<String>,
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct BytePipe {
    data: Vec<u8>,
    pos: usize,
}

impl BytePipe {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            pos: 0,
        }
    }

    fn rest(&self) -> &[u8] {
        &self.data[self.pos..]
    }

    fn pull(&mut self, stream: &mut TcpStream) -> Result<(), Error> {
        if self.pos > 4096 && self.pos * 2 > self.data.len() {
            self.data.drain(..self.pos);
            self.pos = 0;
        }
        let mut buf = [0u8; 8192];
        let count = stream.read(&mut buf).map_err(map_timeout)?;
        if count == 0 {
            return Err(Error::Ended);
        }
        self.data.extend_from_slice(&buf[..count]);
        Ok(())
    }

    fn take(&mut self, stream: &mut TcpStream, count: usize) -> Result<Vec<u8>, Error> {
        while self.rest().len() < count {
            self.pull(stream)?;
        }
        let bytes = self.rest()[..count].to_vec();
        self.pos += count;
        Ok(bytes)
    }

    fn take_byte(&mut self, stream: &mut TcpStream) -> Result<u8, Error> {
        Ok(self.take(stream, 1)?[0])
    }
}

pub struct HikStream {
    stream: TcpStream,
    pipe: BytePipe,
    codec: Codec,
    fu: Vec<u8>,
    pending: Vec<HikNal>,
}

impl HikStream {
    pub fn open(
        host: &str,
        user: &str,
        password: &str,
        channel: u16,
        main: bool,
    ) -> Result<Self, Error> {
        let stream_id = u32::from(channel.max(1)) * 100 + if main { 1 } else { 2 };
        let path = format!("/Streaming/Channels/{stream_id}");
        let url = format!("rtsp://{host}:554{path}");
        let stream = connect_timeout(host, 554)?;
        stream.set_nodelay(true).ok();
        let mut session = Rtsp::new(stream, user, password);
        let describe = session.request("DESCRIBE", &url, &[("Accept", "application/sdp")], &[])?;
        if describe.status == 401 {
            return Err(Error::Device {
                code: 401,
                message: "неверный логин или пароль".into(),
            });
        }
        if describe.status != 200 {
            return Err(Error::Protocol(format!(
                "RTSP DESCRIBE вернул {}",
                describe.status
            )));
        }
        let sdp = String::from_utf8_lossy(&describe.body).to_string();
        let codec = if sdp.contains("H265") || sdp.contains("HEVC") {
            Codec::H265
        } else {
            Codec::H264
        };
        let control = video_control(&sdp);
        let setup_url = if control.starts_with("rtsp://") {
            control
        } else if control.starts_with('/') {
            format!("rtsp://{host}:554{control}")
        } else {
            format!("{url}/{control}")
        };
        let setup = session.request(
            "SETUP",
            &setup_url,
            &[("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1")],
            &[],
        )?;
        if setup.status != 200 {
            return Err(Error::Protocol(format!("RTSP SETUP вернул {}", setup.status)));
        }
        let session_id = setup
            .header("session")
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if session_id.is_empty() {
            return Err(Error::Protocol("RTSP не выдал сессию".into()));
        }
        let play = session.request(
            "PLAY",
            &url,
            &[("Session", &session_id), ("Range", "npt=0.000-")],
            &[],
        )?;
        if play.status != 200 {
            return Err(Error::Protocol(format!("RTSP PLAY вернул {}", play.status)));
        }
        session.stream.set_read_timeout(Some(MEDIA_GAP)).ok();
        Ok(Self {
            stream: session.stream,
            pipe: session.pipe,
            codec,
            fu: Vec::new(),
            pending: Vec::new(),
        })
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    pub fn clone_socket(&self) -> Result<TcpStream, Error> {
        self.stream.try_clone().map_err(Error::Io)
    }

    pub fn read_nal(&mut self) -> Result<HikNal, Error> {
        if let Some(nal) = self.pending.pop() {
            return Ok(nal);
        }
        loop {
            let packet = read_interleaved(&mut self.stream, &mut self.pipe)?;
            if let Some(mut nals) = self.depay(&packet) {
                if nals.is_empty() {
                    continue;
                }
                let first = nals.remove(0);
                self.pending.extend(nals.into_iter().rev());
                return Ok(first);
            }
        }
    }

    fn depay(&mut self, rtp: &[u8]) -> Option<Vec<HikNal>> {
        let payload = rtp_payload(rtp)?;
        if payload.is_empty() {
            return None;
        }
        match self.codec {
            Codec::H265 => self.depay_h265(payload),
            _ => self.depay_h264(payload),
        }
    }

    fn depay_h264(&mut self, payload: &[u8]) -> Option<Vec<HikNal>> {
        let nal_type = payload[0] & 0x1f;
        if nal_type == 28 {
            if payload.len() < 2 {
                return None;
            }
            let fu = payload[1];
            let start = fu & 0x80 != 0;
            let end = fu & 0x40 != 0;
            if start {
                self.fu.clear();
                self.fu.push((payload[0] & 0xe0) | (fu & 0x1f));
            }
            if !self.fu.is_empty() {
                self.fu.extend_from_slice(&payload[2.min(payload.len())..]);
            }
            if end && !self.fu.is_empty() {
                let nal = std::mem::take(&mut self.fu);
                return Some(vec![annex_b(nal, Codec::H264)]);
            }
            None
        } else if nal_type == 24 {
            self.fu.clear();
            let mut nals = Vec::new();
            let mut index = 1;
            while index + 2 <= payload.len() {
                let size = u16::from_be_bytes([payload[index], payload[index + 1]]) as usize;
                index += 2;
                if index + size > payload.len() {
                    break;
                }
                nals.push(annex_b(payload[index..index + size].to_vec(), Codec::H264));
                index += size;
            }
            if nals.is_empty() { None } else { Some(nals) }
        } else if (1..24).contains(&nal_type) {
            self.fu.clear();
            Some(vec![annex_b(payload.to_vec(), Codec::H264)])
        } else {
            None
        }
    }

    fn depay_h265(&mut self, payload: &[u8]) -> Option<Vec<HikNal>> {
        if payload.len() < 2 {
            return None;
        }
        let nal_type = (payload[0] >> 1) & 0x3f;
        if nal_type == 49 {
            if payload.len() < 3 {
                return None;
            }
            let fu = payload[2];
            let start = fu & 0x80 != 0;
            let end = fu & 0x40 != 0;
            if start {
                self.fu.clear();
                let kind = fu & 0x3f;
                self.fu.push((payload[0] & 0x81) | (kind << 1));
                self.fu.push(payload[1]);
            }
            if self.fu.len() >= 2 {
                self.fu.extend_from_slice(&payload[3.min(payload.len())..]);
            }
            if end && !self.fu.is_empty() {
                let nal = std::mem::take(&mut self.fu);
                return Some(vec![annex_b(nal, Codec::H265)]);
            }
            None
        } else if nal_type < 48 {
            self.fu.clear();
            Some(vec![annex_b(payload.to_vec(), Codec::H265)])
        } else {
            None
        }
    }
}

pub fn describe(host: &str, port: u16, user: &str, password: &str) -> Result<HikInfo, Error> {
    let info = http(host, port, user, password, "GET", "/ISAPI/System/deviceInfo", None)?;
    ensure_ok(&info, "устройство не ответило")?;
    let body = String::from_utf8_lossy(&info.body);
    let model = xml_text(&body, "model").unwrap_or_default();
    let channels = match http(
        host,
        port,
        user,
        password,
        "GET",
        "/ISAPI/System/Video/inputs/channels",
        None,
    ) {
        Ok(response) if response.status == 200 => {
            parse_channels(&String::from_utf8_lossy(&response.body))
        }
        _ => Vec::new(),
    };
    let channels = if channels.is_empty() {
        vec!["Камера 1".into()]
    } else {
        channels
    };
    let door = looks_like_door(&model)
        || isapi_exists(host, port, user, password, "/ISAPI/VideoIntercom/capabilities")
        || isapi_exists(host, port, user, password, "/ISAPI/AccessControl/capabilities");
    Ok(HikInfo {
        name: xml_text(&body, "deviceName").unwrap_or_default(),
        model,
        serial: xml_text(&body, "serialNumber").unwrap_or_default(),
        firmware: xml_text(&body, "firmwareVersion").unwrap_or_default(),
        channels,
        door,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoorCall {
    Idle,
    Ring,
    Talk,
}

pub fn call_status(host: &str, port: u16, user: &str, password: &str) -> Result<DoorCall, Error> {
    let response = http(
        host,
        port,
        user,
        password,
        "GET",
        "/ISAPI/VideoIntercom/callStatus?format=json",
        None,
    )?;
    if response.status == 401 {
        return Err(Error::Device {
            code: 401,
            message: "неверный логин или пароль".into(),
        });
    }
    if response.status != 200 {
        return Err(Error::Device {
            code: response.status as i64,
            message: format!("состояние вызова не прочитано ({})", response.status),
        });
    }
    let text = String::from_utf8_lossy(&response.body);
    let word = json_quoted_after_key(&text.to_ascii_lowercase(), "status").ok_or_else(|| {
        Error::Protocol("домофон не сообщил состояние вызова".into())
    })?;
    Ok(match word.as_str() {
        "idle" | "hangup" | "hangingup" => DoorCall::Idle,
        "oncall" | "talk" | "talking" | "connected" | "answered" => DoorCall::Talk,
        _ => DoorCall::Ring,
    })
}

fn json_quoted_after_key(text: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let mut rest = text;
    while let Some(pos) = rest.find(&pattern) {
        let after = rest[pos + pattern.len()..].trim_start();
        if let Some(after) = after.strip_prefix(':') {
            let after = after.trim_start();
            if let Some(after) = after.strip_prefix('"') {
                let end = after.find('"')?;
                return Some(after[..end].to_string());
            }
        }
        rest = &rest[pos + pattern.len()..];
    }
    None
}

pub fn open_door(host: &str, port: u16, user: &str, password: &str) -> Result<(), Error> {
    let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<RemoteControlDoor><cmd>open</cmd></RemoteControlDoor>";
    let response = http(
        host,
        port,
        user,
        password,
        "PUT",
        "/ISAPI/AccessControl/RemoteControl/door/1",
        Some((body, "application/xml")),
    )?;
    if response.status == 401 {
        return Err(Error::Device {
            code: 401,
            message: "неверный логин или пароль".into(),
        });
    }
    let text = String::from_utf8_lossy(&response.body);
    if response.status == 200 && response_ok(&text) {
        return Ok(());
    }
    let detail = xml_text(&text, "statusString").unwrap_or_else(|| format!("код {}", response.status));
    Err(Error::Device {
        code: response.status as i64,
        message: format!("замок не открылся: {detail}"),
    })
}

fn looks_like_door(model: &str) -> bool {
    let model = model.to_ascii_uppercase();
    model.contains("DS-KD") || model.contains("DS-KB") || model.contains("DOOR")
}

fn isapi_exists(host: &str, port: u16, user: &str, password: &str, path: &str) -> bool {
    matches!(http(host, port, user, password, "GET", path, None), Ok(response) if response.status == 200)
}

fn ensure_ok(response: &HttpResponse, what: &str) -> Result<(), Error> {
    if response.status == 401 {
        return Err(Error::Device {
            code: 401,
            message: "неверный логин или пароль".into(),
        });
    }
    if response.status != 200 {
        return Err(Error::Device {
            code: response.status as i64,
            message: format!("{what} ({})", response.status),
        });
    }
    Ok(())
}

fn response_ok(body: &str) -> bool {
    match xml_text(body, "statusCode") {
        Some(code) => code == "0" || code == "1",
        None => true,
    }
}

fn http(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    method: &str,
    path: &str,
    body: Option<(&str, &str)>,
) -> Result<HttpResponse, Error> {
    let first = http_once(host, port, method, path, body, None)?;
    if first.status != 401 {
        return Ok(first);
    }
    let header = first
        .header("www-authenticate")
        .ok_or_else(|| Error::Protocol("нет заголовка авторизации".into()))?;
    let challenge = parse_digest(header).ok_or_else(|| Error::Protocol("дайджест не разобран".into()))?;
    let cnonce = format!("{:016x}", challenge.nonce.len() as u64 ^ 0x5a17_u64.wrapping_mul(port as u64));
    let authorization = authorization(method, path, user, password, &challenge, 1, &cnonce);
    http_once(host, port, method, path, body, Some(&authorization))
}

fn http_once(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    body: Option<(&str, &str)>,
    authorization: Option<&str>,
) -> Result<HttpResponse, Error> {
    let mut stream = connect_timeout(host, port)?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(HANDSHAKE)).ok();
    stream.set_write_timeout(Some(HANDSHAKE)).ok();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: eXUnity Observer\r\nConnection: close\r\n"
    );
    if let Some(authorization) = authorization {
        request.push_str("Authorization: ");
        request.push_str(authorization);
        request.push_str("\r\n");
    }
    let payload = body.map(|(text, _)| text.as_bytes()).unwrap_or(&[]);
    if let Some((_, content_type)) = body {
        request.push_str(&format!(
            "Content-Type: {content_type}\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).map_err(Error::Io)?;
    stream.write_all(payload).map_err(Error::Io)?;
    stream.flush().map_err(Error::Io)?;
    read_http(&mut stream)
}

fn connect_timeout(host: &str, port: u16) -> Result<TcpStream, Error> {
    let mut last = None;
    for socket in (host, port).to_socket_addrs().map_err(|source| Error::Connect {
        host: host.to_string(),
        port,
        source,
    })? {
        match TcpStream::connect_timeout(&socket, HANDSHAKE) {
            Ok(stream) => return Ok(stream),
            Err(err) => last = Some(err),
        }
    }
    Err(Error::Connect {
        host: host.to_string(),
        port,
        source: last.unwrap_or_else(|| std::io::Error::other("адрес не найден")),
    })
}

fn read_http(stream: &mut TcpStream) -> Result<HttpResponse, Error> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let split = loop {
        let count = stream.read(&mut buf).map_err(Error::Io)?;
        if count == 0 {
            return Err(Error::Ended);
        }
        raw.extend_from_slice(&buf[..count]);
        if let Some(index) = find_header_end(&raw) {
            break index;
        }
        if raw.len() > 1024 * 1024 {
            return Err(Error::Protocol("заголовок слишком большой".into()));
        }
    };
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut rest = raw[split + 4..].to_vec();
    let (status, headers) = parse_headers(&head)?;
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    while rest.len() < length {
        let count = stream.read(&mut buf).map_err(Error::Io)?;
        if count == 0 {
            break;
        }
        rest.extend_from_slice(&buf[..count]);
    }
    rest.truncate(length);
    Ok(HttpResponse {
        status,
        headers,
        body: rest,
    })
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn find_header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|item| item == b"\r\n\r\n")
}

fn parse_headers(head: &str) -> Result<(u16, Vec<(String, String)>), Error> {
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|item| item.parse().ok())
        .ok_or_else(|| Error::Protocol("статус ответа не разобран".into()))?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    Ok((status, headers))
}

struct Rtsp {
    stream: TcpStream,
    pipe: BytePipe,
    user: String,
    password: String,
    cseq: u32,
    nc: u32,
    challenge: Option<Challenge>,
}

struct RtspResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl RtspResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

impl Rtsp {
    fn new(stream: TcpStream, user: &str, password: &str) -> Self {
        Self {
            stream,
            pipe: BytePipe::new(),
            user: user.to_string(),
            password: password.to_string(),
            cseq: 0,
            nc: 0,
            challenge: None,
        }
    }

    fn request(
        &mut self,
        method: &str,
        uri: &str,
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> Result<RtspResponse, Error> {
        let response = self.send(method, uri, extra, body, false)?;
        if response.status != 401 {
            return Ok(response);
        }
        let header = response
            .header("www-authenticate")
            .ok_or_else(|| Error::Protocol("RTSP не запросил авторизацию".into()))?;
        self.challenge = Some(parse_digest(header).ok_or_else(|| Error::Protocol("дайджест RTSP не разобран".into()))?);
        self.nc = 0;
        self.send(method, uri, extra, body, true)
    }

    fn send(
        &mut self,
        method: &str,
        uri: &str,
        extra: &[(&str, &str)],
        body: &[u8],
        authorized: bool,
    ) -> Result<RtspResponse, Error> {
        self.cseq += 1;
        let mut text = format!(
            "{method} {uri} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: eXUnity Observer\r\n",
            self.cseq
        );
        if authorized {
            if let Some(challenge) = &self.challenge {
                self.nc += 1;
                let cnonce = format!("{:016x}", self.cseq as u64 ^ 0xA5A5_5A5A);
                let header = authorization(method, uri, &self.user, &self.password, challenge, self.nc, &cnonce);
                text.push_str("Authorization: ");
                text.push_str(&header);
                text.push_str("\r\n");
            }
        }
        for (name, value) in extra {
            text.push_str(name);
            text.push_str(": ");
            text.push_str(value);
            text.push_str("\r\n");
        }
        if !body.is_empty() {
            text.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        text.push_str("\r\n");
        self.stream.set_read_timeout(Some(HANDSHAKE)).ok();
        self.stream.write_all(text.as_bytes()).map_err(Error::Io)?;
        self.stream.write_all(body).map_err(Error::Io)?;
        self.stream.flush().map_err(Error::Io)?;
        read_rtsp(&mut self.stream, &mut self.pipe)
    }
}

fn read_rtsp(stream: &mut TcpStream, pipe: &mut BytePipe) -> Result<RtspResponse, Error> {
    let split = loop {
        if let Some(index) = find_header_end(pipe.rest()) {
            break index;
        }
        pipe.pull(stream)?;
    };
    let head = String::from_utf8_lossy(&pipe.rest()[..split]).to_string();
    pipe.pos += split + 4;
    let (status, headers) = parse_headers(&head)?;
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let body = pipe.take(stream, length)?;
    Ok(RtspResponse {
        status,
        headers,
        body,
    })
}

fn read_interleaved(stream: &mut TcpStream, pipe: &mut BytePipe) -> Result<Vec<u8>, Error> {
    loop {
        let marker = pipe.take_byte(stream)?;
        if marker != b'$' {
            discard_rtsp(stream, pipe, marker)?;
            continue;
        }
        let header = pipe.take(stream, 3)?;
        let channel = header[0];
        let length = u16::from_be_bytes([header[1], header[2]]) as usize;
        if length > 1_048_576 {
            return Err(Error::Protocol("слишком большой RTP".into()));
        }
        let payload = pipe.take(stream, length)?;
        if channel == 0 {
            return Ok(payload);
        }
    }
}

fn discard_rtsp(stream: &mut TcpStream, pipe: &mut BytePipe, first: u8) -> Result<(), Error> {
    let mut seen = vec![first];
    loop {
        seen.push(pipe.take_byte(stream)?);
        if seen.len() >= 4 && seen[seen.len() - 4..] == *b"\r\n\r\n" {
            return Ok(());
        }
        if seen.len() > 65536 {
            return Err(Error::Protocol("служебный ответ RTSP слишком большой".into()));
        }
    }
}

fn map_timeout(err: std::io::Error) -> Error {
    if err.kind() == std::io::ErrorKind::TimedOut || err.kind() == std::io::ErrorKind::WouldBlock {
        Error::Timeout
    } else {
        Error::Io(err)
    }
}

fn rtp_payload(packet: &[u8]) -> Option<&[u8]> {
    if packet.len() < 12 {
        return None;
    }
    let mut offset = 12 + (packet[0] & 0x0f) as usize * 4;
    if packet[0] & 0x10 != 0 {
        if packet.len() < offset + 4 {
            return None;
        }
        let words = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;
        offset += 4 + words * 4;
    }
    packet.get(offset..)
}

fn annex_b(nal: Vec<u8>, codec: Codec) -> HikNal {
    let keyframe = match codec {
        Codec::H265 => {
            let kind = nal.first().map(|byte| (byte >> 1) & 0x3f).unwrap_or(0);
            matches!(kind, 19 | 20 | 21 | 32 | 33 | 34)
        }
        _ => {
            let kind = nal.first().map(|byte| byte & 0x1f).unwrap_or(0);
            matches!(kind, 5 | 7 | 8)
        }
    };
    let mut data = vec![0, 0, 0, 1];
    data.extend(nal);
    HikNal {
        data,
        keyframe,
        codec,
    }
}

fn video_control(sdp: &str) -> String {
    let mut video = false;
    for line in sdp.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("m=") {
            video = rest.starts_with("video");
        } else if video {
            if let Some(control) = line.strip_prefix("a=control:") {
                return control.trim().to_string();
            }
        }
    }
    "trackID=1".into()
}

fn parse_digest(header: &str) -> Option<Challenge> {
    let value = header
        .trim()
        .strip_prefix("Digest")
        .unwrap_or(header)
        .trim();
    let params = digest_params(value);
    let realm = param(&params, "realm")?;
    let nonce = param(&params, "nonce")?;
    let qop = param(&params, "qop").map(|item| {
        item.split(',')
            .map(str::trim)
            .find(|item| *item == "auth")
            .unwrap_or("auth")
            .to_string()
    });
    Some(Challenge {
        realm,
        nonce,
        qop,
        opaque: param(&params, "opaque"),
    })
}

fn param(params: &[(String, String)], name: &str) -> Option<String> {
    params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn digest_params(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        rest = rest.trim_start_matches([',', ' ']);
        if rest.is_empty() {
            break;
        }
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let key = key.trim().to_ascii_lowercase();
        let (item, next) = if let Some(quoted) = after.trim_start().strip_prefix('"') {
            let end = quoted.find('"').unwrap_or(quoted.len());
            let item = quoted[..end].to_string();
            let next = quoted[end..].trim_start_matches('"');
            (item, next)
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (after[..end].trim().to_string(), &after[end..])
        };
        out.push((key, item));
        rest = next;
    }
    out
}

fn authorization(
    method: &str,
    uri: &str,
    user: &str,
    password: &str,
    challenge: &Challenge,
    nc: u32,
    cnonce: &str,
) -> String {
    let nc_text = format!("{nc:08x}");
    let ha1 = md5_hex(format!("{user}:{}:{password}", challenge.realm).as_bytes());
    let ha2 = md5_hex(format!("{method}:{uri}").as_bytes());
    let response = if challenge.qop.is_some() {
        md5_hex(format!("{ha1}:{}:{nc_text}:{cnonce}:auth:{ha2}", challenge.nonce).as_bytes())
    } else {
        md5_hex(format!("{ha1}:{}:{ha2}", challenge.nonce).as_bytes())
    };
    let mut header = format!(
        "Digest username=\"{user}\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{response}\"",
        challenge.realm, challenge.nonce
    );
    if challenge.qop.is_some() {
        header.push_str(&format!(", qop=auth, nc={nc_text}, cnonce=\"{cnonce}\""));
    }
    if let Some(opaque) = &challenge.opaque {
        header.push_str(&format!(", opaque=\"{opaque}\""));
    }
    header
}

fn md5_hex(data: &[u8]) -> String {
    Md5::digest(data).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let text = xml[start..end].trim();
    if text.is_empty() {
        None
    } else {
        Some(unescape(text))
    }
}

fn parse_channels(xml: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<VideoInputChannel") {
        rest = &rest[start + 1..];
        let end = rest.find("</VideoInputChannel>").unwrap_or(rest.len());
        let block = &rest[..end];
        let name = xml_text(block, "name")
            .or_else(|| xml_text(block, "id").map(|id| format!("Камера {id}")))
            .unwrap_or_else(|| format!("Камера {}", names.len() + 1));
        names.push(name);
        rest = &rest[end..];
    }
    names
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2617_digest() {
        let challenge = Challenge {
            realm: "testrealm@host.com".into(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".into(),
            qop: Some("auth".into()),
            opaque: Some("5ccc069c403ebaf9f0171e9517f40e41".into()),
        };
        let header = authorization(
            "GET",
            "/dir/index.html",
            "Mufasa",
            "Circle Of Life",
            &challenge,
            1,
            "0a4f113b",
        );
        assert!(header.contains("response=\"6629fae49393a05397450978507c4ef1\""));
    }

    #[test]
    fn channel_names_come_from_isapi_blocks() {
        let xml = r#"<VideoInputChannel><id>1</id><name>Door</name></VideoInputChannel>"#;
        assert_eq!(parse_channels(xml), vec!["Door".to_string()]);
    }

    #[test]
    fn h264_fragment_becomes_idr() {
        let mut stream = HikStream {
            stream: local_socket(),
            pipe: BytePipe::new(),
            codec: Codec::H264,
            fu: Vec::new(),
            pending: Vec::new(),
        };
        let start = [0x7c, 0x85, 0x11];
        let end = [0x7c, 0x45, 0x22];
        assert!(stream.depay_h264(&start).is_none());
        let nal = stream.depay_h264(&end).unwrap();
        assert!(nal[0].keyframe);
        assert_eq!(&nal[0].data[..5], &[0, 0, 0, 1, 0x65]);
        assert_eq!(&nal[0].data[5..], &[0x11, 0x22]);
    }

    #[test]
    fn call_status_words() {
        let idle = r#"{"CallStatus":{"status":"idle"}}"#;
        let ring = r#"{"CallStatus":{"status":"ring"}}"#;
        let talk = r#"{"CallStatus":{"status":"onCall"}}"#;
        assert_eq!(json_quoted_after_key(&idle.to_ascii_lowercase(), "status").as_deref(), Some("idle"));
        assert_eq!(json_quoted_after_key(&ring.to_ascii_lowercase(), "status").as_deref(), Some("ring"));
        assert_eq!(json_quoted_after_key(&talk.to_ascii_lowercase(), "status").as_deref(), Some("oncall"));
    }

    fn local_socket() -> TcpStream {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let _accepted = listener.accept().unwrap();
        client
    }
}
