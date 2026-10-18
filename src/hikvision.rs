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
    pub channels: Vec<HikChannel>,
    pub door: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HikChannel {
    /// ISAPI channel number; RTSP streams are `/Streaming/Channels/{id}01` and `{id}02`.
    pub id: u16,
    pub name: String,
    pub online: bool,
}

#[derive(Clone)]
pub struct HikNal {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub codec: Codec,
    /// RTP timestamp, 90 kHz.
    pub timestamp: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HikAudioCodec {
    Pcmu,
    Pcma,
    /// 16-bit big-endian PCM.
    L16,
    /// Delivered as ADTS frames, ready for an `aac` demuxer.
    Aac,
    Other(String),
}

#[derive(Clone, Debug)]
pub struct HikAudio {
    pub codec: HikAudioCodec,
    pub rate: u32,
    pub channels: u16,
}

pub enum HikPiece {
    Video(HikNal),
    Audio(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AacFormat {
    size_length: usize,
    index_length: usize,
    index_delta_length: usize,
    profile: u8,
    freq_index: u8,
    channel_config: u8,
}

struct AudioTrack {
    control: String,
    info: HikAudio,
    aac: Option<AacFormat>,
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
    rtsp: Rtsp,
    session_id: String,
    play_url: String,
    codec: Codec,
    fu: Vec<u8>,
    pending: Vec<HikNal>,
    audio: Option<HikAudio>,
    aac: Option<AacFormat>,
    aac_partial: Vec<u8>,
    pending_audio: Vec<Vec<u8>>,
    sdp: String,
    /// Parameter sets announced in the SDP, replayed before keyframes the stream sends without them.
    params: Vec<HikNal>,
    have_params: bool,
}

impl HikStream {
    pub fn open(
        host: &str,
        user: &str,
        password: &str,
        channel: u16,
        main: bool,
    ) -> Result<Self, Error> {
        Self::open_traced(host, user, password, channel, main).0
    }

    /// Same as [`HikStream::open`], plus every RTSP request with its status and the SDP.
    pub fn open_traced(
        host: &str,
        user: &str,
        password: &str,
        channel: u16,
        main: bool,
    ) -> (Result<Self, Error>, Vec<String>) {
        let stream_id = u32::from(channel.max(1)) * 100 + if main { 1 } else { 2 };
        let url = format!("rtsp://{host}:554/Streaming/Channels/{stream_id}");
        Self::open_url(host, user, password, &url)
    }

    /// Recorded video of `track` (`{channel}01` main, `{channel}02` sub) from `begin` to `end`,
    /// both device-local `YYYY-MM-DD HH:MM:SS`.
    pub fn open_playback(
        host: &str,
        user: &str,
        password: &str,
        track: u32,
        begin: &str,
        end: &str,
    ) -> (Result<Self, Error>, Vec<String>) {
        let url = format!(
            "rtsp://{host}:554/Streaming/tracks/{track}?starttime={}&endtime={}",
            compact_time(begin),
            compact_time(end)
        );
        Self::open_url(host, user, password, &url)
    }

    fn open_url(host: &str, user: &str, password: &str, url: &str) -> (Result<Self, Error>, Vec<String>) {
        let mut log = Vec::new();
        let mut audio_tried = false;
        match Self::connect(host, user, password, url, true, &mut log, &mut audio_tried) {
            Err(err) if audio_tried && !matches!(err, Error::Device { code: 401, .. }) => {
                // Some NVRs list an audio track they refuse to set up, and then reject PLAY with 455.
                log.push(format!("со звуком не запустилось ({err}), повтор только с видео"));
                let mut ignored = false;
                let retry = Self::connect(host, user, password, url, false, &mut log, &mut ignored);
                (retry, log)
            }
            result => (result, log),
        }
    }

    fn connect(
        host: &str,
        user: &str,
        password: &str,
        url: &str,
        with_audio: bool,
        log: &mut Vec<String>,
        audio_tried: &mut bool,
    ) -> Result<Self, Error> {
        let stream = connect_timeout(host, 554)?;
        stream.set_nodelay(true).ok();
        let mut session = Rtsp::new(stream, user, password);
        let result = negotiate(&mut session, host, url, with_audio, audio_tried);
        log.append(&mut session.log);
        let negotiated = result?;
        session.stream.set_read_timeout(Some(MEDIA_GAP)).ok();
        let codec = negotiated.codec;
        Ok(Self {
            rtsp: session,
            session_id: negotiated.session_id,
            play_url: negotiated.play_url,
            codec,
            fu: Vec::new(),
            pending: Vec::new(),
            audio: negotiated.audio,
            aac: negotiated.aac,
            aac_partial: Vec::new(),
            pending_audio: Vec::new(),
            params: sprop_params(&negotiated.sdp, codec),
            sdp: negotiated.sdp,
            have_params: false,
        })
    }

    /// Asks a playback session to run at `speed`× (1, 2, 4, 8, 16). The reply is not awaited:
    /// it arrives between media packets and is skipped by the reader.
    pub fn set_speed(&mut self, speed: u8) -> Result<(), Error> {
        let scale = format!("{}.000", speed.max(1));
        let session_id = self.session_id.clone();
        let play_url = self.play_url.clone();
        self.rtsp
            .send_only("PLAY", &play_url, &[("Session", &session_id), ("Scale", &scale)])
    }
}

struct Negotiated {
    sdp: String,
    codec: Codec,
    audio: Option<HikAudio>,
    aac: Option<AacFormat>,
    session_id: String,
    play_url: String,
}

/// `2026-10-06 01:02:03` → `20261006T010203Z`. Hikvision writes device-local time with a literal `Z`.
fn compact_time(stamp: &str) -> String {
    let digits: String = stamp.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 14 {
        return digits;
    }
    format!("{}T{}Z", &digits[..8], &digits[8..14])
}

/// `2026-10-06 01:02:03` → `2026-10-06T01:02:03Z`.
fn isapi_time(stamp: &str) -> String {
    let compact = compact_time(stamp);
    if compact.len() < 16 {
        return stamp.to_string();
    }
    format!(
        "{}-{}-{}T{}:{}:{}Z",
        &compact[0..4],
        &compact[4..6],
        &compact[6..8],
        &compact[9..11],
        &compact[11..13],
        &compact[13..15]
    )
}

/// `2026-10-06T01:02:03Z` (with or without zone) → `2026-10-06 01:02:03`.
fn stamp_from_isapi(text: &str) -> String {
    let text = text.trim();
    let date = text.get(..10).unwrap_or("");
    let time = text.get(11..19).unwrap_or("00:00:00");
    format!("{date} {time}")
}

fn negotiate(
    session: &mut Rtsp,
    host: &str,
    url: &str,
    with_audio: bool,
    audio_tried: &mut bool,
) -> Result<Negotiated, Error> {
    let url = url.to_string();
    {
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
        let content_base = describe
            .header("content-base")
            .or_else(|| describe.header("content-location"))
            .map(str::to_string);
        if let Some(content_base) = &content_base {
            session.log.push(format!("Content-Base: {content_base}"));
        }
        session.log.push(format!("SDP:\n{}", sdp.trim()));
        let base = content_base
            .as_deref()
            .map(|value| value.trim_end_matches('/').to_string())
            .filter(|value| value.starts_with("rtsp://"))
            .unwrap_or_else(|| url.clone());
        let play_url = session_control(&sdp)
            .filter(|control| control.starts_with("rtsp://"))
            .unwrap_or_else(|| base.clone());
        let codec = if sdp.contains("H265") || sdp.contains("HEVC") {
            Codec::H265
        } else {
            Codec::H264
        };
        let setup_url = track_url(&video_control(&sdp), host, &base);
        let setup = session.request(
            "SETUP",
            &setup_url,
            &[("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1")],
            &[],
        )?;
        if setup.status != 200 {
            return Err(Error::Protocol(format!("RTSP SETUP вернул {}", setup.status)));
        }
        session.log.push(format!(
            "  Session: {}  Transport: {}",
            setup.header("session").unwrap_or("—"),
            setup.header("transport").unwrap_or("—")
        ));
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
        let mut audio = None;
        let mut aac = None;
        if let Some(track) = audio_track(&sdp).filter(|_| with_audio) {
            let playable = !matches!(track.info.codec, HikAudioCodec::Other(_));
            if playable {
                *audio_tried = true;
            }
            let accepted = !playable
                || session
                    .request(
                        "SETUP",
                        &track_url(&track.control, host, &base),
                        &[
                            ("Transport", "RTP/AVP/TCP;unicast;interleaved=2-3"),
                            ("Session", &session_id),
                        ],
                        &[],
                    )
                    .is_ok_and(|reply| reply.status == 200);
            if accepted {
                audio = Some(track.info);
                aac = track.aac;
            }
        }
        let play = session.request(
            "PLAY",
            &play_url,
            &[("Session", &session_id), ("Range", "npt=0.000-")],
            &[],
        )?;
        if play.status != 200 {
            return Err(Error::Protocol(format!("RTSP PLAY вернул {}", play.status)));
        }
        Ok(Negotiated {
            sdp,
            codec,
            audio,
            aac,
            session_id,
            play_url,
        })
    }
}

impl HikStream {
    pub fn sdp(&self) -> &str {
        &self.sdp
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// `None` when the camera stream carries no audio track.
    pub fn audio(&self) -> Option<&HikAudio> {
        self.audio.as_ref()
    }

    pub fn clone_socket(&self) -> Result<TcpStream, Error> {
        self.rtsp.stream.try_clone().map_err(Error::Io)
    }

    pub fn read_nal(&mut self) -> Result<HikNal, Error> {
        loop {
            if let HikPiece::Video(nal) = self.read_piece()? {
                return Ok(nal);
            }
        }
    }

    pub fn read_piece(&mut self) -> Result<HikPiece, Error> {
        if let Some(nal) = self.pending.pop() {
            return Ok(HikPiece::Video(nal));
        }
        if let Some(frame) = self.pending_audio.pop() {
            return Ok(HikPiece::Audio(frame));
        }
        loop {
            let (channel, packet) = read_interleaved(&mut self.rtsp.stream, &mut self.rtsp.pipe)?;
            if channel == 2 {
                let mut frames = self.depay_audio(&packet);
                if frames.is_empty() {
                    continue;
                }
                let first = frames.remove(0);
                self.pending_audio.extend(frames.into_iter().rev());
                return Ok(HikPiece::Audio(first));
            }
            if let Some(mut nals) = self.depay(&packet) {
                let timestamp = packet
                    .get(4..8)
                    .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                    .unwrap_or(0);
                for nal in &mut nals {
                    nal.timestamp = timestamp;
                }
                let mut nals = self.with_params(nals);
                if nals.is_empty() {
                    continue;
                }
                let first = nals.remove(0);
                self.pending.extend(nals.into_iter().rev());
                return Ok(HikPiece::Video(first));
            }
        }
    }

    fn with_params(&mut self, nals: Vec<HikNal>) -> Vec<HikNal> {
        let mut out = Vec::with_capacity(nals.len() + self.params.len());
        for nal in nals {
            let kind = nal_kind(&nal);
            if is_parameter_set(nal.codec, kind) {
                self.have_params = true;
            } else if is_random_access(nal.codec, kind) {
                if !self.have_params {
                    out.extend(self.params.iter().map(|param| HikNal {
                        timestamp: nal.timestamp,
                        ..param.clone()
                    }));
                }
                self.have_params = false;
            }
            out.push(nal);
        }
        out
    }

    fn depay_audio(&mut self, rtp: &[u8]) -> Vec<Vec<u8>> {
        let Some(payload) = rtp_payload(rtp) else {
            return Vec::new();
        };
        if payload.is_empty() {
            return Vec::new();
        }
        match self.audio.as_ref().map(|audio| &audio.codec) {
            Some(HikAudioCodec::Aac) => self.depay_aac(payload),
            Some(HikAudioCodec::Pcmu | HikAudioCodec::Pcma | HikAudioCodec::L16) => {
                vec![payload.to_vec()]
            }
            _ => Vec::new(),
        }
    }

    /// RFC 3640 (mpeg4-generic): AU headers, then the access units back to back.
    fn depay_aac(&mut self, payload: &[u8]) -> Vec<Vec<u8>> {
        let Some(format) = self.aac.clone() else {
            return Vec::new();
        };
        if format.size_length == 0 {
            return vec![adts_frame(&format, payload)];
        }
        if payload.len() < 2 {
            return Vec::new();
        }
        let header_bits = u16::from_be_bytes([payload[0], payload[1]]) as usize;
        let header_end = 2 + header_bits.div_ceil(8);
        let (Some(headers), Some(mut data)) = (payload.get(2..header_end), payload.get(header_end..))
        else {
            return Vec::new();
        };
        let mut bits = BitReader::new(headers);
        let mut sizes = Vec::new();
        while bits.used() < header_bits {
            let index_length = if sizes.is_empty() {
                format.index_length
            } else {
                format.index_delta_length
            };
            if bits.used() + format.size_length + index_length > header_bits {
                break;
            }
            sizes.push(bits.read(format.size_length));
            bits.read(index_length);
        }
        if let [size] = sizes[..] {
            if size > data.len() {
                self.aac_partial.extend_from_slice(data);
                if self.aac_partial.len() < size {
                    return Vec::new();
                }
                let unit = std::mem::take(&mut self.aac_partial);
                return vec![adts_frame(&format, &unit[..size])];
            }
        }
        self.aac_partial.clear();
        let mut frames = Vec::with_capacity(sizes.len());
        for size in sizes {
            if size == 0 || size > data.len() {
                break;
            }
            frames.push(adts_frame(&format, &data[..size]));
            data = &data[size..];
        }
        frames
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
        } else if nal_type == 48 {
            self.fu.clear();
            let mut nals = Vec::new();
            let mut index = 2;
            while index + 2 <= payload.len() {
                let size = u16::from_be_bytes([payload[index], payload[index + 1]]) as usize;
                index += 2;
                if size == 0 || index + size > payload.len() {
                    break;
                }
                nals.push(annex_b(payload[index..index + size].to_vec(), Codec::H265));
                index += size;
            }
            if nals.is_empty() { None } else { Some(nals) }
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
    let channels = read_channels(host, port, user, password);
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HikRecord {
    /// Device-local `YYYY-MM-DD HH:MM:SS`.
    pub begin: String,
    pub end: String,
    /// `playbackURI` as the device gave it; also the handle for [`download`].
    pub uri: String,
    pub bytes: u64,
}

/// Recordings of `track` (`{channel}01`) between two device-local `YYYY-MM-DD HH:MM:SS` stamps.
pub fn search(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    track: u32,
    begin: &str,
    end: &str,
) -> Result<Vec<HikRecord>, Error> {
    let search_id = search_id();
    let mut found: Vec<HikRecord> = Vec::new();
    let mut position = 0usize;
    for _ in 0..40 {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<CMSearchDescription><searchID>{search_id}</searchID>\
<trackList><trackID>{track}</trackID></trackList>\
<timeSpanList><timeSpan><startTime>{}</startTime><endTime>{}</endTime></timeSpan></timeSpanList>\
<maxResults>50</maxResults><searchResultPostion>{position}</searchResultPostion>\
<metadataList><metadataDescriptor>//recordType.meta.std-cgi.com</metadataDescriptor></metadataList>\
</CMSearchDescription>",
            isapi_time(begin),
            isapi_time(end)
        );
        let response = http(
            host,
            port,
            user,
            password,
            "POST",
            "/ISAPI/ContentMgmt/search",
            Some((&body, "application/xml")),
        )?;
        ensure_ok(&response, "поиск записей не удался")?;
        let text = String::from_utf8_lossy(&response.body);
        let page = parse_search(&text);
        let count = page.len();
        for item in page {
            if !found.iter().any(|known| known.uri == item.uri) {
                found.push(item);
            }
        }
        let more = xml_text(&text, "responseStatusStrg").is_some_and(|value| value.eq_ignore_ascii_case("MORE"));
        if !more || count == 0 {
            break;
        }
        position += count;
    }
    found.sort_by(|left, right| left.begin.cmp(&right.begin));
    Ok(found)
}

fn parse_search(xml: &str) -> Vec<HikRecord> {
    xml_blocks(xml, "searchMatchItem")
        .into_iter()
        .filter_map(|block| {
            let uri = xml_text(block, "playbackURI")?;
            let bytes = uri_param(&uri, "size").and_then(|size| size.parse().ok()).unwrap_or(0);
            Some(HikRecord {
                begin: stamp_from_isapi(&xml_text(block, "startTime")?),
                end: stamp_from_isapi(&xml_text(block, "endTime")?),
                uri,
                bytes,
            })
        })
        .collect()
}

fn uri_param(uri: &str, key: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        name.eq_ignore_ascii_case(key).then(|| value.to_string())
    })
}

fn search_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let hex = format!("{:032X}", nanos ^ 0x5EED_0B5E_47E4_A11C_E5A7_C0DE_D00D_F00D);
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// `playbackURI` of a time range that is not a whole recorded file.
pub fn playback_uri(host: &str, track: u32, begin: &str, end: &str) -> String {
    format!(
        "rtsp://{host}/Streaming/tracks/{track}/?starttime={}&endtime={}",
        compact_time(begin),
        compact_time(end)
    )
}

/// Saves a recording as MPEG-PS (Hikvision's 40-byte `IMKH` file header is dropped).
/// `progress` gets the bytes written so far and returns `false` to cancel.
pub fn download(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    uri: &str,
    out: &mut dyn Write,
    mut progress: impl FnMut(u64) -> bool,
) -> Result<u64, Error> {
    let escaped = uri.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<downloadRequest version=\"1.0\" xmlns=\"http://www.isapi.org/ver20/XMLSchema\">\
<playbackURI>{escaped}</playbackURI></downloadRequest>"
    );
    let path = "/ISAPI/ContentMgmt/download";
    let mut sink = PsSink::new(out);
    let first = http_streamed(host, port, "GET", path, &body, None, &mut sink, &mut progress)?;
    let (status, headers, detail) = match first {
        Streamed::Done(written) => return Ok(written),
        Streamed::Refused { status, headers, body } => (status, headers, body),
    };
    if status != 401 {
        return Err(isapi_refusal(status, &detail, "регистратор не отдал запись"));
    }
    let authorization = match pick_challenge(&headers) {
        Some(challenge) => {
            let cnonce = format!("{:016x}", challenge.nonce.len() as u64 ^ 0x0d0c_u64.wrapping_mul(port as u64));
            authorization("GET", path, user, password, &challenge, 1, &cnonce)
        }
        None if offers_basic(&headers) => basic_authorization(user, password),
        None => return Err(Error::Protocol("устройство не предложило поддерживаемую авторизацию".into())),
    };
    match http_streamed(host, port, "GET", path, &body, Some(&authorization), &mut sink, &mut progress)? {
        Streamed::Done(written) => Ok(written),
        Streamed::Refused { status, body, .. } => {
            Err(isapi_refusal(status, &body, "регистратор не отдал запись"))
        }
    }
}

fn isapi_refusal(status: u16, body: &[u8], what: &str) -> Error {
    if status == 401 {
        return Error::Device {
            code: 401,
            message: "неверный логин или пароль".into(),
        };
    }
    let text = String::from_utf8_lossy(body);
    let detail = xml_text(&text, "subStatusCode")
        .or_else(|| xml_text(&text, "statusString"))
        .map(|detail| format!(", {detail}"))
        .unwrap_or_default();
    Error::Device {
        code: i64::from(status),
        message: format!("{what} ({status}{detail})"),
    }
}

struct PsSink<'a> {
    out: &'a mut dyn Write,
    head: Vec<u8>,
    written: u64,
}

impl<'a> PsSink<'a> {
    fn new(out: &'a mut dyn Write) -> Self {
        Self {
            out,
            head: Vec::new(),
            written: 0,
        }
    }

    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        const HEADER: usize = 40;
        if self.head.len() < HEADER && self.written == 0 {
            self.head.extend_from_slice(data);
            if self.head.len() < HEADER {
                return Ok(());
            }
            let head = std::mem::take(&mut self.head);
            let skip = if head.starts_with(b"IMKH") { HEADER } else { 0 };
            self.written += (head.len() - skip) as u64;
            return self.out.write_all(&head[skip..]);
        }
        self.written += data.len() as u64;
        self.out.write_all(data)
    }

    fn finish(&mut self) -> std::io::Result<u64> {
        if !self.head.is_empty() {
            let head = std::mem::take(&mut self.head);
            self.written += head.len() as u64;
            self.out.write_all(&head)?;
        }
        self.out.flush()?;
        Ok(self.written)
    }
}

enum Streamed {
    Done(u64),
    Refused {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
}

#[allow(clippy::too_many_arguments)]
fn http_streamed(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    body: &str,
    authorization: Option<&str>,
    sink: &mut PsSink,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<Streamed, Error> {
    let mut stream = connect_timeout(host, port)?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(MEDIA_GAP)).ok();
    stream.set_write_timeout(Some(HANDSHAKE)).ok();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: eXUnity Observer\r\nConnection: close\r\n\
Content-Type: application/xml\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(authorization) = authorization {
        request.push_str("Authorization: ");
        request.push_str(authorization);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).map_err(Error::Io)?;
    stream.flush().map_err(Error::Io)?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 65536];
    let split = loop {
        let count = stream.read(&mut buf).map_err(map_timeout)?;
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
    let (status, headers) = parse_headers(&head)?;
    let chunked = headers
        .iter()
        .any(|(name, value)| name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked"));
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<u64>().ok());
    let rest = raw[split + 4..].to_vec();
    let source = std::io::Cursor::new(rest).chain(stream);
    let mut reader: Box<dyn Read> = if chunked {
        Box::new(ChunkedReader::new(std::io::BufReader::new(source)))
    } else if let Some(length) = length {
        Box::new(source.take(length))
    } else {
        Box::new(source)
    };
    if status != 200 {
        let mut body = Vec::new();
        let _ = reader.take(64 * 1024).read_to_end(&mut body);
        return Ok(Streamed::Refused { status, headers, body });
    }
    loop {
        let count = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(count) => count,
            Err(err) => return Err(map_timeout(err)),
        };
        sink.write(&buf[..count]).map_err(Error::Io)?;
        if !progress(sink.written) {
            return Err(Error::Cancelled);
        }
    }
    let written = sink.finish().map_err(Error::Io)?;
    if written == 0 {
        return Err(Error::Protocol("регистратор прислал пустой файл".into()));
    }
    Ok(Streamed::Done(written))
}

struct ChunkedReader<R: std::io::BufRead> {
    inner: R,
    left: u64,
    done: bool,
}

impl<R: std::io::BufRead> ChunkedReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            left: 0,
            done: false,
        }
    }
}

impl<R: std::io::BufRead> Read for ChunkedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut line = String::new();
            if self.inner.read_line(&mut line)? == 0 {
                self.done = true;
                return Ok(0);
            }
            let size = u64::from_str_radix(line.trim().split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "размер куска не разобран"))?;
            if size == 0 {
                self.done = true;
                return Ok(0);
            }
            self.left = size;
        }
        let want = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let count = self.inner.read(&mut buf[..want])?;
        if count == 0 {
            self.done = true;
            return Ok(0);
        }
        self.left -= count as u64;
        if self.left == 0 {
            let mut crlf = String::new();
            self.inner.read_line(&mut crlf)?;
        }
        Ok(count)
    }
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
        return Err(isapi_refusal(response.status, &response.body, what));
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
    let authorization = match pick_challenge(&first.headers) {
        Some(challenge) => {
            let cnonce =
                format!("{:016x}", challenge.nonce.len() as u64 ^ 0x5a17_u64.wrapping_mul(port as u64));
            authorization(method, path, user, password, &challenge, 1, &cnonce)
        }
        None if offers_basic(&first.headers) => basic_authorization(user, password),
        None => return Err(Error::Protocol("устройство не предложило поддерживаемую авторизацию".into())),
    };
    http_once(host, port, method, path, body, Some(&authorization))
}

/// Devices may offer several `WWW-Authenticate` headers (Basic, Digest SHA-256, Digest MD5).
fn pick_challenge(headers: &[(String, String)]) -> Option<Challenge> {
    headers
        .iter()
        .filter(|(name, value)| {
            name == "www-authenticate" && value.trim_start().to_ascii_lowercase().starts_with("digest")
        })
        .filter(|(_, value)| {
            let params = digest_params(value.trim_start()[6..].trim());
            param(&params, "algorithm").is_none_or(|algorithm| algorithm.eq_ignore_ascii_case("md5"))
        })
        .find_map(|(_, value)| parse_digest(value))
}

fn offers_basic(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(name, value)| {
        name == "www-authenticate" && value.trim_start().to_ascii_lowercase().starts_with("basic")
    })
}

fn basic_authorization(user: &str, password: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = format!("{user}:{password}");
    let mut out = String::from("Basic ");
    for chunk in input.as_bytes().chunks(3) {
        let bytes = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(TABLE[(value >> (18 - index * 6)) as usize & 0x3f] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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
    let chunked = headers
        .iter()
        .any(|(name, value)| name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked"));
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok());
    let body = if chunked {
        loop {
            let (body, complete) = decode_chunked(&rest);
            if complete {
                break body;
            }
            match stream.read(&mut buf) {
                Ok(count) if count > 0 => rest.extend_from_slice(&buf[..count]),
                _ => break body,
            }
        }
    } else if let Some(length) = length {
        while rest.len() < length {
            match stream.read(&mut buf) {
                Ok(count) if count > 0 => rest.extend_from_slice(&buf[..count]),
                _ => break,
            }
        }
        rest.truncate(length);
        rest
    } else {
        // `Connection: close` was requested, so the body runs until the device hangs up.
        while let Ok(count) = stream.read(&mut buf) {
            if count == 0 {
                break;
            }
            rest.extend_from_slice(&buf[..count]);
        }
        rest
    };
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// Returns the body decoded so far and whether the terminating zero-size chunk was reached.
fn decode_chunked(data: &[u8]) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let Some(line_end) = data[pos..].windows(2).position(|pair| pair == b"\r\n") else {
            return (out, false);
        };
        let size_text = String::from_utf8_lossy(&data[pos..pos + line_end]);
        let Ok(size) = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
        else {
            return (out, false);
        };
        pos += line_end + 2;
        if size == 0 {
            return (out, true);
        }
        if data.len() < pos + size {
            out.extend_from_slice(&data[pos..]);
            return (out, false);
        }
        out.extend_from_slice(&data[pos..pos + size]);
        pos += size + 2;
        if pos > data.len() {
            return (out, false);
        }
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
    basic: bool,
    log: Vec<String>,
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
            basic: false,
            log: Vec::new(),
        }
    }

    fn request(
        &mut self,
        method: &str,
        uri: &str,
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> Result<RtspResponse, Error> {
        // Once challenged, sign every request up front: some NVRs reset the session on each 401
        // and then answer the signed retry of PLAY with 455.
        let signed = self.challenge.is_some() || self.basic;
        let response = self.send(method, uri, extra, body, signed)?;
        if response.status != 401 {
            self.log.push(format!("{method} {uri} → {}", response.status));
            return Ok(response);
        }
        self.challenge = pick_challenge(&response.headers);
        self.basic = self.challenge.is_none() && offers_basic(&response.headers);
        if self.challenge.is_none() && !self.basic {
            return Err(Error::Protocol("RTSP не предложил поддерживаемую авторизацию".into()));
        }
        self.nc = 0;
        let response = self.send(method, uri, extra, body, true)?;
        let scheme = if self.basic { "Basic" } else { "Digest" };
        self.log.push(format!("{method} {uri} → 401, {scheme} → {}", response.status));
        Ok(response)
    }

    fn send(
        &mut self,
        method: &str,
        uri: &str,
        extra: &[(&str, &str)],
        body: &[u8],
        authorized: bool,
    ) -> Result<RtspResponse, Error> {
        let text = self.compose(method, uri, extra, body, authorized);
        self.stream.set_read_timeout(Some(HANDSHAKE)).ok();
        self.stream.write_all(text.as_bytes()).map_err(Error::Io)?;
        self.stream.write_all(body).map_err(Error::Io)?;
        self.stream.flush().map_err(Error::Io)?;
        read_rtsp(&mut self.stream, &mut self.pipe)
    }

    /// A signed request mid-stream; its reply is dropped by [`read_interleaved`].
    fn send_only(&mut self, method: &str, uri: &str, extra: &[(&str, &str)]) -> Result<(), Error> {
        let signed = self.challenge.is_some() || self.basic;
        let text = self.compose(method, uri, extra, &[], signed);
        self.stream.write_all(text.as_bytes()).map_err(Error::Io)?;
        self.stream.flush().map_err(Error::Io)
    }

    fn compose(
        &mut self,
        method: &str,
        uri: &str,
        extra: &[(&str, &str)],
        body: &[u8],
        authorized: bool,
    ) -> String {
        self.cseq += 1;
        let mut text = format!(
            "{method} {uri} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: eXUnity Observer\r\n",
            self.cseq
        );
        if authorized {
            let header = if let Some(challenge) = &self.challenge {
                self.nc += 1;
                let cnonce = format!("{:016x}", self.cseq as u64 ^ 0xA5A5_5A5A);
                Some(authorization(method, uri, &self.user, &self.password, challenge, self.nc, &cnonce))
            } else if self.basic {
                Some(basic_authorization(&self.user, &self.password))
            } else {
                None
            };
            if let Some(header) = header {
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
        text
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

fn read_interleaved(stream: &mut TcpStream, pipe: &mut BytePipe) -> Result<(u8, Vec<u8>), Error> {
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
        if channel == 0 || channel == 2 {
            return Ok((channel, payload));
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
    let mut end = packet.len();
    if packet[0] & 0x20 != 0 {
        end = end.saturating_sub(usize::from(*packet.last()?));
    }
    packet.get(offset..end.max(offset))
}

fn nal_kind(nal: &HikNal) -> u8 {
    let header = nal.data.get(4).copied().unwrap_or(0);
    match nal.codec {
        Codec::H265 => (header >> 1) & 0x3f,
        _ => header & 0x1f,
    }
}

fn is_parameter_set(codec: Codec, kind: u8) -> bool {
    match codec {
        Codec::H265 => matches!(kind, 32..=34),
        _ => matches!(kind, 7 | 8),
    }
}

fn is_random_access(codec: Codec, kind: u8) -> bool {
    match codec {
        Codec::H265 => matches!(kind, 16..=21),
        _ => kind == 5,
    }
}

fn video_fmtp(sdp: &str) -> Option<String> {
    let mut video = false;
    for line in sdp.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("m=") {
            video = rest.starts_with("video");
        } else if video {
            if let Some(value) = line.strip_prefix("a=fmtp:") {
                return value.split_once(' ').map(|(_, params)| params.trim().to_string());
            }
        }
    }
    None
}

fn sprop_params(sdp: &str, codec: Codec) -> Vec<HikNal> {
    let params = fmtp_params(&video_fmtp(sdp).unwrap_or_default());
    let encoded: Vec<&str> = match codec {
        Codec::H265 => ["sprop-vps", "sprop-sps", "sprop-pps"]
            .iter()
            .filter_map(|key| params.get(*key).map(String::as_str))
            .collect(),
        _ => params
            .get("sprop-parameter-sets")
            .map(|sets| sets.split(',').collect())
            .unwrap_or_default(),
    };
    encoded
        .into_iter()
        .filter_map(|text| base64_decode(text.trim()))
        .filter(|nal| !nal.is_empty())
        .map(|nal| annex_b(nal, codec))
        .collect()
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\r' | b'\n' | b'\t' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
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
        timestamp: 0,
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

/// The session-level `a=control:` line, before the first `m=` section.
fn session_control(sdp: &str) -> Option<String> {
    for line in sdp.lines() {
        let line = line.trim();
        if line.starts_with("m=") {
            return None;
        }
        if let Some(control) = line.strip_prefix("a=control:") {
            return Some(control.trim().to_string());
        }
    }
    None
}

fn track_url(control: &str, host: &str, base: &str) -> String {
    if control.starts_with("rtsp://") {
        control.to_string()
    } else if control.starts_with('/') {
        format!("rtsp://{host}:554{control}")
    } else {
        format!("{base}/{control}")
    }
}

const AAC_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

fn audio_track(sdp: &str) -> Option<AudioTrack> {
    let mut in_audio = false;
    let mut payload_type = None;
    let mut control = None;
    let mut rtpmap = None;
    let mut fmtp = None;
    for line in sdp.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("m=") {
            if in_audio {
                break;
            }
            in_audio = rest.starts_with("audio");
            if in_audio {
                payload_type = rest.split_whitespace().nth(3).map(str::to_string);
            }
            continue;
        }
        if !in_audio {
            continue;
        }
        if let Some(value) = line.strip_prefix("a=control:") {
            control = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("a=rtpmap:") {
            if let Some((kind, map)) = value.split_once(' ') {
                if Some(kind) == payload_type.as_deref() {
                    rtpmap = Some(map.trim().to_string());
                }
            }
        } else if let Some(value) = line.strip_prefix("a=fmtp:") {
            if let Some((kind, params)) = value.split_once(' ') {
                if Some(kind) == payload_type.as_deref() {
                    fmtp = Some(params.trim().to_string());
                }
            }
        }
    }
    let payload_type = payload_type?;
    let (name, rate, channels) = match &rtpmap {
        Some(map) => {
            let mut parts = map.split('/');
            let name = parts.next().unwrap_or("").to_ascii_uppercase();
            let rate = parts.next().and_then(|part| part.parse().ok()).unwrap_or(8000);
            let channels = parts.next().and_then(|part| part.parse().ok()).unwrap_or(1);
            (name, rate, channels)
        }
        None => match payload_type.as_str() {
            "0" => ("PCMU".to_string(), 8000, 1),
            "8" => ("PCMA".to_string(), 8000, 1),
            "10" => ("L16".to_string(), 44_100, 2),
            "11" => ("L16".to_string(), 44_100, 1),
            "14" => ("MPA".to_string(), 90_000, 1),
            other => (format!("RTP {other}"), 8000, 1),
        },
    };
    let params = fmtp_params(fmtp.as_deref().unwrap_or(""));
    let mut aac = None;
    let codec = match name.as_str() {
        "PCMU" => HikAudioCodec::Pcmu,
        "PCMA" => HikAudioCodec::Pcma,
        "L16" => HikAudioCodec::L16,
        "MPEG4-GENERIC"
            if params
                .get("mode")
                .is_none_or(|mode| mode.eq_ignore_ascii_case("aac-hbr") || mode.eq_ignore_ascii_case("aac-lbr")) =>
        {
            aac = Some(aac_format(&params, rate, channels));
            HikAudioCodec::Aac
        }
        "MPEG4-GENERIC" => HikAudioCodec::Other("AAC".into()),
        _ => HikAudioCodec::Other(name.clone()),
    };
    let (rate, channels) = match &aac {
        Some(format) => (
            AAC_RATES.get(format.freq_index as usize).copied().unwrap_or(rate),
            u16::from(format.channel_config.max(1)),
        ),
        None => (rate, channels),
    };
    Some(AudioTrack {
        control: control.unwrap_or_else(|| "trackID=2".into()),
        info: HikAudio {
            codec,
            rate,
            channels,
        },
        aac,
    })
}

fn fmtp_params(text: &str) -> std::collections::HashMap<String, String> {
    text.split(';')
        .filter_map(|item| item.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect()
}

fn aac_format(
    params: &std::collections::HashMap<String, String>,
    rate: u32,
    channels: u16,
) -> AacFormat {
    let number = |key: &str, default: usize| {
        params
            .get(key)
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let config = params
        .get("config")
        .map(|hex| {
            (0..hex.len() / 2)
                .filter_map(|index| u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok())
                .collect::<Vec<u8>>()
        })
        .unwrap_or_default();
    let (profile, freq_index, channel_config) = if config.len() >= 2 {
        let object = config[0] >> 3;
        let freq = ((config[0] & 0x07) << 1) | (config[1] >> 7);
        let chan = (config[1] >> 3) & 0x0f;
        // ADTS has two bits for the profile; HE-AAC (5, 29) travels as LC with implicit SBR.
        let profile = if (1..=4).contains(&object) { object - 1 } else { 1 };
        (profile, freq, chan)
    } else {
        let freq = AAC_RATES
            .iter()
            .position(|item| *item == rate)
            .unwrap_or(11) as u8;
        (1, freq, channels.clamp(1, 7) as u8)
    };
    AacFormat {
        size_length: number("sizelength", 13),
        index_length: number("indexlength", 3),
        index_delta_length: number("indexdeltalength", 3),
        profile,
        freq_index,
        channel_config,
    }
}

fn adts_frame(format: &AacFormat, unit: &[u8]) -> Vec<u8> {
    let length = unit.len() + 7;
    let mut frame = Vec::with_capacity(length);
    frame.extend_from_slice(&[
        0xFF,
        0xF1,
        (format.profile << 6) | ((format.freq_index & 0x0f) << 2) | ((format.channel_config >> 2) & 0x01),
        ((format.channel_config & 0x03) << 6) | ((length >> 11) as u8 & 0x03),
        (length >> 3) as u8,
        (((length & 0x07) as u8) << 5) | 0x1F,
        0xFC,
    ]);
    frame.extend_from_slice(unit);
    frame
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn used(&self) -> usize {
        self.bit
    }

    fn read(&mut self, count: usize) -> usize {
        let mut value = 0usize;
        for _ in 0..count {
            let byte = self.data.get(self.bit / 8).copied().unwrap_or(0);
            value = (value << 1) | usize::from((byte >> (7 - self.bit % 8)) & 1);
            self.bit += 1;
        }
        value
    }
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

/// Analog inputs of the device itself, then IP cameras of an NVR, then whatever streams it lists.
fn read_channels(host: &str, port: u16, user: &str, password: &str) -> Vec<HikChannel> {
    let get = |path: &str| match http(host, port, user, password, "GET", path, None) {
        Ok(response) if response.status == 200 => {
            Some(String::from_utf8_lossy(&response.body).to_string())
        }
        _ => None,
    };
    let mut channels = get("/ISAPI/System/Video/inputs/channels")
        .map(|xml| parse_channels(&xml, "VideoInputChannel"))
        .unwrap_or_default();
    if let Some(xml) = get("/ISAPI/ContentMgmt/InputProxy/channels") {
        let online = get("/ISAPI/ContentMgmt/InputProxy/channels/status")
            .map(|xml| parse_online(&xml))
            .unwrap_or_default();
        for mut channel in parse_channels(&xml, "InputProxyChannel") {
            if channels.iter().any(|known| known.id == channel.id) {
                continue;
            }
            channel.online = online.get(&channel.id).copied().unwrap_or(true);
            channels.push(channel);
        }
    }
    if channels.is_empty() {
        channels = get("/ISAPI/Streaming/channels")
            .map(|xml| parse_streaming(&xml))
            .unwrap_or_default();
    }
    if channels.is_empty() {
        channels.push(HikChannel {
            id: 1,
            name: "Камера 1".into(),
            online: true,
        });
    }
    channels
}

fn parse_channels(xml: &str, tag: &str) -> Vec<HikChannel> {
    xml_blocks(xml, tag)
        .into_iter()
        .filter_map(|block| {
            let id = xml_text(block, "id")?.parse::<u16>().ok()?;
            let name = xml_text(block, "name").unwrap_or_else(|| format!("Камера {id}"));
            Some(HikChannel {
                id,
                name,
                online: true,
            })
        })
        .collect()
}

fn parse_online(xml: &str) -> std::collections::HashMap<u16, bool> {
    xml_blocks(xml, "InputProxyChannelStatus")
        .into_iter()
        .filter_map(|block| {
            let id = xml_text(block, "id")?.parse::<u16>().ok()?;
            let online = xml_text(block, "online").is_some_and(|value| value.eq_ignore_ascii_case("true"));
            Some((id, online))
        })
        .collect()
}

fn parse_streaming(xml: &str) -> Vec<HikChannel> {
    let mut channels: Vec<HikChannel> = Vec::new();
    for block in xml_blocks(xml, "StreamingChannel") {
        let Some(stream) = xml_text(block, "id").and_then(|id| id.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(id) = u16::try_from(stream / 100) else {
            continue;
        };
        if id == 0 || channels.iter().any(|known| known.id == id) {
            continue;
        }
        let name = xml_text(block, "channelName").unwrap_or_else(|| format!("Камера {id}"));
        channels.push(HikChannel {
            id,
            name,
            online: true,
        });
    }
    channels
}

/// Bodies of every `<tag>` / `<tag attr=…>` element; `<tagList>` and other longer names don't match.
fn xml_blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut blocks = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        if !after.starts_with(['>', ' ', '\t', '\r', '\n']) {
            rest = after;
            continue;
        }
        let Some(body_start) = after.find('>').map(|index| index + 1) else {
            break;
        };
        let body = &after[body_start..];
        let Some(end) = body.find(&close) else {
            break;
        };
        blocks.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    blocks
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
        let xml = r#"<VideoInputChannelList version="2.0"><VideoInputChannel><id>1</id><name>Door</name></VideoInputChannel></VideoInputChannelList>"#;
        assert_eq!(
            parse_channels(xml, "VideoInputChannel"),
            vec![HikChannel {
                id: 1,
                name: "Door".into(),
                online: true
            }]
        );
    }

    #[test]
    fn nvr_ip_channels_keep_their_ids_and_status() {
        let list = r#"<?xml version="1.0" encoding="UTF-8"?>
<InputProxyChannelList version="2.0" xmlns="http://www.hikvision.com/ver20/XMLSchema">
<InputProxyChannel version="2.0">
<id>33</id>
<name>Двор</name>
<sourceInputPortDescriptor>
<proxyProtocol>HIKVISION</proxyProtocol>
<ipAddress>192.168.1.64</ipAddress>
<managePortNo>8000</managePortNo>
<srcInputPort>1</srcInputPort>
<userName>admin</userName>
</sourceInputPortDescriptor>
</InputProxyChannel>
<InputProxyChannel version="2.0">
<id>34</id>
<name>Вход &amp; калитка</name>
</InputProxyChannel>
</InputProxyChannelList>"#;
        let status = r#"<InputProxyChannelStatusList version="2.0">
<InputProxyChannelStatus><id>33</id><online>true</online></InputProxyChannelStatus>
<InputProxyChannelStatus><id>34</id><online>false</online></InputProxyChannelStatus>
</InputProxyChannelStatusList>"#;
        let channels = parse_channels(list, "InputProxyChannel");
        assert_eq!(channels.iter().map(|channel| channel.id).collect::<Vec<_>>(), vec![33, 34]);
        assert_eq!(channels[1].name, "Вход & калитка");
        let online = parse_online(status);
        assert_eq!(online.get(&33), Some(&true));
        assert_eq!(online.get(&34), Some(&false));
    }

    fn reply_once(response: &'static [u8]) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request);
            socket.write_all(response).unwrap();
        });
        port
    }

    #[test]
    fn http_reads_chunked_body() {
        let port = reply_once(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/xml\r\n\r\n\
              7\r\n<id>33<\r\n6\r\n/id>ok\r\n0\r\n\r\n",
        );
        let response = http("127.0.0.1", port, "admin", "", "GET", "/ISAPI/x", None).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(String::from_utf8_lossy(&response.body), "<id>33</id>ok");
    }

    #[test]
    fn http_reads_body_until_close_without_length() {
        let port = reply_once(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n<name>NVR</name>");
        let response = http("127.0.0.1", port, "admin", "", "GET", "/ISAPI/x", None).unwrap();
        assert_eq!(String::from_utf8_lossy(&response.body), "<name>NVR</name>");
    }

    #[test]
    fn digest_md5_is_picked_among_several_challenges() {
        let headers = vec![
            ("www-authenticate".to_string(), "Basic realm=\"DS\"".to_string()),
            (
                "www-authenticate".to_string(),
                "Digest realm=\"DS\", nonce=\"sha\", algorithm=SHA-256, qop=\"auth\"".to_string(),
            ),
            (
                "www-authenticate".to_string(),
                "Digest realm=\"DS\", nonce=\"md5\", qop=\"auth\"".to_string(),
            ),
        ];
        assert_eq!(pick_challenge(&headers).unwrap().nonce, "md5");
        assert!(offers_basic(&headers));
        assert!(pick_challenge(&headers[..1]).is_none());
        assert_eq!(basic_authorization("admin", "12345"), "Basic YWRtaW46MTIzNDU=");
        assert_eq!(basic_authorization("a", "b"), "Basic YTpi");
    }

    #[test]
    fn streaming_list_gives_one_channel_per_camera() {
        let xml = r#"<StreamingChannelList>
<StreamingChannel><id>101</id><channelName>Камера 1</channelName></StreamingChannel>
<StreamingChannel><id>102</id><channelName>Камера 1</channelName></StreamingChannel>
<StreamingChannel><id>3301</id><channelName>IP 1</channelName></StreamingChannel>
</StreamingChannelList>"#;
        let channels = parse_streaming(xml);
        assert_eq!(channels.iter().map(|channel| channel.id).collect::<Vec<_>>(), vec![1, 33]);
        assert_eq!(channels[1].name, "IP 1");
    }

    #[test]
    fn h264_fragment_becomes_idr() {
        let mut stream = HikStream {
            rtsp: Rtsp::new(local_socket(), "", ""),
            session_id: String::new(),
            play_url: String::new(),
            codec: Codec::H264,
            fu: Vec::new(),
            pending: Vec::new(),
            audio: None,
            aac: None,
            aac_partial: Vec::new(),
            pending_audio: Vec::new(),
            sdp: String::new(),
            params: Vec::new(),
            have_params: false,
        };
        let start = [0x7c, 0x85, 0x11];
        let end = [0x7c, 0x45, 0x22];
        assert!(stream.depay_h264(&start).is_none());
        let nal = stream.depay_h264(&end).unwrap();
        assert!(nal[0].keyframe);
        assert_eq!(&nal[0].data[..5], &[0, 0, 0, 1, 0x65]);
        assert_eq!(&nal[0].data[5..], &[0x11, 0x22]);
    }

    const SDP_G711: &str = "v=0\r\n\
        m=video 0 RTP/AVP 96\r\n\
        a=rtpmap:96 H264/90000\r\n\
        a=control:rtsp://10.0.0.5:554/Streaming/Channels/101/trackID=1\r\n\
        m=audio 0 RTP/AVP 0\r\n\
        a=rtpmap:0 PCMU/8000\r\n\
        a=control:rtsp://10.0.0.5:554/Streaming/Channels/101/trackID=2\r\n";

    const SDP_AAC: &str = "v=0\r\n\
        m=video 0 RTP/AVP 96\r\n\
        a=rtpmap:96 H265/90000\r\n\
        a=control:trackID=1\r\n\
        m=audio 0 RTP/AVP 104\r\n\
        a=rtpmap:104 mpeg4-generic/16000/1\r\n\
        a=fmtp:104 profile-level-id=15; streamtype=5; mode=AAC-hbr; config=1408; SizeLength=13; IndexLength=3; IndexDeltaLength=3; Profile=1;\r\n\
        a=control:trackID=2\r\n";

    fn stream_with(sdp: &str) -> HikStream {
        let track = audio_track(sdp).unwrap();
        HikStream {
            rtsp: Rtsp::new(local_socket(), "", ""),
            session_id: String::new(),
            play_url: String::new(),
            codec: Codec::H264,
            fu: Vec::new(),
            pending: Vec::new(),
            audio: Some(track.info),
            aac: track.aac,
            aac_partial: Vec::new(),
            pending_audio: Vec::new(),
            sdp: sdp.to_string(),
            params: Vec::new(),
            have_params: false,
        }
    }

    #[test]
    fn sdp_audio_track_g711() {
        let track = audio_track(SDP_G711).unwrap();
        assert_eq!(track.info.codec, HikAudioCodec::Pcmu);
        assert_eq!(track.info.rate, 8000);
        assert_eq!(track.control, "rtsp://10.0.0.5:554/Streaming/Channels/101/trackID=2");
        assert_eq!(video_control(SDP_G711), "rtsp://10.0.0.5:554/Streaming/Channels/101/trackID=1");
    }

    #[test]
    fn sdp_audio_track_aac_reads_config() {
        let track = audio_track(SDP_AAC).unwrap();
        assert_eq!(track.info.codec, HikAudioCodec::Aac);
        assert_eq!(track.info.rate, 16_000);
        assert_eq!(track.info.channels, 1);
        let aac = track.aac.unwrap();
        assert_eq!((aac.profile, aac.freq_index, aac.channel_config), (1, 8, 1));
        assert_eq!((aac.size_length, aac.index_length, aac.index_delta_length), (13, 3, 3));
        assert_eq!(track_url(&track.control, "h", "rtsp://h:554/S/101"), "rtsp://h:554/S/101/trackID=2");
    }

    #[test]
    fn sdp_without_audio_or_with_unknown_codec() {
        let video_only = "m=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n";
        assert!(audio_track(video_only).is_none());
        let g726 = "m=audio 0 RTP/AVP 97\r\na=rtpmap:97 G726-32/8000\r\n";
        assert_eq!(
            audio_track(g726).unwrap().info.codec,
            HikAudioCodec::Other("G726-32".into())
        );
    }

    fn rtp(payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x80, 0x68, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        packet.extend_from_slice(payload);
        packet
    }

    #[test]
    fn aac_units_become_adts_frames() {
        let mut stream = stream_with(SDP_AAC);
        let headers = [0x00, 0x20, 0x00, 3 << 3, 0x00, 2 << 3];
        let mut payload = headers.to_vec();
        payload.extend_from_slice(&[1, 2, 3, 4, 5]);
        let frames = stream.depay_audio(&rtp(&payload));
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0][..7], [0xFF, 0xF1, 0x60, 0x40, 0x01, 0x5F, 0xFC]);
        assert_eq!(&frames[0][7..], &[1, 2, 3]);
        assert_eq!(&frames[1][7..], &[4, 5]);
    }

    #[test]
    fn fragmented_aac_unit_is_joined() {
        let mut stream = stream_with(SDP_AAC);
        let header = [0x00, 0x10, (6 >> 5) as u8, ((6 & 0x1f) << 3) as u8];
        let mut first = header.to_vec();
        first.extend_from_slice(&[1, 2, 3, 4]);
        let mut second = header.to_vec();
        second.extend_from_slice(&[5, 6]);
        assert!(stream.depay_audio(&rtp(&first)).is_empty());
        let frames = stream.depay_audio(&rtp(&second));
        assert_eq!(frames.len(), 1);
        assert_eq!(&frames[0][7..], &[1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn g711_payload_passes_through() {
        let mut stream = stream_with(SDP_G711);
        assert_eq!(stream.depay_audio(&rtp(&[0xFF, 0x7F])), vec![vec![0xFF, 0x7F]]);
    }

    fn video_stream(codec: Codec, sdp: &str) -> HikStream {
        HikStream {
            rtsp: Rtsp::new(local_socket(), "", ""),
            session_id: String::new(),
            play_url: String::new(),
            codec,
            fu: Vec::new(),
            pending: Vec::new(),
            audio: None,
            aac: None,
            aac_partial: Vec::new(),
            pending_audio: Vec::new(),
            sdp: sdp.to_string(),
            params: sprop_params(sdp, codec),
            have_params: false,
        }
    }

    fn read_request(socket: &mut TcpStream) -> String {
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        while !raw.ends_with(b"\r\n\r\n") {
            if socket.read(&mut byte).unwrap() == 0 {
                break;
            }
            raw.push(byte[0]);
        }
        String::from_utf8_lossy(&raw).to_string()
    }

    #[test]
    fn rtsp_signs_setup_and_play_after_the_first_challenge() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let challenge = "RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\n\
                WWW-Authenticate: Digest realm=\"NVR\", nonce=\"n1\"\r\n\r\n";
            let request = read_request(&mut socket);
            assert!(request.starts_with("DESCRIBE") && !request.contains("Authorization"));
            socket.write_all(challenge.as_bytes()).unwrap();
            let request = read_request(&mut socket);
            assert!(request.contains("Authorization: Digest"));
            let sdp = "v=0\r\na=control:*\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=control:trackID=video\r\n";
            socket
                .write_all(
                    format!(
                        "RTSP/1.0 200 OK\r\nCSeq: 2\r\nContent-Base: rtsp://127.0.0.1:{port}/Streaming/Channels/101/\r\n\
                         Content-Length: {}\r\n\r\n{sdp}",
                        sdp.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
            for method in ["SETUP", "PLAY"] {
                let request = read_request(&mut socket);
                assert!(request.starts_with(method), "{request}");
                if !request.contains("Authorization: Digest") {
                    socket.write_all(b"RTSP/1.0 455 Method Not Valid in This State\r\n\r\n").unwrap();
                    return;
                }
                if method == "SETUP" {
                    assert!(request.contains("/Streaming/Channels/101/trackID=video"));
                    socket
                        .write_all(b"RTSP/1.0 200 OK\r\nSession: 77;timeout=60\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n")
                        .unwrap();
                } else {
                    assert!(request.contains("Session: 77"));
                    socket.write_all(b"RTSP/1.0 200 OK\r\nSession: 77\r\n\r\n").unwrap();
                    let mut packet = vec![b'$', 0, 0, 14];
                    packet.extend_from_slice(&[0x80, 0x60, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0x65, 0x88]);
                    socket.write_all(&packet).unwrap();
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        });
        let host = format!("127.0.0.1:{port}");
        let mut session = Rtsp::new(TcpStream::connect(&host).unwrap(), "admin", "secret");
        let url = format!("rtsp://127.0.0.1:{port}/Streaming/Channels/101");
        let mut audio_tried = false;
        let negotiated = negotiate(&mut session, "127.0.0.1", &url, true, &mut audio_tried).unwrap();
        assert_eq!(negotiated.codec, Codec::H264);
        assert_eq!(negotiated.session_id, "77");
        let (channel, packet) = read_interleaved(&mut session.stream, &mut session.pipe).unwrap();
        assert_eq!(channel, 0);
        assert_eq!(rtp_payload(&packet), Some(&[0x65, 0x88][..]));
        server.join().unwrap();
    }

    #[test]
    fn search_result_becomes_records() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<CMSearchResult version="2.0" xmlns="http://www.isapi.org/ver20/XMLSchema">
<searchID>X</searchID><responseStatus>true</responseStatus><responseStatusStrg>OK</responseStatusStrg>
<numOfMatches>2</numOfMatches>
<matchList>
<searchMatchItem>
<sourceID>{0000-0000}</sourceID><trackID>101</trackID>
<timeSpan><startTime>2026-10-06T01:00:00Z</startTime><endTime>2026-10-06T01:30:00Z</endTime></timeSpan>
<mediaSegmentDescriptor><contentType>video</contentType><codecType>H.264-BP</codecType>
<playbackURI>rtsp://10.18.10.61/Streaming/tracks/101/?starttime=20261006T010000Z&amp;endtime=20261006T013000Z&amp;name=00000000231000300&amp;size=262144000</playbackURI>
</mediaSegmentDescriptor>
</searchMatchItem>
<searchMatchItem>
<timeSpan><startTime>2026-10-06T01:30:00Z</startTime><endTime>2026-10-06T02:00:00Z</endTime></timeSpan>
<mediaSegmentDescriptor><playbackURI>rtsp://10.18.10.61/Streaming/tracks/101/?starttime=20261006T013000Z&amp;endtime=20261006T020000Z&amp;name=2&amp;size=1000</playbackURI></mediaSegmentDescriptor>
</searchMatchItem>
</matchList>
</CMSearchResult>"#;
        let records = parse_search(xml);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].begin, "2026-10-06 01:00:00");
        assert_eq!(records[0].end, "2026-10-06 01:30:00");
        assert_eq!(records[0].bytes, 262_144_000);
        assert!(records[0].uri.contains("&endtime=20261006T013000Z&name="));
    }

    #[test]
    fn hikvision_time_formats() {
        assert_eq!(compact_time("2026-10-06 01:02:03"), "20261006T010203Z");
        assert_eq!(isapi_time("2026-10-06 01:02:03"), "2026-10-06T01:02:03Z");
        assert_eq!(stamp_from_isapi("2026-10-06T01:02:03Z"), "2026-10-06 01:02:03");
        assert_eq!(stamp_from_isapi("2026-10-06T01:02:03+05:00"), "2026-10-06 01:02:03");
        assert_eq!(
            playback_uri("h", 101, "2026-10-06 01:00:00", "2026-10-06 01:10:00"),
            "rtsp://h/Streaming/tracks/101/?starttime=20261006T010000Z&endtime=20261006T011000Z"
        );
    }

    #[test]
    fn download_streams_chunked_ps_without_imkh_header() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let request = read_request(&mut socket);
            assert!(request.starts_with("GET /ISAPI/ContentMgmt/download"));
            socket
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"r\", nonce=\"n\", qop=\"auth\"\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            drop(socket);
            let (mut socket, _) = listener.accept().unwrap();
            let request = read_request(&mut socket);
            assert!(request.contains("Authorization: Digest"));
            let mut file = b"IMKH".to_vec();
            file.resize(40, 0);
            file.extend_from_slice(&[0, 0, 1, 0xBA, 1, 2, 3]);
            let mut reply = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            for part in file.chunks(9) {
                reply.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
                reply.extend_from_slice(part);
                reply.extend_from_slice(b"\r\n");
            }
            reply.extend_from_slice(b"0\r\n\r\n");
            socket.write_all(&reply).unwrap();
        });
        let mut out = Vec::new();
        let mut seen = 0;
        let written = download("127.0.0.1", port, "admin", "pw", "rtsp://h/x?a=1&b=2", &mut out, |bytes| {
            seen = bytes;
            true
        })
        .unwrap();
        assert_eq!(out, vec![0, 0, 1, 0xBA, 1, 2, 3]);
        assert_eq!(written, 7);
        assert_eq!(seen, 7);
    }

    #[test]
    fn h265_aggregation_packet_is_split() {
        let mut stream = video_stream(Codec::H265, "");
        let payload = [
            0x60, 0x01, 0x00, 0x03, 0x40, 0x01, 0xAA, 0x00, 0x03, 0x42, 0x01, 0xBB, 0x00, 0x03, 0x44,
            0x01, 0xCC,
        ];
        let nals = stream.depay_h265(&payload).unwrap();
        let kinds: Vec<u8> = nals.iter().map(nal_kind).collect();
        assert_eq!(kinds, vec![32, 33, 34]);
        assert!(nals.iter().all(|nal| nal.keyframe));
    }

    #[test]
    fn sdp_parameter_sets_come_before_a_bare_keyframe() {
        let sdp = "m=video 0 RTP/AVP 96\r\n\
            a=rtpmap:96 H264/90000\r\n\
            a=fmtp:96 profile-level-id=420029; packetization-mode=1; sprop-parameter-sets=Z0IAKeKQ,aM48gA==\r\n";
        let mut stream = video_stream(Codec::H264, sdp);
        assert_eq!(stream.params.len(), 2);
        let idr = annex_b(vec![0x65, 0x88], Codec::H264);
        let kinds: Vec<u8> = stream.with_params(vec![idr.clone()]).iter().map(nal_kind).collect();
        assert_eq!(kinds, vec![7, 8, 5]);
        let sps = annex_b(vec![0x67, 0x42], Codec::H264);
        let kinds: Vec<u8> = stream.with_params(vec![sps, idr]).iter().map(nal_kind).collect();
        assert_eq!(kinds, vec![7, 5]);
    }

    #[test]
    fn rtp_padding_is_dropped() {
        let mut packet = vec![0xA0, 0x60, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        packet.extend_from_slice(&[0x41, 0x9A, 0, 0, 3]);
        assert_eq!(rtp_payload(&packet), Some(&[0x41, 0x9A][..]));
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
