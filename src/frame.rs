use crate::error::Error;

pub const MAX_FRAME: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
    Mpeg4,
    Jpeg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl WallTime {
    pub fn from_bits(value: u32) -> Self {
        Self {
            second: (value & 0x3F) as u8,
            minute: ((value & 0xFC0) >> 6) as u8,
            hour: ((value & 0x1F000) >> 12) as u8,
            day: ((value & 0x3E0000) >> 17) as u8,
            month: ((value & 0x3C00000) >> 22) as u8,
            year: (((value & 0xFC000000) >> 26) + 2000) as u16,
        }
    }

    pub fn to_bits(self) -> u32 {
        (self.second as u32)
            | ((self.minute as u32) << 6)
            | ((self.hour as u32) << 12)
            | ((self.day as u32) << 17)
            | ((self.month as u32) << 22)
            | ((self.year.saturating_sub(2000) as u32) << 26)
    }
}

impl std::fmt::Display for WallTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub codec: Codec,
    pub keyframe: bool,
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub time: Option<WallTime>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub codec: u8,
    pub rate: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Video(VideoFrame),
    Audio(AudioFrame),
    Info(Vec<u8>),
}

/// Забирает один кадр из буфера полезной нагрузки XM.
/// `None` значит, что кадр ещё не собран целиком.
pub fn pull_frame(buf: &mut Vec<u8>) -> Result<Option<Frame>, Error> {
    if buf.len() < 4 {
        return Ok(None);
    }
    if buf.starts_with(&[0xFF, 0xD8, 0xFF]) {
        let data = std::mem::take(buf);
        return Ok(Some(Frame::Video(VideoFrame {
            codec: Codec::Jpeg,
            keyframe: true,
            width: 0,
            height: 0,
            fps: 0,
            time: None,
            data,
        })));
    }

    let kind = u32::from_be_bytes(buf[0..4].try_into().unwrap());
    let (header_len, media_len, frame) = match kind {
        0x1FC | 0x1FE => {
            if buf.len() < 16 {
                return Ok(None);
            }
            let media = buf[4];
            let fps = buf[5];
            let width = u16::from(buf[6]) * 8;
            let height = u16::from(buf[7]) * 8;
            let time = WallTime::from_bits(u32::from_le_bytes(buf[8..12].try_into().unwrap()));
            let len = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
            let codec = codec_from_byte(media);
            (
                16,
                len,
                Frame::Video(VideoFrame {
                    codec,
                    keyframe: true,
                    width,
                    height,
                    fps,
                    time: Some(time),
                    data: Vec::new(),
                }),
            )
        }
        0x1FD => {
            if buf.len() < 8 {
                return Ok(None);
            }
            let len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
            (
                8,
                len,
                Frame::Video(VideoFrame {
                    codec: Codec::H264,
                    keyframe: false,
                    width: 0,
                    height: 0,
                    fps: 0,
                    time: None,
                    data: Vec::new(),
                }),
            )
        }
        0x1FA | 0x1F9 => {
            if buf.len() < 8 {
                return Ok(None);
            }
            let codec = buf[4];
            let rate = buf[5];
            let len = u16::from_le_bytes(buf[6..8].try_into().unwrap()) as usize;
            let placeholder = if kind == 0x1FA {
                Frame::Audio(AudioFrame {
                    codec,
                    rate,
                    data: Vec::new(),
                })
            } else {
                Frame::Info(Vec::new())
            };
            (8, len, placeholder)
        }
        _ => {
            return Err(Error::Protocol(trf!("неизвестный кадр {kind}", kind = format!("{:08X}", kind))));
        }
    };

    if media_len > MAX_FRAME {
        return Err(Error::Protocol(trf!("кадр {media_len} байт больше допустимого", media_len = media_len)));
    }
    if buf.len() < header_len + media_len {
        return Ok(None);
    }
    let data = buf[header_len..header_len + media_len].to_vec();
    buf.drain(..header_len + media_len);
    let frame = match frame {
        Frame::Video(mut video) => {
            video.data = data;
            Frame::Video(video)
        }
        Frame::Audio(mut audio) => {
            audio.data = data;
            Frame::Audio(audio)
        }
        Frame::Info(_) => Frame::Info(data),
    };
    Ok(Some(frame))
}

fn codec_from_byte(value: u8) -> Codec {
    match value & 0x0F {
        0x01 => Codec::Mpeg4,
        0x03 => Codec::H265,
        _ => Codec::H264,
    }
}

/// Кодек по первым байтам кадра. Надёжнее метки в заголовке XM:
/// у части камер `0x12` всё равно означает H.264, а не H.265.
/// Байт `0x27` — SPS H.264, хотя его старшие биты похожи на тип H.265.
pub fn sniff_codec(data: &[u8]) -> Option<Codec> {
    let header = first_nal(data)?;
    if header & 0x80 == 0 {
        let avc = header & 0x1F;
        if matches!(avc, 1 | 5 | 6 | 7 | 8 | 9) {
            return Some(Codec::H264);
        }
    }
    let hevc = (header >> 1) & 0x3F;
    if matches!(hevc, 16 | 17 | 18 | 19 | 20 | 21 | 32 | 33 | 34) {
        return Some(Codec::H265);
    }
    None
}

fn first_nal(data: &[u8]) -> Option<u8> {
    let end = data.len().min(256);
    let data = &data[..end];
    let mut index = 0;
    while index + 3 < data.len() {
        if data[index] == 0 && data[index + 1] == 0 {
            if data[index + 2] == 1 {
                return data.get(index + 3).copied();
            }
            if data[index + 2] == 0 && data.get(index + 3) == Some(&1) {
                return data.get(index + 4).copied();
            }
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i_frame(payload: &[u8]) -> Vec<u8> {
        let time = WallTime {
            year: 2026,
            month: 10,
            day: 6,
            hour: 2,
            minute: 18,
            second: 0,
        };
        let mut buf = Vec::new();
        buf.extend_from_slice(&0x1FCu32.to_be_bytes());
        buf.push(3); // H.265
        buf.push(15);
        buf.push(240); // 1920 / 8
        buf.push(135); // 1080 / 8
        buf.extend_from_slice(&time.to_bits().to_le_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(payload);
        buf
    }

    #[test]
    fn parses_keyframe() {
        let mut buf = i_frame(&[0, 0, 0, 1, 0x40]);
        let frame = pull_frame(&mut buf).unwrap().unwrap();
        let Frame::Video(video) = frame else {
            panic!("ожидался видеокадр");
        };
        assert_eq!(video.codec, Codec::H265);
        assert!(video.keyframe);
        assert_eq!(video.width, 1920);
        assert_eq!(video.height, 1080);
        assert_eq!(video.fps, 15);
        assert_eq!(video.time.unwrap().to_string(), "2026-10-06 02:18:00");
        assert_eq!(video.data, vec![0, 0, 0, 1, 0x40]);
        assert!(buf.is_empty());
    }

    #[test]
    fn marker_0x12_stays_h264() {
        let mut buf = i_frame(&[0, 0, 0, 1]);
        buf[4] = 0x12;
        let Frame::Video(video) = pull_frame(&mut buf).unwrap().unwrap() else {
            panic!("ожидался видеокадр");
        };
        assert_eq!(video.codec, Codec::H264);
    }

    #[test]
    fn sniff_distinguishes_h264_and_h265() {
        assert_eq!(sniff_codec(&[0, 0, 0, 1, 0x67, 0x42]), Some(Codec::H264));
        assert_eq!(sniff_codec(&[0, 0, 0, 1, 0x27, 0x4D]), Some(Codec::H264));
        assert_eq!(sniff_codec(&[0, 0, 0, 1, 0x40, 0x01]), Some(Codec::H265));
    }

    #[test]
    fn waits_for_split_p_frame() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0x1FDu32.to_be_bytes());
        buf.extend_from_slice(&10u32.to_le_bytes());
        buf.extend_from_slice(&[1, 2, 3, 4]);
        assert!(pull_frame(&mut buf).unwrap().is_none());
        buf.extend_from_slice(&[5, 6, 7, 8, 9, 10, 11]);
        let Frame::Video(video) = pull_frame(&mut buf).unwrap().unwrap() else {
            panic!("ожидался видеокадр");
        };
        assert!(!video.keyframe);
        assert_eq!(video.data, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(buf, vec![11]);
    }
}
