use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use xm_nvr::{
    hik_describe, hik_download, hik_search, Codec, Error, HikPiece, HikStream, PlaybackSpec, Session,
    StreamKind,
};

#[derive(Parser)]
#[command(
    name = "xm-nvr",
    about = "Клиент регистратора XM по локальному или белому IP"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Серийный номер, прошивка и число каналов
    Info {
        #[command(flatten)]
        device: DeviceArgs,
    },
    /// Список записей на диске регистратора
    Search {
        #[command(flatten)]
        device: DeviceArgs,
        /// Номер канала, с 1
        #[arg(long)]
        channel: u16,
        /// Начало, формат «2026-10-06 00:00:00»
        #[arg(long)]
        start: String,
        /// Конец, тот же формат
        #[arg(long)]
        end: String,
    },
    /// Живое видео в файл H.264/H.265
    Live {
        #[command(flatten)]
        device: DeviceArgs,
        #[arg(long)]
        channel: u16,
        /// main или sub
        #[arg(long, default_value = "sub")]
        stream: String,
        /// Сколько секунд читать
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        #[arg(long)]
        out: String,
    },
    /// Скачать запись с регистратора в файл H.264/H.265
    Play {
        #[command(flatten)]
        device: DeviceArgs,
        #[arg(long)]
        channel: u16,
        #[arg(long)]
        start: String,
        #[arg(long)]
        end: String,
        /// Путь файла с регистратора, если поиск его уже вернул
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        out: String,
    },
    /// Проверка Hikvision: каналы, описание RTSP-потока и что реально приходит
    Hik {
        #[arg(long)]
        host: String,
        #[arg(long, default_value_t = 80)]
        port: u16,
        #[arg(long, default_value = "admin")]
        user: String,
        /// Если не задан, берётся переменная XM_PASSWORD, иначе пустой пароль
        #[arg(long)]
        password: Option<String>,
        /// Номер камеры в списке, с 1. Без него выводится только список каналов
        #[arg(long)]
        channel: Option<u16>,
        /// main или sub
        #[arg(long, default_value = "sub")]
        stream: String,
        /// Сколько секунд читать поток
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        /// Сохранить видео в файл H.264/H.265
        #[arg(long)]
        out: Option<String>,
        /// Архив: начало, формат «2026-10-06 00:00:00». Вместе с --end ищет записи и читает первую
        #[arg(long)]
        start: Option<String>,
        /// Архив: конец, тот же формат
        #[arg(long)]
        end: Option<String>,
        /// Архив: скачать первую найденную запись в этот файл (MPEG-PS)
        #[arg(long)]
        download: Option<String>,
    },
}

#[derive(clap::Args)]
struct DeviceArgs {
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 34567)]
    port: u16,
    #[arg(long, default_value = "admin")]
    user: String,
    /// Если не задан, берётся переменная XM_PASSWORD, иначе пустой пароль
    #[arg(long)]
    password: Option<String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), Error> {
    let cli = Cli::parse();
    match cli.command {
        Command::Info { device } => {
            let mut session = connect(&device)?;
            let info = session.device_info()?;
            println!("адрес: {}:{}", session.host(), session.port());
            println!("серийный: {}", dash(&info.serial));
            println!("прошивка: {}", dash(&info.software));
            println!("плата: {}", dash(&info.hardware));
            println!(
                "каналы: {} (аналог {}, ip {})",
                info.video_channels(),
                info.analog_channels,
                info.digital_channels
            );
            if !info.titles.is_empty() {
                for (index, title) in info.titles.iter().enumerate() {
                    if !title.is_empty() {
                        println!("  {}. {title}", index + 1);
                    }
                }
            }
        }
        Command::Search {
            device,
            channel,
            start,
            end,
        } => {
            check_time(&start)?;
            check_time(&end)?;
            let channel = protocol_channel(channel)?;
            let mut session = connect(&device)?;
            let files = session.search(channel, &start, &end)?;
            println!("найдено: {}", files.len());
            for file in files {
                println!(
                    "{}  {}  {}  {}",
                    file.begin, file.end, file.bytes, file.path
                );
            }
        }
        Command::Live {
            device,
            channel,
            stream,
            seconds,
            out,
        } => {
            let channel = protocol_channel(channel)?;
            let stream = StreamKind::parse(&stream)?;
            let mut session = connect(&device)?;
            session.start_live(channel, stream)?;
            let mut file = File::create(&out)?;
            let mut frames = 0u64;
            let mut bytes = 0u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
            while std::time::Instant::now() < deadline {
                let frame = session.next_video()?;
                file.write_all(&frame.data)?;
                frames += 1;
                bytes += frame.data.len() as u64;
                if frames == 1 {
                    println!(
                        "поток {}x{} {:.0} кадр/с, ключевой кадр {}",
                        frame.width,
                        frame.height,
                        frame.fps,
                        frame.keyframe
                    );
                }
            }
            println!("кадров: {frames}, видео: {}, файл: {out}", human_size(bytes));
        }
        Command::Play {
            device,
            channel,
            start,
            end,
            file,
            out,
        } => {
            check_time(&start)?;
            check_time(&end)?;
            let channel = protocol_channel(channel)?;
            let mut session = connect(&device)?;
            let spec = PlaybackSpec {
                channel,
                begin: start,
                end,
                path: file,
            };
            let mut output = File::create(&out)?;
            let stats = session.download(&spec, &mut output)?;
            println!(
                "кадров: {}, ключевых: {}, видео: {}, файл: {out}",
                stats.frames,
                stats.keyframes,
                human_size(stats.bytes)
            );
        }
        Command::Hik {
            host,
            port,
            user,
            password,
            channel,
            stream,
            seconds,
            out,
            start,
            end,
            download,
        } => hik(HikArgs {
            host,
            port,
            user,
            password,
            channel,
            stream,
            seconds,
            out,
            start,
            end,
            download,
        })?,
    }
    Ok(())
}

struct HikArgs {
    host: String,
    port: u16,
    user: String,
    password: Option<String>,
    channel: Option<u16>,
    stream: String,
    seconds: u64,
    out: Option<String>,
    start: Option<String>,
    end: Option<String>,
    download: Option<String>,
}

fn hik(args: HikArgs) -> Result<(), Error> {
    let HikArgs {
        host,
        port,
        user,
        password,
        channel,
        stream,
        seconds,
        out,
        start,
        end,
        download,
    } = args;
    let (host, user, stream) = (host.as_str(), user.as_str(), stream.as_str());
    let password = password
        .or_else(|| std::env::var("XM_PASSWORD").ok())
        .unwrap_or_default();
    let info = hik_describe(host, port, user, &password)?;
    println!("модель: {}", dash(&info.model));
    println!("серийный: {}", dash(&info.serial));
    println!("прошивка: {}", dash(&info.firmware));
    println!("каналы: {}", info.channels.len());
    for (index, item) in info.channels.iter().enumerate() {
        let offline = if item.online { "" } else { " (не в сети)" };
        println!("  {}. id {}  {}{offline}", index + 1, item.id, item.name);
    }
    let Some(channel) = channel else {
        return Ok(());
    };
    let picked = info
        .channels
        .get(usize::from(channel.max(1)) - 1)
        .ok_or_else(|| Error::Protocol(format!("канала {channel} нет в списке")))?;
    let main = StreamKind::parse(stream)? == StreamKind::Main;
    let (opened, log) = match (start, end) {
        (Some(start), Some(end)) => {
            check_time(&start)?;
            check_time(&end)?;
            let track = u32::from(picked.id) * 100 + 1;
            let records = hik_search(host, port, user, &password, track, &start, &end)?;
            println!("записей на треке {track}: {}", records.len());
            for record in &records {
                println!("  {}  {}  {}  {}", record.begin, record.end, human_size(record.bytes), record.uri);
            }
            let Some(first) = records.first() else {
                return Ok(());
            };
            if let Some(path) = &download {
                let mut file = File::create(path)?;
                let started = Instant::now();
                let bytes = hik_download(host, port, user, &password, &first.uri, &mut file, |_| true)?;
                println!(
                    "скачано: {} за {:.1} с, файл: {path}",
                    human_size(bytes),
                    started.elapsed().as_secs_f32()
                );
            }
            println!("воспроизведение с {} по {}", first.begin, first.end);
            HikStream::open_playback(host, user, &password, track, &first.begin, &first.end)
        }
        (None, None) => {
            println!(
                "RTSP: rtsp://{host}:554/Streaming/Channels/{}",
                u32::from(picked.id) * 100 + if main { 1 } else { 2 }
            );
            HikStream::open_traced(host, user, &password, picked.id, main)
        }
        _ => return Err(Error::Protocol("для архива нужны и --start, и --end".into())),
    };
    println!("--- RTSP ---");
    for line in &log {
        println!("{line}");
    }
    println!("------------");
    let mut rtsp = opened?;
    println!("видео: {:?}, звук: {:?}", rtsp.codec(), rtsp.audio());
    let mut file = out.as_deref().map(File::create).transpose()?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut kinds: BTreeMap<u8, u64> = BTreeMap::new();
    let (mut nals, mut keyframes, mut bytes, mut audio) = (0u64, 0u64, 0u64, 0u64);
    let mut stamps: Option<(u32, u32)> = None;
    while Instant::now() < deadline {
        match rtsp.read_piece() {
            Ok(HikPiece::Video(nal)) => {
                stamps = Some((stamps.map_or(nal.timestamp, |(first, _)| first), nal.timestamp));
                nals += 1;
                keyframes += u64::from(nal.keyframe);
                bytes += nal.data.len() as u64;
                let header = nal.data.get(4).copied().unwrap_or(0);
                let kind = if nal.codec == Codec::H265 {
                    (header >> 1) & 0x3f
                } else {
                    header & 0x1f
                };
                *kinds.entry(kind).or_default() += 1;
                if let Some(file) = file.as_mut() {
                    file.write_all(&nal.data)?;
                }
            }
            Ok(HikPiece::Audio(_)) => audio += 1,
            Err(err) => {
                println!("поток прервался: {err}");
                break;
            }
        }
    }
    println!(
        "за {seconds} с: NAL {nals}, ключевых {keyframes}, видео {}, пакетов звука {audio}",
        human_size(bytes)
    );
    if let Some((first, last)) = stamps {
        println!("время по RTP-меткам: {:.1} с", f64::from(last.wrapping_sub(first)) / 90_000.0);
    }
    let kinds: Vec<String> = kinds.iter().map(|(kind, count)| format!("{kind}×{count}")).collect();
    println!("типы NAL: {}", if kinds.is_empty() { "—".to_string() } else { kinds.join(", ") });
    if let Some(out) = out {
        println!("файл: {out}");
    }
    Ok(())
}

fn connect(device: &DeviceArgs) -> Result<Session, Error> {
    let password = device.password.clone().or_else(|| std::env::var("XM_PASSWORD").ok());
    Session::connect(
        &device.host,
        device.port,
        &device.user,
        password.as_deref().unwrap_or(""),
    )
}

fn protocol_channel(channel: u16) -> Result<u16, Error> {
    if channel == 0 {
        return Err(Error::Protocol(
            "номер канала начинается с 1".into(),
        ));
    }
    Ok(channel - 1)
}

fn check_time(value: &str) -> Result<(), Error> {
    let bytes = value.as_bytes();
    let ok = bytes.len() == 19
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b' '
        && bytes[13] == b':'
        && bytes[16] == b':';
    if ok {
        Ok(())
    } else {
        Err(Error::Protocol(
            "время нужно в виде 2026-10-06 00:00:00".into(),
        ))
    }
}

fn dash(value: &str) -> &str {
    if value.is_empty() {
        "—"
    } else {
        value
    }
}

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} МБ", bytes as f64 / 1024.0 / 1024.0)
    } else if bytes >= 1024 {
        format!("{:.0} КБ", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} Б")
    }
}
