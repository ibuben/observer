use std::fs::File;
use std::io::Write;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use xm_nvr::{Error, PlaybackSpec, Session, StreamKind};

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
