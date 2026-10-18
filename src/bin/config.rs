use std::collections::BTreeMap;
use std::time::Instant;

use egui_phosphor::regular as ph;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConfigSection {
    Channels,
    Detect,
    Record,
    General,
    Picture,
    Network,
    Disk,
    Users,
    Maintain,
}

impl ConfigSection {
    pub fn title(self) -> &'static str {
        match self {
            Self::Channels => "Каналы",
            Self::Detect => "Детекция",
            Self::Record => "Запись",
            Self::General => "Общие",
            Self::Picture => "Картинка",
            Self::Network => "Сеть",
            Self::Disk => "Диск",
            Self::Users => "Пользователи",
            Self::Maintain => "Обслуживание",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Channels => ph::VIDEO_CAMERA,
            Self::Detect => ph::BELL,
            Self::Record => ph::CLOCK_COUNTER_CLOCKWISE,
            Self::General => ph::SLIDERS,
            Self::Picture => ph::PICTURE_IN_PICTURE,
            Self::Network => ph::BROADCAST,
            Self::Disk => ph::HARD_DRIVES,
            Self::Users => ph::USER,
            Self::Maintain => ph::WRENCH,
        }
    }

}

#[derive(Clone)]
pub struct ConfigBundle {
    pub titles: Vec<String>,
    pub values: BTreeMap<String, Value>,
    pub camera: Vec<Option<Value>>,
    pub time: String,
    pub time_at: Option<Instant>,
    pub users: Value,
    pub storage: Value,
    pub note: String,
    pub net_host: String,
    pub net_mask: String,
    pub net_gate: String,
    pub net_dns: String,
    pub net_dns2: String,
}

impl ConfigBundle {
    pub fn empty() -> Self {
        Self {
            titles: Vec::new(),
            values: BTreeMap::new(),
            camera: Vec::new(),
            time: String::new(),
            time_at: None,
            users: Value::Null,
            storage: Value::Null,
            note: String::new(),
            net_host: String::new(),
            net_mask: String::new(),
            net_gate: String::new(),
            net_dns: String::new(),
            net_dns2: String::new(),
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    pub fn sections(&self) -> Vec<ConfigSection> {
        let mut list = Vec::new();
        if !self.titles.is_empty() || self.has("Simplify.Encode") || self.has("AVEnc.Encode") {
            list.push(ConfigSection::Channels);
        }
        if self.has("Detect.MotionDetect")
            || self.has("Detect.BlindDetect")
            || self.has("Detect.LossDetect")
        {
            list.push(ConfigSection::Detect);
        }
        if self.has("Record") {
            list.push(ConfigSection::Record);
        }
        if self.has("General.General") || self.has("General.Location") {
            list.push(ConfigSection::General);
        }
        if self.has("AVEnc.VideoWidget") || self.camera.iter().any(Option::is_some) {
            list.push(ConfigSection::Picture);
        }
        if self.has("NetWork.NetCommon") {
            list.push(ConfigSection::Network);
        }
        if self.storage.as_array().is_some_and(|items| !items.is_empty()) {
            list.push(ConfigSection::Disk);
        }
        if self.users.as_array().is_some_and(|items| !items.is_empty()) {
            list.push(ConfigSection::Users);
        }
        list.push(ConfigSection::Maintain);
        list
    }

    pub fn encode_name(&self) -> Option<&'static str> {
        if self.has("Simplify.Encode") {
            Some("Simplify.Encode")
        } else if self.has("AVEnc.Encode") {
            Some("AVEnc.Encode")
        } else {
            None
        }
    }
}

pub fn hex_to_ip(text: &str) -> Option<String> {
    let hex = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(format!(
        "{}.{}.{}.{}",
        value & 0xff,
        (value >> 8) & 0xff,
        (value >> 16) & 0xff,
        (value >> 24) & 0xff
    ))
}

pub fn ip_to_hex(text: &str) -> Option<String> {
    let mut parts = [0u8; 4];
    let mut count = 0;
    for piece in text.trim().split('.') {
        if count >= 4 {
            return None;
        }
        parts[count] = piece.parse().ok()?;
        count += 1;
    }
    if count != 4 {
        return None;
    }
    let value = u32::from(parts[0])
        | (u32::from(parts[1]) << 8)
        | (u32::from(parts[2]) << 16)
        | (u32::from(parts[3]) << 24);
    Some(format!("0x{value:08X}"))
}

pub fn hex_megabytes(text: &str) -> Option<u64> {
    let hex = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(hex, 16).ok()
}

pub fn format_space(text: &str) -> String {
    let Some(mb) = hex_megabytes(text) else {
        return "—".into();
    };
    if mb >= 10_240 {
        format!("{:.1} ГБ", mb as f64 / 1024.0)
    } else {
        format!("{mb} МБ")
    }
}

pub fn parse_slot(text: &str) -> (u32, String, String) {
    let mut parts = text.split_whitespace();
    let mask = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let range = parts.next().unwrap_or("00:00:00-00:00:00");
    let (start, end) = range.split_once('-').unwrap_or(("00:00:00", "00:00:00"));
    (mask, start.to_string(), end.to_string())
}

pub fn format_slot(mask: u32, start: &str, end: &str) -> String {
    format!("{mask} {start}-{end}")
}

pub fn region_row(text: &str) -> u32 {
    let hex = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(hex, 16).unwrap_or(0)
}

pub fn write_region_row(bits: u32) -> String {
    format!("0x{bits:08X}")
}

pub fn toggle_region_bit(text: &str, column: usize) -> String {
    if column >= 32 {
        return text.to_string();
    }
    let mut bits = region_row(text);
    bits ^= 1 << column;
    write_region_row(bits)
}

pub fn weekday_short(index: usize) -> &'static str {
    match index {
        0 => "Вс",
        1 => "Пн",
        2 => "Вт",
        3 => "Ср",
        4 => "Чт",
        5 => "Пт",
        6 => "Сб",
        _ => "—",
    }
}

pub fn weekday_label(index: usize) -> &'static str {
    match index {
        0 => "Воскресенье",
        1 => "Понедельник",
        2 => "Вторник",
        3 => "Среда",
        4 => "Четверг",
        5 => "Пятница",
        6 => "Суббота",
        _ => "День",
    }
}

pub fn object_string(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

pub fn set_object_string(value: &mut Value, key: &str, text: String) {
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_string(), json!(text));
    }
}

pub fn object_i64(value: &Value, key: &str) -> i64 {
    match value.get(key) {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0),
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn set_object_i64(value: &mut Value, key: &str, number: i64) {
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_string(), json!(number));
    }
}

pub fn object_bool(value: &Value, key: &str) -> bool {
    match value.get(key) {
        Some(Value::Bool(on)) => *on,
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0) != 0,
        Some(Value::String(text)) => text == "true" || text == "1",
        _ => false,
    }
}

pub fn set_object_bool(value: &mut Value, key: &str, on: bool) {
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_string(), json!(on));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_roundtrip() {
        assert_eq!(hex_to_ip("0x0909120A").as_deref(), Some("10.18.9.9"));
        assert_eq!(ip_to_hex("10.18.9.1").as_deref(), Some("0x0109120A"));
        assert_eq!(hex_to_ip("0x00FFFFFF").as_deref(), Some("255.255.255.0"));
    }

    #[test]
    fn slot_keeps_mask() {
        let (mask, start, end) = parse_slot("1 00:00:00-24:00:00");
        assert_eq!(mask, 1);
        assert_eq!(format_slot(mask, &start, &end), "1 00:00:00-24:00:00");
    }

    #[test]
    fn region_bit_toggles_low_column() {
        assert_eq!(toggle_region_bit("0x00000000", 0), "0x00000001");
        assert_eq!(toggle_region_bit("0x00000001", 0), "0x00000000");
    }
}
