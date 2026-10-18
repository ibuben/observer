use std::collections::HashSet;
use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{device_message, Error};
use crate::frame::{pull_frame, AudioFrame, Codec, Frame, VideoFrame, WallTime};
use crate::hash::sofia_hash;
use crate::packet::{self, set_timeouts};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const MEDIA_GAP: Duration = Duration::from_secs(20);
const LOGIN: u16 = 1000;
const KEEPALIVE: u16 = 1006;
const SYSTEM_INFO: u16 = 1020;
const CONFIG_SET: u16 = 1040;
const CONFIG_GET: u16 = 1042;
const SYS_MANAGER: u16 = 1450;
const STORAGE_MANAGER: u16 = 1460;
const TIME_QUERY: u16 = 1452;
const USERS_GET: u16 = 1472;
const USERS_SET: u16 = 1474;
const CHANNEL_TITLE: u16 = 1048;
const CHANNEL_TITLE_SET: u16 = 1046;
const MONITOR_START: u16 = 1410;
const MONITOR_CLAIM: u16 = 1413;
const SET_IFRAME: u16 = 1562;
const SET_IFRAME_RSP: u16 = 1563;
const PTZ: u16 = 1400;
const PLAY_CTRL: u16 = 1420;
const PLAY_CLAIM: u16 = 1424;
const FILE_QUERY: u16 = 1440;
const LOG_QUERY: u16 = 1442;
const MEDIA_MSGS: [u16; 2] = [1412, 1426];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Main,
    Extra,
}

impl StreamKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamKind::Main => "Main",
            StreamKind::Extra => "Extra",
        }
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "main" | "Main" => Ok(StreamKind::Main),
            "sub" | "extra" | "Extra" => Ok(StreamKind::Extra),
            _ => Err(Error::Protocol(
                "поток должен быть main или sub".into(),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub serial: String,
    pub software: String,
    pub hardware: String,
    pub analog_channels: u16,
    pub digital_channels: u16,
    pub titles: Vec<String>,
    pub raw: Value,
}

impl DeviceInfo {
    pub fn video_channels(&self) -> u16 {
        self.analog_channels.saturating_add(self.digital_channels)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLog {
    pub time: String,
    pub kind: String,
    pub user: String,
    pub data: String,
}

#[derive(Debug, Clone)]
pub struct Recording {
    pub channel: u16,
    pub begin: String,
    pub end: String,
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct PlaybackSpec {
    pub channel: u16,
    pub begin: String,
    pub end: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DownloadStats {
    pub frames: u64,
    pub keyframes: u64,
    pub bytes: u64,
    pub fps: u8,
    pub position: Option<WallTime>,
}

pub struct Session {
    stream: TcpStream,
    host: String,
    port: u16,
    session_id: u32,
    seq: u32,
    alive_every: Duration,
    last_alive: Instant,
    pending: Vec<u8>,
    video_codec: Codec,
    video_width: u16,
    video_height: u16,
    video_fps: u8,
}

pub enum LivePiece {
    Video(VideoFrame),
    Audio(AudioFrame),
}

impl Session {
    pub fn connect(host: &str, port: u16, user: &str, password: &str) -> Result<Self, Error> {
        let addr = format!("{host}:{port}");
        let mut last_error = None;
        let mut stream = None;
        for socket in addr
            .to_socket_addrs()
            .map_err(|source| Error::Connect {
                host: host.to_string(),
                port,
                source,
            })?
        {
            match TcpStream::connect_timeout(&socket, Duration::from_secs(8)) {
                Ok(tcp) => {
                    stream = Some(tcp);
                    break;
                }
                Err(err) => last_error = Some(err),
            }
        }
        let stream = stream.ok_or_else(|| Error::Connect {
            host: host.to_string(),
            port,
            source: last_error.unwrap_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "адрес не найден")
            }),
        })?;
        set_timeouts(&stream, DEFAULT_TIMEOUT)?;
        let mut session = Self {
            stream,
            host: host.to_string(),
            port,
            session_id: 0,
            seq: 0,
            alive_every: Duration::from_secs(20),
            last_alive: Instant::now(),
            pending: Vec::new(),
            video_codec: Codec::H264,
            video_width: 0,
            video_height: 0,
            video_fps: 0,
        };
        session.login(user, password)?;
        Ok(session)
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn device_info(&mut self) -> Result<DeviceInfo, Error> {
        let reply = self.request(SYSTEM_INFO, "SystemInfo", json!({}))?;
        let info = reply.get("SystemInfo").cloned().unwrap_or(Value::Null);
        let analog = json_u16(info.get("VideoInChannel"));
        let digital = json_u16(info.get("DigChannel"));
        let titles = self.channel_titles().unwrap_or_default();
        Ok(DeviceInfo {
            serial: json_string(info.get("SerialNo")),
            software: json_string(info.get("SoftWareVersion")),
            hardware: json_string(info.get("HardWare")),
            analog_channels: analog,
            digital_channels: digital,
            titles,
            raw: info,
        })
    }

    pub fn search_logs(&mut self, begin: &str, end: &str) -> Result<Vec<DeviceLog>, Error> {
        let mut found = Vec::new();
        let mut position = 0i64;
        for _ in 0..40 {
            let reply = match self.request(
                LOG_QUERY,
                "OPLogQuery",
                json!({
                    "BeginTime": begin,
                    "EndTime": end,
                    "Type": "LogAll",
                    "LogPosition": position
                }),
            ) {
                Ok(reply) => reply,
                Err(err) if is_no_data(&err) => break,
                Err(err) => return Err(err),
            };
            let Some(list) = reply.get("OPLogQuery").and_then(Value::as_array) else {
                break;
            };
            if list.is_empty() {
                break;
            }
            let mut next = position;
            for item in list {
                let kind = json_string(item.get("Type"));
                let time = json_string(item.get("Time").or_else(|| item.get("BeginTime")));
                if kind.is_empty() && time.is_empty() {
                    continue;
                }
                if let Some(pos) = json_i64(item.get("Position")) {
                    if pos + 1 > next {
                        next = pos + 1;
                    }
                }
                found.push(DeviceLog {
                    time,
                    kind,
                    user: json_string(item.get("User")),
                    data: json_data(item.get("Data")),
                });
            }
            if list.len() < 16 || next <= position || found.len() >= 1000 {
                break;
            }
            position = next;
        }
        Ok(found)
    }

    pub fn search(
        &mut self,
        channel: u16,
        begin: &str,
        end: &str,
    ) -> Result<Vec<Recording>, Error> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = begin.to_string();
        for _ in 0..200 {
            let page = self.search_page(channel, &cursor, end)?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            let next_cursor = page.last().map(|item| item.begin.clone()).unwrap_or(cursor.clone());
            let mut added = 0;
            for item in page {
                if seen.insert(item.path.clone()) {
                    found.push(item);
                    added += 1;
                }
            }
            if page_len < 64 || added == 0 || next_cursor == cursor {
                break;
            }
            cursor = next_cursor;
        }
        Ok(found)
    }

    pub fn start_live(&mut self, channel: u16, stream: StreamKind) -> Result<(), Error> {
        let params = json!({
            "Channel": channel,
            "CombinMode": "NONE",
            "StreamType": stream.as_str(),
            "TransMode": "TCP"
        });
        self.request(
            MONITOR_CLAIM,
            "OPMonitor",
            json!({ "Action": "Claim", "Parameter": params }),
        )?;
        self.send_named(
            MONITOR_START,
            "OPMonitor",
            json!({ "Action": "Start", "Parameter": params }),
        )?;
        let _ = self.force_iframe(channel, stream);
        self.reset_media_state();
        Ok(())
    }

    pub fn force_iframe(&mut self, channel: u16, stream: StreamKind) -> Result<(), Error> {
        self.send_named(
            SET_IFRAME,
            "OPSetIFrame",
            json!({
                "Channel": channel,
                "StreamType": match stream {
                    StreamKind::Main => 0,
                    StreamKind::Extra => 1,
                }
            }),
        )
    }

    pub fn ptz(&mut self, channel: u16, command: &str, step: u8, stop: bool) -> Result<(), Error> {
        let step = step.clamp(1, 8);
        self.request(
            PTZ,
            "OPPTZControl",
            json!({
                "Command": command,
                "Parameter": {
                    "AUX": {"Number": 0, "Status": "On"},
                    "Channel": channel,
                    "MenuOpts": "Enter",
                    "POINT": {"bottom": 0, "left": 0, "right": 0, "top": 0},
                    "Pattern": "SetBegin",
                    "Preset": if stop { -1 } else { 65535 },
                    "Step": step,
                    "Tour": 0
                }
            }),
        )?;
        Ok(())
    }

    pub fn start_playback(&mut self, spec: &PlaybackSpec) -> Result<(), Error> {
        self.claim_playback(spec)?;
        self.send_named(PLAY_CTRL, "OPPlayBack", self.playback_body(spec, "Start"))?;
        self.reset_media_state();
        Ok(())
    }

    /// `speed` is 1, 2, 4, 8, or 16. Sofia reads `Parameter.Value` as log2 of the rate.
    pub fn set_playback_rate(&mut self, spec: &PlaybackSpec, speed: u8) -> Result<(), Error> {
        let speed = match speed {
            2 | 4 | 8 | 16 => speed,
            _ => 1,
        };
        let mut body = self.playback_body(spec, "Fast");
        if let Some(parameter) = body.get_mut("Parameter").and_then(Value::as_object_mut) {
            parameter.insert("Value".into(), json!(speed.trailing_zeros()));
        }
        self.send_named(PLAY_CTRL, "OPPlayBack", body)?;
        Ok(())
    }

    pub fn locate_playback(&mut self, spec: &PlaybackSpec) -> Result<(), Error> {
        self.send_named(PLAY_CTRL, "OPPlayBack", self.playback_body(spec, "Locate"))?;
        Ok(())
    }

    pub fn clone_socket(&self) -> Result<TcpStream, Error> {
        self.stream.try_clone().map_err(Error::Io)
    }

    pub fn next_video(&mut self) -> Result<VideoFrame, Error> {
        loop {
            match self.next_piece()? {
                LivePiece::Video(video) => return Ok(video),
                LivePiece::Audio(_) => continue,
            }
        }
    }

    pub fn next_piece(&mut self) -> Result<LivePiece, Error> {
        let started = Instant::now();
        loop {
            self.maybe_keepalive()?;
            if let Some(frame) = pull_frame(&mut self.pending)? {
                match frame {
                    Frame::Video(video) => {
                        if let Some(video) = self.accept_video(Frame::Video(video)) {
                            return Ok(LivePiece::Video(video));
                        }
                    }
                    Frame::Audio(audio) => return Ok(LivePiece::Audio(audio)),
                    Frame::Info(_) => continue,
                }
                continue;
            }
            match packet::read_packet(&mut self.stream) {
                Ok(packet) => {
                    if self.take_control_packet(&packet)? {
                        continue;
                    }
                    self.pending.extend_from_slice(&packet.payload);
                }
                Err(Error::Timeout) => {
                    if started.elapsed() > MEDIA_GAP {
                        return Err(Error::Timeout);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    pub fn download(&mut self, spec: &PlaybackSpec, out: &mut dyn Write) -> Result<DownloadStats, Error> {
        self.download_with(spec, out, |_| true, |_| true)
    }

    /// `progress` и `on_audio` возвращают `false`, если скачивание нужно остановить.
    pub fn download_with(
        &mut self,
        spec: &PlaybackSpec,
        out: &mut dyn Write,
        mut progress: impl FnMut(&DownloadStats) -> bool,
        mut on_audio: impl FnMut(&AudioFrame) -> bool,
    ) -> Result<DownloadStats, Error> {
        self.claim_playback(spec)?;
        set_timeouts(&self.stream, Duration::from_secs(1))?;
        self.send_named(
            PLAY_CTRL,
            "OPPlayBack",
            self.playback_body(spec, "DownloadStart"),
        )?;
        self.pending.clear();
        let mut stats = DownloadStats::default();
        if !progress(&stats) {
            return self.cancel_download(spec);
        }
        let mut idle = Instant::now();
        loop {
            self.maybe_keepalive()?;
            while let Some(frame) = pull_frame(&mut self.pending)? {
                match frame {
                    Frame::Audio(audio) => {
                        if !on_audio(&audio) {
                            return self.cancel_download(spec);
                        }
                    }
                    other => {
                        if let Some(video) = self.accept_video(other) {
                            out.write_all(&video.data)?;
                            stats.frames += 1;
                            stats.bytes += video.data.len() as u64;
                            if video.keyframe {
                                stats.keyframes += 1;
                            }
                            if video.fps > 0 {
                                stats.fps = video.fps;
                            }
                            if video.time.is_some() {
                                stats.position = video.time;
                            }
                            if !progress(&stats) {
                                return self.cancel_download(spec);
                            }
                        }
                    }
                }
            }
            match packet::read_packet(&mut self.stream) {
                Ok(packet) if packet.payload.is_empty() => break,
                Ok(packet) => {
                    idle = Instant::now();
                    if self.take_control_packet(&packet)? {
                        continue;
                    }
                    self.pending.extend_from_slice(&packet.payload);
                }
                Err(Error::Ended) => break,
                Err(Error::Timeout) => {
                    if !progress(&stats) {
                        return self.cancel_download(spec);
                    }
                    if idle.elapsed() > Duration::from_secs(30) {
                        return Err(Error::Timeout);
                    }
                }
                Err(err) => return Err(err),
            }
        }
        let _ = self.send_named(
            PLAY_CTRL,
            "OPPlayBack",
            self.playback_body(spec, "DownloadStop"),
        );
        Ok(stats)
    }

    fn cancel_download(&mut self, spec: &PlaybackSpec) -> Result<DownloadStats, Error> {
        let _ = self.send_named(
            PLAY_CTRL,
            "OPPlayBack",
            self.playback_body(spec, "DownloadStop"),
        );
        Err(Error::Cancelled)
    }

    fn login(&mut self, user: &str, password: &str) -> Result<(), Error> {
        let body = json!({
            "EncryptType": "MD5",
            "LoginType": "DVRIP-Web",
            "PassWord": sofia_hash(password),
            "UserName": user
        });
        let reply = self.roundtrip(LOGIN, &body)?;
        let alive = reply
            .get("AliveInterval")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(5, 120);
        self.alive_every = Duration::from_secs(alive);
        self.last_alive = Instant::now();
        Ok(())
    }

    pub fn channel_titles(&mut self) -> Result<Vec<String>, Error> {
        let reply = self.request(CHANNEL_TITLE, "ChannelTitle", json!({}))?;
        let Some(list) = reply.get("ChannelTitle").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        Ok(list.iter().map(|item| json_string(Some(item))).collect())
    }

    pub fn set_channel_title(&mut self, channel: u16, title: &str) -> Result<Vec<String>, Error> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Error::Protocol("имя канала пустое".into()));
        }
        if title.chars().count() > 32 {
            return Err(Error::Protocol("имя длиннее 32 символов".into()));
        }
        let mut titles = self.channel_titles()?;
        let index = channel as usize;
        if titles.is_empty() || index >= titles.len() {
            return Err(Error::Protocol("регистратор не отдал имя этого канала".into()));
        }
        titles[index] = title.to_string();
        self.request(CHANNEL_TITLE_SET, "ChannelTitle", json!(titles.clone()))?;
        Ok(self.channel_titles().unwrap_or(titles))
    }

    pub fn config_get(&mut self, name: &str) -> Result<Value, Error> {
        let reply = self.request(CONFIG_GET, name, json!(""))?;
        reply
            .get(name)
            .cloned()
            .ok_or_else(|| Error::Protocol(format!("регистратор не вернул {name}")))
    }

    pub fn config_set(&mut self, name: &str, value: &Value) -> Result<(), Error> {
        self.request(CONFIG_SET, name, value.clone())?;
        Ok(())
    }

    pub fn system_query(&mut self, name: &str) -> Result<Value, Error> {
        let reply = self.request(SYSTEM_INFO, name, json!(""))?;
        Ok(reply.get(name).cloned().unwrap_or(Value::Null))
    }

    pub fn query_time(&mut self) -> Result<String, Error> {
        let reply = self.request(TIME_QUERY, "OPTimeQuery", json!(""))?;
        Ok(json_string(reply.get("OPTimeQuery")))
    }

    pub fn set_time(&mut self, stamp: &str) -> Result<(), Error> {
        self.request(SYS_MANAGER, "OPTimeSetting", json!(stamp))?;
        Ok(())
    }

    pub fn users(&mut self) -> Result<Value, Error> {
        let reply = self.request(USERS_GET, "Users", json!(""))?;
        Ok(reply.get("Users").cloned().unwrap_or(Value::Null))
    }

    pub fn set_users(&mut self, value: &Value) -> Result<(), Error> {
        self.request(USERS_SET, "Users", value.clone())?;
        Ok(())
    }

    pub fn format_disk(&mut self, disk: u32, part: u32) -> Result<(), Error> {
        self.request(
            STORAGE_MANAGER,
            "OPStorageManager",
            json!({
                "Action": "Clear",
                "SerialNo": disk,
                "PartNo": part,
                "Type": "Data"
            }),
        )?;
        Ok(())
    }

    pub fn reboot(&mut self) -> Result<(), Error> {
        self.request(SYS_MANAGER, "OPMachine", json!({"Action": "Reboot"}))?;
        Ok(())
    }

    pub fn channel_links(&mut self) -> Result<Vec<String>, Error> {
        let reply = self.request(CONFIG_GET, "NetWork.ChnStatus", json!(""))?;
        let Some(list) = reply.get("NetWork.ChnStatus").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        Ok(list
            .iter()
            .map(|item| json_string(item.get("Status")))
            .collect())
    }

    fn search_page(
        &mut self,
        channel: u16,
        begin: &str,
        end: &str,
    ) -> Result<Vec<Recording>, Error> {
        let reply = match self.request(
            FILE_QUERY,
            "OPFileQuery",
            json!({
                "BeginTime": begin,
                "EndTime": end,
                "Channel": channel,
                "DriverTypeMask": "0x0000FFFF",
                "Event": "*",
                "StreamType": "0x00000000",
                "Type": "h264"
            }),
        ) {
            Ok(reply) => reply,
            Err(err) if is_no_data(&err) => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let Some(list) = reply.get("OPFileQuery").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        let mut records = Vec::with_capacity(list.len());
        for item in list {
            let Some(path) = item.get("FileName").and_then(Value::as_str) else {
                continue;
            };
            records.push(Recording {
                channel,
                begin: json_string(item.get("BeginTime")),
                end: json_string(item.get("EndTime")),
                path: path.to_string(),
                bytes: json_u64(item.get("FileLength").or_else(|| item.get("FileSize"))),
            });
        }
        Ok(records)
    }

    fn claim_playback(&mut self, spec: &PlaybackSpec) -> Result<(), Error> {
        self.request(PLAY_CLAIM, "OPPlayBack", self.playback_body(spec, "Claim"))?;
        Ok(())
    }

    fn playback_body(&self, spec: &PlaybackSpec, action: &str) -> Value {
        let (mode, file_name) = match &spec.path {
            Some(path) => ("ByName", path.as_str()),
            None => ("ByTime", ""),
        };
        json!({
            "Action": action,
            "StartTime": spec.begin,
            "EndTime": spec.end,
            "Parameter": {
                "Channel": spec.channel,
                "FileName": file_name,
                "PlayMode": mode,
                "StreamType": 0,
                "TransMode": "TCP",
                "Value": 0
            }
        })
    }

    fn reset_media_state(&mut self) {
        self.pending.clear();
        self.video_width = 0;
        self.video_height = 0;
        self.video_fps = 0;
        self.video_codec = Codec::H264;
    }

    fn accept_video(&mut self, frame: Frame) -> Option<VideoFrame> {
        let Frame::Video(mut video) = frame else {
            return None;
        };
        if video.keyframe {
            if video.codec != Codec::Jpeg {
                self.video_codec = video.codec;
            }
            if video.width > 0 {
                self.video_width = video.width;
                self.video_height = video.height;
            }
            if video.fps > 0 {
                self.video_fps = video.fps;
            }
        } else {
            video.codec = self.video_codec;
            video.width = self.video_width;
            video.height = self.video_height;
            video.fps = self.video_fps;
        }
        Some(video)
    }

    fn take_control_packet(&mut self, packet: &packet::Packet) -> Result<bool, Error> {
        if packet.msg == SET_IFRAME_RSP {
            return Ok(true);
        }
        let looks_like_media = MEDIA_MSGS.contains(&packet.msg)
            || packet.payload.starts_with(&[0x00, 0x00, 0x01])
            || packet.payload.starts_with(&[0xFF, 0xD8, 0xFF]);
        if looks_like_media && !packet.payload.starts_with(b"{") {
            return Ok(false);
        }
        if packet.payload.first().copied() != Some(b'{') && !packet.payload.starts_with(b"\n{") {
            if packet.payload.is_empty() {
                return Ok(true);
            }
            if looks_like_media {
                return Ok(false);
            }
        }
        if let Ok(value) = packet::parse_json_payload(&packet.payload) {
            if let Some(code) = value.get("Ret").and_then(Value::as_i64) {
                if code != 100 && code != 515 {
                    return Err(Error::Device {
                        code,
                        message: device_message(code).into(),
                    });
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn maybe_keepalive(&mut self) -> Result<(), Error> {
        if self.last_alive.elapsed() < self.alive_every {
            return Ok(());
        }
        let body = json!({
            "Name": "KeepAlive",
            "SessionID": session_hex(self.session_id)
        });
        self.write_json(KEEPALIVE, &body)?;
        self.last_alive = Instant::now();
        Ok(())
    }

    fn request(&mut self, code: u16, name: &str, inner: Value) -> Result<Value, Error> {
        self.send_named(code, name, inner)?;
        self.read_reply()
    }

    fn send_named(&mut self, code: u16, name: &str, inner: Value) -> Result<(), Error> {
        let body = json!({
            "Name": name,
            "SessionID": session_hex(self.session_id),
            name: inner
        });
        self.write_json(code, &body)
    }

    fn roundtrip(&mut self, code: u16, body: &Value) -> Result<Value, Error> {
        self.write_json(code, body)?;
        self.read_reply()
    }

    fn write_json(&mut self, code: u16, body: &Value) -> Result<(), Error> {
        debug_out(body);
        let payload = packet::json_payload(body)?;
        packet::write_packet(&mut self.stream, self.session_id, self.seq, code, &payload)?;
        self.seq = self.seq.wrapping_add(1);
        Ok(())
    }

    fn read_reply(&mut self) -> Result<Value, Error> {
        let packet = packet::read_packet(&mut self.stream)?;
        if packet.session != 0 {
            self.session_id = packet.session;
        }
        let value = packet::parse_json_payload(&packet.payload)?;
        debug_in(&value);
        if let Some(text) = value.get("SessionID").and_then(Value::as_str) {
            self.session_id = parse_session(text)?;
        }
        let code = value.get("Ret").and_then(Value::as_i64).unwrap_or(101);
        if code != 100 && code != 515 {
            return Err(Error::Device {
                code,
                message: device_message(code).into(),
            });
        }
        Ok(value)
    }
}

fn session_hex(id: u32) -> String {
    format!("0x{id:08X}")
}

fn parse_session(text: &str) -> Result<u32, Error> {
    let hex = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(hex, 16).map_err(|_| Error::Protocol(format!("SessionID {text}")))
}

/// XM firmware answers a file or log query with nothing to return as Ret 119, not an empty list.
fn is_no_data(err: &Error) -> bool {
    matches!(err, Error::Device { code: 119, .. })
}

fn json_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

fn json_i64(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(number)) => number.as_i64(),
        Some(Value::String(text)) => text.parse().ok(),
        _ => None,
    }
}

fn json_data(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(other @ (Value::Object(_) | Value::Array(_))) => other.to_string(),
        _ => String::new(),
    }
}

fn json_u16(value: Option<&Value>) -> u16 {
    json_u64(value).min(u16::MAX as u64) as u16
}

fn json_u64(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(0),
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn debug_enabled() -> bool {
    std::env::var("XM_DEBUG").ok().as_deref() == Some("1")
}

fn debug_out(value: &Value) {
    if debug_enabled() {
        eprintln!("=> {}", redact(value));
    }
}

fn debug_in(value: &Value) {
    if debug_enabled() {
        eprintln!("<= {value}");
    }
}

fn redact(value: &Value) -> Value {
    let mut copy = value.clone();
    if let Some(object) = copy.as_object_mut() {
        if object.contains_key("PassWord") {
            object.insert("PassWord".into(), json!("***"));
        }
    }
    copy
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::frame::WallTime;
    use crate::packet;

    fn serve<F>(handler: F) -> String
    where
        F: FnOnce(TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handler(stream);
        });
        addr.to_string()
    }

    fn read_json(stream: &mut TcpStream) -> (u16, Value) {
        let packet = packet::read_packet(stream).unwrap();
        let value = packet::parse_json_payload(&packet.payload).unwrap();
        (packet.msg, value)
    }

    fn write_json(stream: &mut TcpStream, session: u32, msg: u16, value: Value) {
        let payload = packet::json_payload(&value).unwrap();
        packet::write_packet(stream, session, 0, msg, &payload).unwrap();
    }

    #[test]
    fn login_info_and_search_against_mock() {
        let addr = serve(|mut stream| {
            let (msg, login) = read_json(&mut stream);
            assert_eq!(msg, LOGIN);
            assert_eq!(login["UserName"], "admin");
            assert_eq!(login["PassWord"], "6QNMIQGe");
            write_json(
                &mut stream,
                7,
                1001,
                json!({
                    "Ret": 100,
                    "SessionID": "0x00000007",
                    "AliveInterval": 21
                }),
            );
            let (msg, info) = read_json(&mut stream);
            assert_eq!(msg, SYSTEM_INFO);
            assert_eq!(info["SessionID"], "0x00000007");
            write_json(
                &mut stream,
                7,
                1021,
                json!({
                    "Ret": 100,
                    "SessionID": "0x00000007",
                    "SystemInfo": {
                        "SerialNo": "abc",
                        "SoftWareVersion": "2.0",
                        "HardWare": "XM",
                        "VideoInChannel": 0,
                        "DigChannel": 4
                    }
                }),
            );
            let _ = read_json(&mut stream);
            write_json(
                &mut stream,
                7,
                1049,
                json!({
                    "Ret": 100,
                    "ChannelTitle": ["Двор", "Вход"]
                }),
            );
            let (msg, query) = read_json(&mut stream);
            assert_eq!(msg, FILE_QUERY);
            assert_eq!(query["OPFileQuery"]["Channel"], 0);
            write_json(
                &mut stream,
                7,
                1441,
                json!({
                    "Ret": 100,
                    "OPFileQuery": [{
                        "FileName": "/idea0/a.h264",
                        "BeginTime": "2026-10-06 01:00:00",
                        "EndTime": "2026-10-06 01:05:00",
                        "FileLength": "1000"
                    }]
                }),
            );
        });
        let (host, port) = addr.rsplit_once(':').unwrap();
        let mut session = Session::connect(host, port.parse().unwrap(), "admin", "admin").unwrap();
        let info = session.device_info().unwrap();
        assert_eq!(info.serial, "abc");
        assert_eq!(info.video_channels(), 4);
        assert_eq!(info.titles, vec!["Двор".to_string(), "Вход".to_string()]);
        let files = session
            .search(0, "2026-10-06 00:00:00", "2026-10-06 23:59:59")
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/idea0/a.h264");
        assert_eq!(files[0].bytes, 1000);
    }

    #[test]
    fn search_treats_ret_119_as_no_records() {
        let addr = serve(|mut stream| {
            let _ = read_json(&mut stream);
            write_json(
                &mut stream,
                5,
                1001,
                json!({"Ret": 100, "SessionID": "0x00000005", "AliveInterval": 20}),
            );
            let (msg, _) = read_json(&mut stream);
            assert_eq!(msg, FILE_QUERY);
            write_json(&mut stream, 5, 1441, json!({"Ret": 119, "SessionID": "0x00000005"}));

            let (msg, _) = read_json(&mut stream);
            assert_eq!(msg, FILE_QUERY);
            let page: Vec<Value> = (0..64)
                .map(|index| {
                    json!({
                        "FileName": format!("/idea0/{index}.h264"),
                        "BeginTime": format!("2026-10-06 {:02}:{:02}:00", index / 60, index % 60),
                        "EndTime": format!("2026-10-06 {:02}:{:02}:59", index / 60, index % 60),
                        "FileLength": "1000"
                    })
                })
                .collect();
            write_json(&mut stream, 5, 1441, json!({"Ret": 100, "OPFileQuery": page}));
            let (msg, _) = read_json(&mut stream);
            assert_eq!(msg, FILE_QUERY);
            write_json(&mut stream, 5, 1441, json!({"Ret": 119, "SessionID": "0x00000005"}));
        });
        let (host, port) = addr.rsplit_once(':').unwrap();
        let mut session = Session::connect(host, port.parse().unwrap(), "admin", "").unwrap();
        let empty = session
            .search(0, "2026-10-01 00:00:00", "2026-10-01 23:59:59")
            .unwrap();
        assert!(empty.is_empty());
        let paged = session
            .search(0, "2026-10-06 00:00:00", "2026-10-06 23:59:59")
            .unwrap();
        assert_eq!(paged.len(), 64);
    }

    #[test]
    fn set_channel_title_keeps_the_other_names() {
        let addr = serve(|mut stream| {
            let _ = read_json(&mut stream);
            write_json(
                &mut stream,
                3,
                1001,
                json!({"Ret": 100, "SessionID": "0x00000003", "AliveInterval": 20}),
            );
            let (msg, _) = read_json(&mut stream);
            assert_eq!(msg, CHANNEL_TITLE);
            write_json(
                &mut stream,
                3,
                1049,
                json!({"Ret": 100, "ChannelTitle": ["Kuhnya", "outdoor", "koridor"]}),
            );
            let (msg, body) = read_json(&mut stream);
            assert_eq!(msg, CHANNEL_TITLE_SET);
            assert_eq!(
                body["ChannelTitle"],
                json!(["Kuhnya", "двор", "koridor"])
            );
            write_json(&mut stream, 3, 1047, json!({"Ret": 100}));
            let (msg, _) = read_json(&mut stream);
            assert_eq!(msg, CHANNEL_TITLE);
            write_json(
                &mut stream,
                3,
                1049,
                json!({"Ret": 100, "ChannelTitle": ["Kuhnya", "двор", "koridor"]}),
            );
        });
        let (host, port) = addr.rsplit_once(':').unwrap();
        let mut session = Session::connect(host, port.parse().unwrap(), "admin", "").unwrap();
        let titles = session.set_channel_title(1, " двор ").unwrap();
        assert_eq!(titles, vec!["Kuhnya", "двор", "koridor"]);
    }

    #[test]
    fn ptz_stop_repeats_the_direction_with_preset_minus_one() {
        let addr = serve(|mut stream| {
            let _ = read_json(&mut stream);
            write_json(
                &mut stream,
                1,
                1001,
                json!({"Ret": 100, "SessionID": "0x00000001", "AliveInterval": 20}),
            );
            let (msg, start) = read_json(&mut stream);
            assert_eq!(msg, PTZ);
            assert_eq!(start["OPPTZControl"]["Command"], "DirectionLeft");
            assert_eq!(start["OPPTZControl"]["Parameter"]["Preset"], 65535);
            assert_eq!(start["OPPTZControl"]["Parameter"]["Step"], 3);
            assert_eq!(start["OPPTZControl"]["Parameter"]["Channel"], 4);
            write_json(
                &mut stream,
                1,
                1401,
                json!({"Ret": 100, "SessionID": "0x00000001"}),
            );
            let (msg, stop) = read_json(&mut stream);
            assert_eq!(msg, PTZ);
            assert_eq!(stop["OPPTZControl"]["Command"], "DirectionLeft");
            assert_eq!(stop["OPPTZControl"]["Parameter"]["Preset"], -1);
            assert_eq!(stop["OPPTZControl"]["Parameter"]["Step"], 3);
            write_json(
                &mut stream,
                1,
                1401,
                json!({"Ret": 100, "SessionID": "0x00000001"}),
            );
        });
        let (host, port) = addr.rsplit_once(':').unwrap();
        let mut session = Session::connect(host, port.parse().unwrap(), "admin", "").unwrap();
        session.ptz(4, "DirectionLeft", 3, false).unwrap();
        session.ptz(4, "DirectionLeft", 3, true).unwrap();
    }

    #[test]
    fn live_frame_from_mock() {
        let addr = serve(|mut stream| {
            let _ = read_json(&mut stream);
            write_json(
                &mut stream,
                1,
                1001,
                json!({"Ret": 100, "SessionID": "0x00000001", "AliveInterval": 20}),
            );
            let (msg, claim) = read_json(&mut stream);
            assert_eq!(msg, MONITOR_CLAIM);
            assert_eq!(claim["OPMonitor"]["Parameter"]["StreamType"], "Extra");
            write_json(&mut stream, 1, 1414, json!({"Ret": 100, "SessionID": "0x00000001"}));
            let (msg, _) = read_json(&mut stream);
            assert_eq!(msg, MONITOR_START);
            let (msg, iframe) = read_json(&mut stream);
            assert_eq!(msg, 1562);
            assert_eq!(iframe["OPSetIFrame"]["Channel"], 1);
            assert_eq!(iframe["OPSetIFrame"]["StreamType"], 1);

            let time = WallTime {
                year: 2026,
                month: 10,
                day: 6,
                hour: 1,
                minute: 2,
                second: 3,
            };
            let mut payload = Vec::new();
            payload.extend_from_slice(&0x1FCu32.to_be_bytes());
            payload.push(2);
            payload.push(25);
            payload.push(160);
            payload.push(90);
            payload.extend_from_slice(&time.to_bits().to_le_bytes());
            payload.extend_from_slice(&4u32.to_le_bytes());
            payload.extend_from_slice(&[9, 9, 9, 9]);
            packet::write_packet(&mut stream, 1, 1, 1412, &payload).unwrap();
        });
        let (host, port) = addr.rsplit_once(':').unwrap();
        let mut session = Session::connect(host, port.parse().unwrap(), "admin", "").unwrap();
        session.start_live(1, StreamKind::Extra).unwrap();
        let frame = session.next_video().unwrap();
        assert!(frame.keyframe);
        assert_eq!(frame.width, 1280);
        assert_eq!(frame.height, 720);
        assert_eq!(frame.data, vec![9, 9, 9, 9]);
    }
}
