//! Протокол Xiongmai (DVRIP) по TCP, порт 34567.
//! Подключение только по IP: локальный адрес или белый адрес с пробросом порта.

mod error;
mod frame;
mod hash;
mod hikvision;
mod packet;
mod session;

pub use hikvision::{
    call_status as hik_call_status, describe as hik_describe, download as hik_download,
    open_door as hik_open_door, playback_uri as hik_playback_uri, search as hik_search, DoorCall,
    HikAudio, HikAudioCodec, HikChannel, HikInfo, HikPiece, HikRecord, HikStream,
};

pub use error::Error;
pub use hash::sofia_hash;
pub use frame::{sniff_codec, AudioFrame, Codec, Frame, VideoFrame, WallTime};
pub use session::{
    DeviceInfo, DeviceLog, LivePiece, PlaybackSpec, Recording, Session, StreamKind,
};
