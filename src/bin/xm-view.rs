#![cfg_attr(windows, windows_subsystem = "windows")]

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, SyncSender};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_phosphor::regular as ph;

mod config;
use xm_nvr::{
    hik_call_status, sniff_codec, sofia_hash, AudioFrame, Codec, DeviceLog, DoorCall, LivePiece,
    PlaybackSpec, Recording, Session, StreamKind, WallTime,
};

const DEFAULT_ACCENT: egui::Color32 = egui::Color32::from_rgb(78, 196, 176);
const INK: egui::Color32 = egui::Color32::from_rgb(8, 24, 22);
const CANVAS: egui::Color32 = egui::Color32::from_rgb(7, 8, 10);
const PANEL: egui::Color32 = egui::Color32::from_rgb(18, 21, 26);
const TILE: egui::Color32 = egui::Color32::from_rgb(28, 33, 40);
const HOVER: egui::Color32 = egui::Color32::from_rgb(38, 46, 54);
const LINE: egui::Color32 = egui::Color32::from_rgb(48, 56, 66);
const TEXT: egui::Color32 = egui::Color32::from_rgb(232, 236, 241);
const MUTED: egui::Color32 = egui::Color32::from_rgb(142, 152, 166);
const VIDEO: egui::Color32 = egui::Color32::from_rgb(3, 4, 6);
const HEADER: egui::Color32 = egui::Color32::from_rgb(36, 42, 50);
const CELL_RADIUS: f32 = 8.0;
const TOP_BAR: f32 = 58.0;
const LIVE_BOTTOM_BAR: f32 = 64.0;
const ARCHIVE_BAR: f32 = 148.0;
const MAX_RANGE_SECS: i64 = 48 * 3600;
const SIDEBAR: f32 = 276.0;
const MAX_CELLS: usize = 64;

fn main() -> eframe::Result<()> {
    let icon = load_window_icon();
    let wordmark = load_wordmark();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("eXUnity Observer")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([1100.0, 700.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "eXUnity Observer",
        options,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx);
            apply_style(&cc.egui_ctx, DEFAULT_ACCENT);
            let texture = wordmark.map(|logo| {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [logo.width as usize, logo.height as usize],
                    &logo.rgba,
                );
                cc.egui_ctx
                    .load_texture("logo", image, egui::TextureOptions::LINEAR)
            });
            Ok(Box::new(Viewer::new(texture)))
        }),
    )
}

struct LoadedLogo {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn load_window_icon() -> Option<egui::IconData> {
    let image = image::load_from_memory(include_bytes!("../../Observer.png")).ok()?;
    Some(square_icon(&trim_transparent(image.to_rgba8()), 256))
}

fn load_wordmark() -> Option<LoadedLogo> {
    let image = image::load_from_memory(include_bytes!("../../logo4.png")).ok()?;
    let display = image.to_rgba8();
    let (width, height) = display.dimensions();
    Some(LoadedLogo {
        width,
        height,
        rgba: display.into_raw(),
    })
}

fn trim_transparent(image: image::RgbaImage) -> image::RgbaImage {
    let (width, height) = image.dimensions();
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] > 8 {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if min_x > max_x || min_y > max_y {
        return image;
    }
    let pad = 2;
    let x0 = min_x.saturating_sub(pad);
    let y0 = min_y.saturating_sub(pad);
    let x1 = (max_x + pad).min(width - 1);
    let y1 = (max_y + pad).min(height - 1);
    image::imageops::crop_imm(&image, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
}

fn square_icon(image: &image::RgbaImage, side: u32) -> egui::IconData {
    let (width, height) = image.dimensions();
    let scale = (side as f32 * 0.9) / width.max(height).max(1) as f32;
    let fitted_w = ((width as f32) * scale).round().max(1.0) as u32;
    let fitted_h = ((height as f32) * scale).round().max(1.0) as u32;
    let fitted = image::imageops::resize(
        image,
        fitted_w,
        fitted_h,
        image::imageops::FilterType::Lanczos3,
    );
    let mut canvas = image::RgbaImage::from_pixel(side, side, image::Rgba([0, 0, 0, 0]));
    image::imageops::overlay(
        &mut canvas,
        &fitted,
        ((side - fitted_w) / 2) as i64,
        ((side - fitted_h) / 2) as i64,
    );
    egui::IconData {
        rgba: canvas.into_raw(),
        width: side,
        height: side,
    }
}

fn default_accent() -> [u8; 3] {
    [DEFAULT_ACCENT.r(), DEFAULT_ACCENT.g(), DEFAULT_ACCENT.b()]
}

fn accent_from_rgb(rgb: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

fn accent_soft(accent: egui::Color32) -> egui::Color32 {
    egui::Color32::from_rgb(
        (u16::from(accent.r()) * 30 / 100) as u8,
        (u16::from(accent.g()) * 30 / 100) as u8,
        (u16::from(accent.b()) * 30 / 100) as u8,
    )
}

fn accent_ink(accent: egui::Color32) -> egui::Color32 {
    let light = 0.2126 * f32::from(accent.r())
        + 0.7152 * f32::from(accent.g())
        + 0.0722 * f32::from(accent.b());
    if light > 160.0 { INK } else { TEXT }
}

fn ui_accent(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().hyperlink_color
}

fn viewport_fullscreen(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.viewport().fullscreen.unwrap_or(false))
}

fn tune_list_selection(ui: &mut egui::Ui) {
    let accent = ui_accent(ui);
    let wash = |channel: u8| (u16::from(channel) * 52 / 100) as u8;
    let radius = egui::CornerRadius::same(4);
    let style = ui.style_mut();
    style.visuals.selection.bg_fill = egui::Color32::from_rgb(wash(accent.r()), wash(accent.g()), wash(accent.b()));
    style.visuals.selection.stroke = egui::Stroke::NONE;
    for widgets in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widgets.corner_radius = radius;
    }
}

fn accent_presets() -> [(&'static str, [u8; 3]); 5] {
    [
        ("Целадон", [78, 196, 176]),
        ("Синий", [88, 156, 232]),
        ("Янтарь", [224, 168, 72]),
        ("Коралл", [214, 112, 96]),
        ("Фиолетовый", [168, 132, 220]),
    ]
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

fn apply_style(ctx: &egui::Context, accent: egui::Color32) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = egui::Color32::from_rgb(16, 19, 24);
    visuals.extreme_bg_color = CANVAS;
    visuals.faint_bg_color = TILE;
    visuals.code_bg_color = TILE;
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.menu_corner_radius = egui::CornerRadius::same(4);
    visuals.window_stroke = egui::Stroke::new(1.0, LINE);
    let radius = egui::CornerRadius::same(4);
    for widgets in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widgets.corner_radius = radius;
        widgets.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.fg_stroke.color = TEXT;
    visuals.widgets.inactive.bg_fill = TILE;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, LINE);
    visuals.widgets.inactive.fg_stroke.color = TEXT;
    visuals.widgets.hovered.bg_fill = HOVER;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent);
    visuals.widgets.hovered.fg_stroke.color = TEXT;
    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.fg_stroke.color = accent_ink(accent);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(0.0, egui::Color32::TRANSPARENT);
    visuals.widgets.open.bg_fill = TILE;
    visuals.widgets.open.fg_stroke.color = TEXT;
    let soft = accent_soft(accent);
    visuals.selection.bg_fill = soft;
    visuals.selection.stroke = egui::Stroke::new(1.0, accent);
    visuals.hyperlink_color = accent;
    visuals.override_text_color = Some(TEXT);
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.spacing.menu_margin = egui::Margin::symmetric(8, 8);
        style.spacing.item_spacing.y = 4.0;
    });
}

fn tune_inputs(ui: &mut egui::Ui) {
    let radius = egui::CornerRadius::same(4);
    let style = ui.style_mut();
    style.spacing.button_padding = egui::vec2(12.0, 10.0);
    style.spacing.interact_size.y = 36.0;
    style.spacing.item_spacing.y = 10.0;
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::proportional(14.0),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(14.0),
    );
    for widgets in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widgets.corner_radius = radius;
    }
}

fn tune_menu(ui: &mut egui::Ui) {
    let radius = egui::CornerRadius::same(4);
    let style = ui.style_mut();
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.interact_size.y = 34.0;
    style.spacing.item_spacing = egui::vec2(0.0, 4.0);
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(14.0),
    );
    for widgets in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widgets.corner_radius = radius;
        widgets.expansion = 0.0;
    }
    style.visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    style.visuals.widgets.hovered.weak_bg_fill = HOVER;
    style.visuals.widgets.hovered.bg_fill = HOVER;
    style.visuals.widgets.active.weak_bg_fill = HOVER;
    style.visuals.widgets.active.bg_fill = HOVER;
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).color(MUTED).size(13.0));
    ui.add_space(6.0);
}

fn accent_label(ui: &egui::Ui, text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text.into())
        .size(size)
        .strong()
        .color(ui_accent(ui))
}

fn settings_section(ui: &mut egui::Ui, title: String, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0, LINE))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.label(accent_label(ui, title, 15.0));
            ui.add_space(8.0);
            add(ui);
        });
    ui.add_space(12.0);
}

fn setting_note(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.label(egui::RichText::new(text).size(13.0).color(MUTED));
    });
}

fn text_field(
    ui: &mut egui::Ui,
    value: &mut String,
    secret: bool,
    width: f32,
    hint: &str,
) -> egui::Response {
    tune_inputs(ui);
    let mut editor = egui::TextEdit::singleline(value)
        .desired_width(width)
        .font(egui::FontId::proportional(14.0))
        .vertical_align(egui::Align::Center)
        .margin(egui::Margin::symmetric(12, 10))
        .min_size(egui::vec2(0.0, 36.0));
    if !hint.is_empty() {
        editor = editor.hint_text(hint);
    }
    if secret {
        editor = editor.password(true);
    }
    ui.add(editor)
}

fn polish_toolbar(ui: &mut egui::Ui) {
    let radius = egui::CornerRadius::same(4);
    let style = ui.style_mut();
    style.spacing.button_padding = egui::vec2(12.0, 10.0);
    style.spacing.interact_size.y = 36.0;
    style.spacing.item_spacing.x = 8.0;
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(15.0),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::proportional(15.0),
    );
    for widgets in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widgets.corner_radius = radius;
    }
}

fn with_icon(icon: &str, text: &str) -> String {
    format!("{icon}  {text}")
}

fn config_nav_item(ui: &mut egui::Ui, icon: &str, title: &str, on: bool) -> egui::Response {
    let width = ui.available_width().max(1.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 36.0), egui::Sense::click());
    let fill = if on {
        ui.visuals().selection.bg_fill
    } else if response.hovered() {
        HOVER
    } else {
        egui::Color32::TRANSPARENT
    };
    if fill != egui::Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(4), fill);
    }
    let color = TEXT;
    let icon_galley = ui.painter().layout_no_wrap(
        icon.to_owned(),
        egui::FontId::proportional(16.0),
        color,
    );
    let text_galley = ui.painter().layout_no_wrap(
        title.to_owned(),
        egui::FontId::proportional(14.0),
        color,
    );
    let slot = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 10.0, rect.top()),
        egui::vec2(22.0, rect.height()),
    );
    ui.painter().galley(
        slot.center() - icon_galley.size() * 0.5,
        icon_galley,
        color,
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 40.0, rect.center().y - text_galley.size().y * 0.5),
        text_galley,
        color,
    );
    response
}

fn layout_icon(layout: u32) -> &'static str {
    match layout {
        1 => ph::SQUARE,
        4 => ph::SQUARES_FOUR,
        9 => ph::GRID_NINE,
        _ => ph::DOTS_NINE,
    }
}

fn page_icon(page: Page) -> &'static str {
    match page {
        Page::Home => ph::HOUSE,
        Page::Live => ph::VIDEO_CAMERA,
        Page::Device => ph::HARD_DRIVES,
        Page::Archive => ph::CLOCK_COUNTER_CLOCKWISE,
        Page::Journal => ph::CLIPBOARD_TEXT,
        Page::Settings => ph::GEAR,
        Page::Config => ph::SLIDERS,
        Page::Later(title, _) => later_icon(title),
    }
}

fn later_icon(title: &str) -> &'static str {
    match title {
        "Журнал" => ph::CLIPBOARD_TEXT,
        "Конфигуратор" => ph::SLIDERS,
        _ => ph::CIRCLE,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Live,
    Device,
    Archive,
    Journal,
    Settings,
    Config,
    #[allow(dead_code)]
    Later(&'static str, &'static str),
}

struct Picture {
    width: usize,
    height: usize,
    rgba: Arc<Vec<u8>>,
    seq: u64,
}

impl Picture {
    fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            rgba: Arc::new(Vec::new()),
            seq: 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Source {
    device_id: u64,
    channel: u16,
}

struct RenameDraft {
    device_id: u64,
    channel: u16,
    text: String,
    focus: bool,
}

struct ListClick {
    source: Source,
    cell: usize,
    at: std::time::Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Vendor {
    Xm,
    Hikvision,
}

impl Default for Vendor {
    fn default() -> Self {
        Self::Xm
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceDialog {
    Add,
    Edit(u64),
    Details(u64),
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SavedDevice {
    id: u64,
    name: String,
    host: String,
    port: String,
    user: String,
    password: String,
    #[serde(default)]
    kind: Vendor,
}

#[derive(Clone)]
struct LiveDevice {
    id: u64,
    name: String,
    host: String,
    port: u16,
    user: String,
    password: String,
    kind: Vendor,
    door: bool,
    serial: String,
    software: String,
    hardware: String,
    channels: u16,
    titles: Vec<String>,
    links: Vec<String>,
    connected: bool,
    error: String,
}

impl LiveDevice {
    fn from_saved(saved: SavedDevice) -> Option<Self> {
        let port = saved.port.trim().parse().ok()?;
        Some(Self {
            id: saved.id,
            name: saved.name,
            host: saved.host,
            port,
            user: saved.user,
            password: saved.password,
            kind: saved.kind,
            door: false,
            serial: String::new(),
            software: String::new(),
            hardware: String::new(),
            channels: 0,
            titles: Vec::new(),
            links: Vec::new(),
            connected: false,
            error: String::new(),
        })
    }

    fn saved(&self) -> SavedDevice {
        SavedDevice {
            id: self.id,
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port.to_string(),
            user: self.user.clone(),
            password: self.password.clone(),
            kind: self.kind,
        }
    }

    fn display_name(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            self.host.clone()
        } else {
            name.to_string()
        }
    }
}

struct PcmChunk {
    rate: u32,
    samples: Vec<i16>,
}

struct StreamSlot {
    generation: AtomicU64,
    killer: Mutex<Option<TcpStream>>,
    picture: Mutex<Picture>,
    audio_out: Mutex<Option<SyncSender<PcmChunk>>>,
    awake: AtomicBool,
    clock: Mutex<Option<WallTime>>,
    play_speed: AtomicU8,
    seek_to: Mutex<Option<String>>,
}

impl StreamSlot {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            generation: AtomicU64::new(0),
            killer: Mutex::new(None),
            picture: Mutex::new(Picture::new()),
            audio_out: Mutex::new(None),
            awake: AtomicBool::new(true),
            clock: Mutex::new(None),
            play_speed: AtomicU8::new(1),
            seek_to: Mutex::new(None),
        })
    }

    fn begin(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(socket) = self.killer.lock().unwrap().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        let mut picture = self.picture.lock().unwrap();
        picture.rgba = Arc::new(Vec::new());
        picture.width = 0;
        picture.height = 0;
        picture.seq = 0;
        drop(picture);
        *self.clock.lock().unwrap() = None;
        *self.seek_to.lock().unwrap() = None;
        generation
    }

    fn stop(&self) {
        let _ = self.begin();
    }

    fn still(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    fn arm(&self, generation: u64, socket: TcpStream) {
        if !self.still(generation) {
            let _ = socket.shutdown(Shutdown::Both);
            return;
        }
        *self.killer.lock().unwrap() = Some(socket);
        if !self.still(generation) {
            if let Some(socket) = self.killer.lock().unwrap().take() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }
}

struct Cell {
    source: Option<Source>,
    slot: Arc<StreamSlot>,
    seen: u64,
    texture: Option<egui::TextureHandle>,
}

impl Cell {
    fn new() -> Self {
        Self {
            source: None,
            slot: StreamSlot::new(),
            seen: 0,
            texture: None,
        }
    }
}

#[derive(Clone)]
struct PlayingClip {
    path: String,
    begin: String,
    end: String,
}

#[derive(Clone)]
struct JournalRow {
    time: String,
    device: String,
    kind: String,
    channel: Option<u16>,
    detail: String,
}

#[derive(Clone)]
struct DownloadDraft {
    channel: u16,
    path: String,
    file_begin: String,
    file_end: String,
    limit_begin: String,
    limit_end: String,
    begin: String,
    end: String,
    bytes: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DoorPhase {
    Idle,
    Ring,
    Talk,
}

#[derive(Clone)]
struct DoorReport {
    phase: DoorPhase,
    device_id: u64,
    name: String,
}

impl Default for DoorReport {
    fn default() -> Self {
        Self {
            phase: DoorPhase::Idle,
            device_id: 0,
            name: String::new(),
        }
    }
}

#[derive(Clone, Copy)]
enum ConfigConfirm {
    Network,
    Users,
    Reboot,
    Format { disk: usize, part: usize, armed: bool },
}

#[derive(Clone, Copy)]
struct ConfigClock {
    epoch: i64,
    at: Instant,
}

struct CallHold {
    page: Page,
    focus: Option<usize>,
    selected: usize,
    index: usize,
    previous: Option<Source>,
    replaced: bool,
    door_id: u64,
}

struct Viewer {
    page: Page,
    open_tabs: Vec<Page>,
    draft_name: String,
    draft_kind: Vendor,
    host: String,
    port: String,
    user: String,
    password: String,
    day: String,
    archive_from: String,
    archive_to: String,
    archive_picker: bool,
    picker_from: String,
    picker_to: String,
    picker_editing_from: bool,
    picker_year: i32,
    picker_month: u32,
    picker_hour: String,
    picker_minute: String,
    archive_speed: u8,
    download_busy: Arc<AtomicBool>,
    download_cancel: Arc<AtomicBool>,
    download_got: Arc<AtomicU64>,
    download_total: Arc<AtomicU64>,
    download_path: Arc<Mutex<String>>,
    download_at: Arc<AtomicI64>,
    download_from: i64,
    download_to: i64,
    download_draft: Option<DownloadDraft>,
    archive_playing: Option<PlayingClip>,
    seek_preview: Option<(i64, Instant, i64)>,
    archive_device: u64,
    archive_channel: u16,
    layout: u32,
    stream: StreamKind,
    default_stream: StreamKind,
    hwaccel: bool,
    download_mp4: bool,
    accent: egui::Color32,
    focus: Option<usize>,
    selected_cell: usize,
    cells: Vec<Cell>,
    archive: Arc<StreamSlot>,
    archive_seen: u64,
    archive_texture: Option<egui::TextureHandle>,
    status: Arc<Mutex<String>>,
    devices: Arc<Mutex<Vec<LiveDevice>>>,
    next_id: u64,
    recordings: Arc<Mutex<Vec<Recording>>>,
    journal: Arc<Mutex<Vec<JournalRow>>>,
    journal_from: String,
    journal_to: String,
    journal_busy: Arc<AtomicBool>,
    journal_filter: u8,
    picker_journal: bool,
    config_device: u64,
    config_section: config::ConfigSection,
    config_channel: u16,
    config_user: usize,
    config_day: usize,
    config_busy: Arc<AtomicBool>,
    config_bundle: Arc<Mutex<Option<config::ConfigBundle>>>,
    config_token: Arc<AtomicU64>,
    config_password: String,
    config_clock: Option<ConfigClock>,
    config_time_draft: String,
    config_time_editing: bool,
    config_format_name: String,
    config_confirm: Option<ConfigConfirm>,
    last_saved: String,
    aspect: Aspect,
    audio_cell: Option<usize>,
    ptz_tx: mpsc::Sender<PtzJob>,
    ptz_step: u8,
    ptz_cmd: Option<&'static str>,
    ptz_hold: Option<&'static str>,
    ptz_open: bool,
    links_at: std::time::Instant,
    links_busy: Arc<AtomicBool>,
    door_confirm: Option<u64>,
    door_slide: f32,
    door_drag: bool,
    door_moved: bool,
    door_call: bool,
    door_camera: Option<Source>,
    door_report: Arc<Mutex<DoorReport>>,
    door_watch_gen: Arc<AtomicU64>,
    door_watch_alive: Arc<AtomicBool>,
    call_hold: Option<CallHold>,
    call_dismissed: bool,
    call_ended: bool,
    call_phase: DoorPhase,
    call_door_name: String,
    rename: Option<RenameDraft>,
    device_dialog: Option<DeviceDialog>,
    rename_busy: Arc<AtomicBool>,
    list_click: Option<ListClick>,
    texture_cursor: usize,
    persist_at: std::time::Instant,
    logo: Option<egui::TextureHandle>,
}

impl Viewer {
    fn new(logo: Option<egui::TextureHandle>) -> Self {
        let saved = Settings::load();
        let devices: Vec<LiveDevice> = saved
            .devices
            .into_iter()
            .filter_map(LiveDevice::from_saved)
            .collect();
        let max_id = devices.iter().map(|device| device.id).max().unwrap_or(0);
        let next_id = saved.next_id.max(max_id + 1).max(1);
        let archive_device = if devices.iter().any(|device| device.id == saved.archive_device) {
            saved.archive_device
        } else {
            devices.first().map(|device| device.id).unwrap_or(0)
        };
        let mut cells: Vec<Cell> = (0..MAX_CELLS).map(|_| Cell::new()).collect();
        for (index, source) in saved.cells.iter().enumerate().take(MAX_CELLS) {
            cells[index].source = *source;
        }
        let status = if devices.is_empty() {
            "Добавьте регистратор в разделе «Устройства».".into()
        } else {
            "Подключение устройств…".into()
        };
        let status = Arc::new(Mutex::new(status));
        let page = page_from_name(&saved.page);
        let open_tabs = if page == Page::Home {
            Vec::new()
        } else {
            vec![page]
        };
        let (archive_from, archive_to) =
            normalize_range(&saved.archive_from, &saved.archive_to, &saved.day);
        let day = archive_from
            .get(..10)
            .unwrap_or(saved.day.as_str())
            .to_string();
        let journal_from = format!("{day} 00:00:00");
        let journal_to = format!("{day} 23:59:59");
        let (picker_year, picker_month) = parse_stamp(&archive_from)
            .map(|(year, month, _, _, _, _)| (year, month))
            .unwrap_or((2026, 10));
        let mut viewer = Self {
            page,
            open_tabs,
            draft_name: String::new(),
            draft_kind: Vendor::Xm,
            host: if devices.is_empty() {
                saved.host
            } else {
                String::new()
            },
            port: saved.port,
            user: saved.user,
            password: if devices.is_empty() {
                saved.password
            } else {
                String::new()
            },
            day,
            archive_from,
            archive_to,
            archive_picker: false,
            picker_from: String::new(),
            picker_to: String::new(),
            picker_editing_from: true,
            picker_year,
            picker_month,
            picker_hour: String::new(),
            picker_minute: String::new(),
            archive_speed: 1,
            download_busy: Arc::new(AtomicBool::new(false)),
            download_cancel: Arc::new(AtomicBool::new(false)),
            download_got: Arc::new(AtomicU64::new(0)),
            download_total: Arc::new(AtomicU64::new(0)),
            download_path: Arc::new(Mutex::new(String::new())),
            download_at: Arc::new(AtomicI64::new(0)),
            download_from: 0,
            download_to: 0,
            download_draft: None,
            archive_playing: None,
            seek_preview: None,
            archive_device,
            archive_channel: saved.archive_channel,
            layout: saved.layout,
    stream: stream_from_name(&saved.stream),
    default_stream: stream_from_name(&saved.stream),
            hwaccel: saved.hwaccel,
            download_mp4: saved.download_mp4,
            accent: accent_from_rgb(saved.accent),
            focus: None,
            selected_cell: 0,
            cells,
            archive: StreamSlot::new(),
            archive_seen: 0,
            archive_texture: None,
            status: Arc::clone(&status),
            devices: Arc::new(Mutex::new(devices)),
            next_id,
            recordings: Arc::new(Mutex::new(Vec::new())),
            journal: Arc::new(Mutex::new(Vec::new())),
            journal_from,
            journal_to,
            journal_busy: Arc::new(AtomicBool::new(false)),
            journal_filter: 1,
            picker_journal: false,
            config_device: 0,
            config_section: config::ConfigSection::Channels,
            config_channel: 0,
            config_user: 0,
            config_day: 0,
            config_busy: Arc::new(AtomicBool::new(false)),
            config_bundle: Arc::new(Mutex::new(None)),
            config_token: Arc::new(AtomicU64::new(0)),
            config_password: String::new(),
            config_clock: None,
            config_time_draft: String::new(),
            config_time_editing: false,
            config_format_name: String::new(),
            config_confirm: None,
            last_saved: String::new(),
            aspect: aspect_from_name(&saved.aspect),
            audio_cell: None,
            ptz_tx: spawn_ptz_worker(Arc::clone(&status)),
            ptz_step: 1,
            ptz_cmd: None,
            ptz_hold: None,
            ptz_open: false,
            links_at: std::time::Instant::now(),
            links_busy: Arc::new(AtomicBool::new(false)),
            door_confirm: None,
            door_slide: 0.0,
            door_drag: false,
            door_moved: false,
            door_call: saved.door_call,
            door_camera: saved.door_camera,
            door_report: Arc::new(Mutex::new(DoorReport::default())),
            door_watch_gen: Arc::new(AtomicU64::new(0)),
            door_watch_alive: Arc::new(AtomicBool::new(false)),
            call_hold: None,
            call_dismissed: false,
            call_ended: false,
            call_phase: DoorPhase::Idle,
            call_door_name: String::new(),
            rename: None,
            device_dialog: None,
            rename_busy: Arc::new(AtomicBool::new(false)),
            list_click: None,
            texture_cursor: 0,
            persist_at: std::time::Instant::now(),
            logo,
        };
        let ids: Vec<u64> = viewer
            .devices
            .lock()
            .unwrap()
            .iter()
            .map(|device| device.id)
            .collect();
        for id in ids {
            viewer.probe(id);
        }
        if viewer.page == Page::Live {
            viewer.enforce_unique_sources();
            viewer.sync_streams();
        } else {
            viewer.enforce_unique_sources();
        }
        viewer.last_saved = viewer.snapshot();
        viewer
    }

    fn snapshot(&self) -> String {
        let devices = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .map(LiveDevice::saved)
            .collect();
        let settings = Settings {
            host: self.host.clone(),
            port: self.port.clone(),
            user: self.user.clone(),
            password: self.password.clone(),
            devices,
            next_id: self.next_id,
            day: self
                .archive_from
                .get(..10)
                .unwrap_or(self.day.as_str())
                .to_string(),
            archive_from: self.archive_from.clone(),
            archive_to: self.archive_to.clone(),
            archive_device: self.archive_device,
            archive_channel: self.archive_channel,
            layout: self.layout,
            stream: stream_name(self.default_stream).into(),
            hwaccel: self.hwaccel,
            download_mp4: self.download_mp4,
            door_call: self.door_call,
            door_camera: self.door_camera,
            accent: [self.accent.r(), self.accent.g(), self.accent.b()],
            aspect: aspect_name_saved(self.aspect).into(),
            page: page_name(self.page),
            cells: self.cells.iter().map(|cell| cell.source).collect(),
        };
        serde_json::to_string_pretty(&settings).unwrap_or_default()
    }

    fn persist(&mut self) {
        if self.persist_at.elapsed() < Duration::from_millis(400) {
            return;
        }
        self.persist_at = std::time::Instant::now();
        let text = self.snapshot();
        if text.is_empty() || text == self.last_saved {
            return;
        }
        if std::fs::write(settings_path(), &text).is_ok() {
            self.last_saved = text;
        }
    }

    fn device_credentials(
        &self,
        id: u64,
    ) -> Result<(Vendor, String, u16, String, String), String> {
        let devices = self.devices.lock().unwrap();
        let device = devices
            .iter()
            .find(|item| item.id == id)
            .or_else(|| devices.first())
            .ok_or("Добавьте устройство")?;
        Ok((
            device.kind,
            device.host.clone(),
            device.port,
            device.user.clone(),
            device.password.clone(),
        ))
    }

    fn set_status(&self, text: impl Into<String>) {
        *self.status.lock().unwrap() = text.into();
    }

    fn status_line(&self) -> String {
        if self.download_busy.load(Ordering::Acquire) {
            return self.download_caption();
        }
        self.status.lock().unwrap().clone()
    }

    fn download_percent(&self) -> Option<u32> {
        let at = self.download_at.load(Ordering::Relaxed);
        let from = self.download_from;
        let to = self.download_to;
        if at > 0 && to > from {
            let at = at.clamp(from, to);
            return Some((((at - from) as f64 / (to - from) as f64) * 100.0).round() as u32);
        }
        let got = self.download_got.load(Ordering::Relaxed);
        let total = self.download_total.load(Ordering::Relaxed);
        if total > 0 && got > 0 && got <= total {
            return Some((((got as f64 / total as f64) * 100.0).round() as u32).min(100));
        }
        None
    }

    fn download_caption(&self) -> String {
        let got = self.download_got.load(Ordering::Relaxed);
        match self.download_percent() {
            Some(percent) if got > 0 => format!("Скачивание {percent}% · {}", format_bytes(got)),
            Some(percent) => format!("Скачивание {percent}%"),
            None if got > 0 => format!("Скачивание {}…", format_bytes(got)),
            None => "Скачивание…".into(),
        }
    }

    fn source_label(&self, source: Source) -> String {
        let devices = self.devices.lock().unwrap();
        let device = devices.iter().find(|item| item.id == source.device_id);
        let title = channel_title(device, source.channel);
        if devices.len() > 1 {
            let name = device
                .map(LiveDevice::display_name)
                .unwrap_or_else(|| "?".into());
            format!("{name} · {title}")
        } else {
            title
        }
    }

    fn stream_kind(&self) -> StreamKind {
        self.stream
    }

    fn set_stream(&mut self, stream: StreamKind) {
        if self.stream == stream {
            return;
        }
        self.stream = stream;
        self.set_status(match stream {
            StreamKind::Main => "Основной поток",
            StreamKind::Extra => "Дополнительный поток",
        });
        if self.page == Page::Live {
            self.sync_streams();
        }
    }

    fn swap_cells(&mut self, from: usize, to: usize) {
        if from == to || from >= self.cells.len() || to >= self.cells.len() {
            return;
        }
        self.cells.swap(from, to);
        self.selected_cell = swapped_index(self.selected_cell, from, to);
        self.focus = self.focus.map(|index| swapped_index(index, from, to));
        self.audio_cell = self.audio_cell.map(|index| swapped_index(index, from, to));
    }

    fn grid_indices(&self) -> Vec<usize> {
        (0..self.layout as usize).collect()
    }

    fn set_page(&mut self, page: Page) {
        let was_live = self.page == Page::Live;
        let was_archive = self.page == Page::Archive;
        let was_journal = self.page == Page::Journal;
        let was_config = self.page == Page::Config;
        if page != Page::Home && !self.open_tabs.contains(&page) {
            self.open_tabs.push(page);
        }
        self.page = page;
        if was_live && self.page != Page::Live {
            self.set_audio(None);
            self.stop_grid();
        }
        if was_archive && self.page != Page::Archive {
            self.archive.stop();
            self.archive_seen = 0;
            self.archive_texture = None;
        }
        if self.page == Page::Live && !was_live {
            self.sync_streams();
        }
        if self.page == Page::Archive && !was_archive {
            self.search();
        }
        if self.page == Page::Journal && !was_journal {
            self.search_journal();
        }
        if self.page == Page::Config && !was_config {
            self.load_config(false);
        }
    }

    fn close_tab(&mut self, page: Page) {
        let Some(index) = self.open_tabs.iter().position(|item| *item == page) else {
            return;
        };
        self.open_tabs.remove(index);
        if self.page != page {
            return;
        }
        let next = self
            .open_tabs
            .get(index)
            .or_else(|| index.checked_sub(1).and_then(|prev| self.open_tabs.get(prev)))
            .copied()
            .unwrap_or(Page::Home);
        self.set_page(next);
    }

    fn stop_grid(&mut self) {
        for cell in &self.cells {
            cell.slot.stop();
        }
    }

    fn set_layout(&mut self, layout: u32) {
        self.focus = None;
        self.layout = layout;
        if self.selected_cell >= layout as usize {
            self.selected_cell = 0;
        }
        if self.page == Page::Live {
            self.sync_streams();
        }
    }

    fn sync_streams(&mut self) {
        let active = self.grid_indices();
        let stream = self.stream_kind();
        let plan: Vec<(usize, Option<Source>)> = (0..MAX_CELLS)
            .map(|index| (index, self.cells[index].source))
            .collect();
        for (index, source) in plan {
            if active.contains(&index) {
                if let Some(source) = source {
                    self.start_cell(index, source, stream);
                }
            } else {
                self.cells[index].slot.stop();
            }
        }
    }

    fn finish_list_click(&mut self, ctx: &egui::Context) {
        let Some(click) = &self.list_click else {
            return;
        };
        let wait = Duration::from_millis(300);
        let elapsed = click.at.elapsed();
        if elapsed < wait {
            ctx.request_repaint_after(wait.saturating_sub(elapsed));
            return;
        }
        let Some(click) = self.list_click.take() else {
            return;
        };
        self.focus_shown(click.source);
    }

    fn focus_shown(&mut self, source: Source) {
        let Some(index) = self.find_shown(source) else {
            self.set_status("Эта камера не открыта в сетке");
            return;
        };
        self.selected_cell = index;
        self.focus = None;
        self.set_status(self.source_label(source));
    }

    fn find_shown(&self, source: Source) -> Option<usize> {
        let limit = (self.layout as usize).min(self.cells.len());
        self.cells
            .iter()
            .enumerate()
            .take(limit)
            .find(|(_, cell)| cell.source == Some(source))
            .map(|(index, _)| index)
    }

    fn channel_placed(&self, source: Source) -> bool {
        self.find_shown(source).is_some()
    }

    fn open_channel(&mut self, index: usize, source: Source) {
        if index >= self.cells.len() {
            return;
        }
        self.selected_cell = index;
        let already = self.cells[index].source == Some(source);
        self.cells[index].source = Some(source);
        self.clear_source_except(index, source);
        if already {
            self.set_status(self.source_label(source));
            return;
        }
        let stream = self.stream_kind();
        let name = self.source_label(source);
        self.set_status(format!("Открываю {name}"));
        self.start_cell(index, source, stream);
    }

    fn clear_source_except(&mut self, keep: usize, source: Source) {
        let stale: Vec<usize> = self
            .cells
            .iter()
            .enumerate()
            .filter_map(|(index, cell)| {
                (index != keep && cell.source == Some(source)).then_some(index)
            })
            .collect();
        let audio_moved = self.audio_cell.is_some_and(|index| stale.contains(&index));
        for index in stale {
            self.cells[index].source = None;
            self.cells[index].slot.stop();
            self.cells[index].seen = 0;
            self.cells[index].texture = None;
            if self.focus == Some(index) {
                self.focus = Some(keep);
            }
            if self.selected_cell == index {
                self.selected_cell = keep;
            }
        }
        if audio_moved {
            self.set_audio(Some(keep));
        }
    }

    fn enforce_unique_sources(&mut self) {
        let mut seen = Vec::new();
        let sources: Vec<(usize, Source)> = self
            .cells
            .iter()
            .enumerate()
            .filter_map(|(index, cell)| cell.source.map(|source| (index, source)))
            .collect();
        for (index, source) in sources {
            if seen.contains(&source) {
                continue;
            }
            seen.push(source);
            self.clear_source_except(index, source);
        }
    }

    fn fill_grid(&mut self) {
        let sources = {
            let devices = self.devices.lock().unwrap();
            let mut sources = Vec::new();
            for device in devices.iter() {
                for channel in 0..device_channel_count(device) {
                    if !channel_available(device, channel) {
                        continue;
                    }
                    sources.push(Source {
                        device_id: device.id,
                        channel,
                    });
                }
            }
            sources
        };
        if sources.is_empty() {
            self.set_status("Нет каналов: дождитесь подключения или добавьте устройство");
            return;
        }
        let visible = if self.focus.is_some() {
            1
        } else {
            self.layout as usize
        };
        for index in 0..visible {
            let next = sources.get(index).copied();
            if next.is_none() && self.cells[index].source.is_some() {
                self.cells[index].slot.stop();
                self.cells[index].seen = 0;
                self.cells[index].texture = None;
            }
            self.cells[index].source = next;
        }
        self.enforce_unique_sources();
        self.sync_streams();
        self.set_status(format!(
            "Сетка: {} из {} каналов",
            visible.min(sources.len()),
            sources.len()
        ));
    }

    fn start_cell(&mut self, index: usize, source: Source, stream: StreamKind) {
        let (kind, host, port, user, password) = match self.device_credentials(source.device_id) {
            Ok(value) => value,
            Err(err) => {
                self.set_status(err);
                return;
            }
        };
        let slot = Arc::clone(&self.cells[index].slot);
        let generation = slot.begin();
        self.cells[index].seen = 0;
        self.cells[index].texture = None;
        let status = Arc::clone(&self.status);
        let hwaccel = self.hwaccel;
        let channel = source.channel;
        if kind == Vendor::Hikvision {
            thread::spawn(move || {
                run_hik_live(slot, generation, status, host, user, password, channel, stream, hwaccel);
            });
            return;
        }
        thread::spawn(move || {
            run_media(
                slot,
                generation,
                status,
                false,
                host,
                port,
                user,
                password,
                MediaJob::Live {
                    channel,
                    stream,
                },
                String::new(),
                hwaccel,
            );
        });
    }

    fn add_device(&mut self) -> bool {
        let port = match self.port.trim().parse::<u16>() {
            Ok(port) => port,
            Err(_) => {
                self.set_status("порт должен быть числом");
                return false;
            }
        };
        let host = self.host.trim().to_string();
        if host.is_empty() {
            self.set_status("нужен адрес регистратора");
            return false;
        }
        let id = self.next_id;
        self.next_id += 1;
        let name = {
            let trimmed = self.draft_name.trim();
            if trimmed.is_empty() {
                host.clone()
            } else {
                trimmed.to_string()
            }
        };
        self.devices.lock().unwrap().push(LiveDevice {
            id,
            name,
            host,
            port,
            user: self.user.trim().to_string(),
            password: self.password.clone(),
            kind: self.draft_kind,
            door: false,
            serial: String::new(),
            software: String::new(),
            hardware: String::new(),
            channels: 0,
            titles: Vec::new(),
            links: Vec::new(),
            connected: false,
            error: String::new(),
        });
        if self.archive_device == 0 {
            self.archive_device = id;
        }
        self.draft_name.clear();
        self.host.clear();
        self.password.clear();
        self.set_status("Подключение…");
        self.probe(id);
        true
    }

    fn write_device(&mut self, id: u64) -> bool {
        let port = match self.port.trim().parse::<u16>() {
            Ok(port) => port,
            Err(_) => {
                self.set_status("порт должен быть числом");
                return false;
            }
        };
        let host = self.host.trim().to_string();
        if host.is_empty() {
            self.set_status("нужен адрес регистратора");
            return false;
        }
        let name = {
            let trimmed = self.draft_name.trim();
            if trimmed.is_empty() {
                host.clone()
            } else {
                trimmed.to_string()
            }
        };
        let found = {
            let mut list = self.devices.lock().unwrap();
            let Some(device) = list.iter_mut().find(|device| device.id == id) else {
                return false;
            };
            device.name = name;
            device.host = host;
            device.port = port;
            device.user = self.user.trim().to_string();
            device.password = self.password.clone();
            device.kind = self.draft_kind;
            device.door = false;
            device.serial.clear();
            device.software.clear();
            device.hardware.clear();
            device.channels = 0;
            device.titles.clear();
            device.links.clear();
            device.connected = false;
            device.error.clear();
            true
        };
        if found {
            self.set_status("Подключение…");
            self.probe(id);
        }
        found
    }

    fn remove_device(&mut self, id: u64) {
        self.devices.lock().unwrap().retain(|device| device.id != id);
        for cell in &mut self.cells {
            if cell.source.is_some_and(|source| source.device_id == id) {
                cell.source = None;
                cell.slot.stop();
                cell.seen = 0;
                cell.texture = None;
            }
        }
        if self.archive_device == id {
            self.archive_device = self
                .devices
                .lock()
                .unwrap()
                .first()
                .map(|device| device.id)
                .unwrap_or(0);
        }
        self.set_status("Устройство удалено");
    }

    fn refresh_links(&mut self) {
        if self.links_at.elapsed() < Duration::from_secs(20) {
            return;
        }
        if self.links_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.links_at = std::time::Instant::now();
        let jobs: Vec<(u64, String, u16, String, String)> = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .filter(|device| device.connected && device.kind == Vendor::Xm)
            .map(|device| {
                (
                    device.id,
                    device.host.clone(),
                    device.port,
                    device.user.clone(),
                    device.password.clone(),
                )
            })
            .collect();
        let devices = Arc::clone(&self.devices);
        let busy = Arc::clone(&self.links_busy);
        thread::spawn(move || {
            for (id, host, port, user, password) in jobs {
                let Ok(mut session) = Session::connect(&host, port, &user, &password) else {
                    continue;
                };
                let Ok(links) = session.channel_links() else {
                    continue;
                };
                if let Some(device) = devices
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|device| device.id == id)
                {
                    device.links = links;
                }
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    fn probe(&self, id: u64) {
        let Some((kind, host, port, user, password, name)) = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .find(|device| device.id == id)
            .map(|device| {
                (
                    device.kind,
                    device.host.clone(),
                    device.port,
                    device.user.clone(),
                    device.password.clone(),
                    device.display_name(),
                )
            })
        else {
            return;
        };
        let status = Arc::clone(&self.status);
        let devices = Arc::clone(&self.devices);
        thread::spawn(move || {
            probe_device(status, devices, id, kind, name, host, port, user, password)
        });
    }

    fn search(&mut self) {
        let (begin, end) = match prepare_range(&self.archive_from, &self.archive_to) {
            Ok(range) => range,
            Err(message) => {
                self.set_status(message);
                return;
            }
        };
        self.archive_from = begin.clone();
        self.archive_to = end.clone();
        if let Some(day) = self.archive_from.get(..10) {
            self.day = day.to_string();
        }
        let (kind, host, port, user, password) = match self.device_credentials(self.archive_device) {
            Ok(value) => value,
            Err(err) => {
                self.set_status(err);
                return;
            }
        };
        self.archive.stop();
        self.archive_seen = 0;
        self.archive_texture = None;
        self.archive_playing = None;
        self.seek_preview = None;
        self.download_draft = None;
        self.recordings.lock().unwrap().clear();
        if kind == Vendor::Hikvision {
            self.set_status("Архив Hikvision пока не подключён");
            return;
        }
        let channel = self.archive_channel;
        self.set_status("Поиск записей…");
        let status = Arc::clone(&self.status);
        let recordings = Arc::clone(&self.recordings);
        thread::spawn(move || match Session::connect(&host, port, &user, &password) {
            Ok(mut session) => match session.search(channel, &begin, &end) {
                Ok(files) => {
                    let count = files.len();
                    *recordings.lock().unwrap() = files;
                    *status.lock().unwrap() = format!("Записей: {count}");
                }
                Err(err) => *status.lock().unwrap() = err.to_string(),
            },
            Err(err) => *status.lock().unwrap() = err.to_string(),
        });
    }

    fn search_journal(&mut self) {
        if self
            .journal_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let (begin, end) = match prepare_range(&self.journal_from, &self.journal_to) {
            Ok(range) => range,
            Err(message) => {
                self.journal_busy.store(false, Ordering::Release);
                self.set_status(message);
                return;
            }
        };
        self.journal_from = begin.clone();
        self.journal_to = end.clone();
        let jobs: Vec<(String, String, u16, String, String)> = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .filter(|device| device.kind == Vendor::Xm)
            .map(|device| {
                (
                    device.display_name(),
                    device.host.clone(),
                    device.port,
                    device.user.clone(),
                    device.password.clone(),
                )
            })
            .collect();
        let hikvision = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .any(|device| device.kind == Vendor::Hikvision);
        if jobs.is_empty() {
            self.journal.lock().unwrap().clear();
            self.journal_busy.store(false, Ordering::Release);
            self.set_status(if hikvision {
                "Журнал Hikvision пока не подключён"
            } else {
                "Добавьте регистратор"
            });
            return;
        }
        self.journal.lock().unwrap().clear();
        self.set_status("Читаю журнал регистраторов…");
        let status = Arc::clone(&self.status);
        let rows = Arc::clone(&self.journal);
        let busy = Arc::clone(&self.journal_busy);
        thread::spawn(move || {
            let mut all = Vec::new();
            let mut failed = Vec::new();
            for (name, host, port, user, password) in jobs {
                match Session::connect(&host, port, &user, &password) {
                    Ok(mut session) => match session.search_logs(&begin, &end) {
                        Ok(logs) => {
                            for log in logs {
                                let (kind, channel, detail) = interpret_log(&log);
                                all.push(JournalRow {
                                    time: log.time,
                                    device: name.clone(),
                                    kind,
                                    channel,
                                    detail,
                                });
                            }
                        }
                        Err(err) => failed.push(format!("{name}: {err}")),
                    },
                    Err(err) => failed.push(format!("{name}: {err}")),
                }
            }
            all.sort_by(|left, right| right.time.cmp(&left.time));
            let count = all.len();
            *rows.lock().unwrap() = all;
            let mut text = format!("Событий: {count}");
            if let Some(err) = failed.first() {
                text = format!("{text} · {err}");
            }
            if hikvision {
                text = format!("{text} · журнал Hikvision пока не подключён");
            }
            *status.lock().unwrap() = text;
            busy.store(false, Ordering::Release);
        });
    }

    fn play(&mut self, recording: Recording) {
        let (kind, host, port, user, password) = match self.device_credentials(self.archive_device) {
            Ok(value) => value,
            Err(err) => {
                self.set_status(err);
                return;
            }
        };
        if kind == Vendor::Hikvision {
            self.set_status("Архив Hikvision пока не подключён");
            return;
        }
        let generation = self.archive.begin();
        self.archive_seen = 0;
        self.archive_texture = None;
        self.archive_playing = Some(PlayingClip {
            path: recording.path.clone(),
            begin: recording.begin.clone(),
            end: recording.end.clone(),
        });
        self.seek_preview = None;
        let label = format!("Архив {} – {}", recording.begin, recording.end);
        self.set_status(format!("{label}: ожидание кадра…"));
        let spec = PlaybackSpec {
            channel: recording.channel,
            begin: recording.begin.clone(),
            end: recording.end.clone(),
            path: Some(recording.path),
        };
        let slot = Arc::clone(&self.archive);
        let status = Arc::clone(&self.status);
        let hwaccel = self.hwaccel;
        thread::spawn(move || {
            run_media(
                slot,
                generation,
                status,
                true,
                host,
                port,
                user,
                password,
                MediaJob::Play(spec),
                label,
                hwaccel,
            );
        });
    }

    fn download_recording(&mut self, recording: Recording) {
        if self
            .download_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.set_status("Уже скачивается другой файл");
            return;
        }
        let (kind, host, port, user, password) =
            match self.device_credentials(self.archive_device) {
                Ok(value) => value,
                Err(err) => {
                    self.download_busy.store(false, Ordering::Release);
                    self.set_status(err);
                    return;
                }
            };
        if kind == Vendor::Hikvision {
            self.download_busy.store(false, Ordering::Release);
            self.set_status("Архив Hikvision пока не подключён");
            return;
        }
        let pack = self.download_mp4;
        let dir = download_dir();
        let stem = download_stem(&recording);
        let raw_path = dir.join(format!("{stem}.h264"));
        let part_path = dir.join(format!("{stem}.part"));
        let pcm_path = dir.join(format!("{stem}.pcm"));
        let mp4_path = dir.join(format!("{stem}.mp4"));
        let path = if pack { mp4_path.clone() } else { raw_path.clone() };
        self.download_cancel.store(false, Ordering::Release);
        self.download_got.store(0, Ordering::Relaxed);
        self.download_total.store(recording.bytes, Ordering::Relaxed);
        self.download_from = stamp_epoch(&recording.begin).unwrap_or(0);
        self.download_to = stamp_epoch(&recording.end).unwrap_or(0);
        self.download_at.store(0, Ordering::Relaxed);
        if !recording.path.is_empty() {
            *self.download_path.lock().unwrap() = recording.path.clone();
        }
        self.set_status(format!("Скачиваю {}…", path.display()));
        let status = Arc::clone(&self.status);
        let busy = Arc::clone(&self.download_busy);
        let got = Arc::clone(&self.download_got);
        let at = Arc::clone(&self.download_at);
        let cancel = Arc::clone(&self.download_cancel);
        let spec = PlaybackSpec {
            channel: recording.channel,
            begin: recording.begin,
            end: recording.end,
            path: if recording.path.is_empty() {
                None
            } else {
                Some(recording.path)
            },
        };
        thread::spawn(move || {
            let result = (|| -> Result<(u64, u8, u32), String> {
                let video_path = if pack { &part_path } else { &raw_path };
                let mut file = File::create(video_path).map_err(|err| err.to_string())?;
                let mut pcm = if pack {
                    Some(File::create(&pcm_path).map_err(|err| err.to_string())?)
                } else {
                    None
                };
                let mut pcm_rate = 0u32;
                let mut pcm_bytes = 0u64;
                let mut session =
                    Session::connect(&host, port, &user, &password).map_err(|err| err.to_string())?;
                if cancel.load(Ordering::Acquire) {
                    return Err("скачивание отменено".into());
                }
                let stats = session
                    .download_with(
                        &spec,
                        &mut file,
                        |stats| {
                            got.store(stats.bytes, Ordering::Relaxed);
                            if let Some(time) = stats.position {
                                at.store(wall_epoch(time), Ordering::Relaxed);
                            }
                            !cancel.load(Ordering::Acquire)
                        },
                        |audio| {
                            if let (Some(out), Some(chunk)) = (pcm.as_mut(), decode_xm_audio(audio))
                            {
                                if pcm_rate == 0 {
                                    pcm_rate = chunk.rate;
                                }
                                if chunk.rate == pcm_rate {
                                    for sample in &chunk.samples {
                                        if out.write_all(&sample.to_le_bytes()).is_err() {
                                            break;
                                        }
                                        pcm_bytes += 2;
                                    }
                                }
                            }
                            !cancel.load(Ordering::Acquire)
                        },
                    )
                    .map_err(|err| err.to_string())?;
                drop(file);
                drop(pcm);
                Ok((stats.bytes, stats.fps, if pcm_bytes > 0 { pcm_rate } else { 0 }))
            })();
            let remove_temps = || {
                let _ = std::fs::remove_file(&part_path);
                let _ = std::fs::remove_file(&pcm_path);
                let _ = std::fs::remove_file(&mp4_path);
                let _ = std::fs::remove_file(&raw_path);
            };
            match result {
                Ok((bytes, fps, rate)) if pack => {
                    *status.lock().unwrap() = "Собираю MP4…".into();
                    let audio = (rate > 0).then_some(pcm_path.as_path());
                    match pack_mp4(&part_path, audio, rate, fps, &mp4_path, &cancel)
                    {
                        Ok(()) => {
                            let _ = std::fs::remove_file(&part_path);
                            let _ = std::fs::remove_file(&pcm_path);
                            let size = std::fs::metadata(&mp4_path).map(|item| item.len()).unwrap_or(bytes);
                            let sound = if rate > 0 {
                                "картинка без перекодирования, звук AAC"
                            } else {
                                "картинка без перекодирования, в записи не было звука"
                            };
                            *status.lock().unwrap() =
                                format!("Сохранено {} · {} · {sound}", mp4_path.display(), format_bytes(size));
                        }
                        Err(err) if cancel.load(Ordering::Acquire) || err == "скачивание отменено" => {
                            remove_temps();
                            *status.lock().unwrap() = "Скачивание отменено".into();
                        }
                        Err(err) => {
                            let _ = std::fs::remove_file(&pcm_path);
                            let _ = std::fs::remove_file(&mp4_path);
                            let kept = match sniff_saved(&part_path) {
                                Some(Codec::H265) => {
                                    let hevc = part_path.with_extension("h265");
                                    let _ = std::fs::rename(&part_path, &hevc);
                                    hevc
                                }
                                _ => {
                                    let _ = std::fs::rename(&part_path, &raw_path);
                                    raw_path.clone()
                                }
                            };
                            *status.lock().unwrap() = format!(
                                "Не удалось собрать MP4: {err}. Оставлен поток {}",
                                kept.display()
                            );
                        }
                    }
                }
                Ok((bytes, _, _)) => {
                    *status.lock().unwrap() = format!(
                        "Сохранено {} · {} · поток без контейнера",
                        raw_path.display(),
                        format_bytes(bytes)
                    );
                }
                Err(err) if cancel.load(Ordering::Acquire) || err == "скачивание отменено" => {
                    remove_temps();
                    *status.lock().unwrap() = "Скачивание отменено".into();
                }
                Err(err) => {
                    remove_temps();
                    *status.lock().unwrap() = format!("Не удалось скачать: {err}");
                }
            }
            busy.store(false, Ordering::Release);
        });
    }

    fn cancel_download(&mut self) {
        if !self.download_busy.load(Ordering::Acquire) {
            return;
        }
        if self.download_cancel.swap(true, Ordering::AcqRel) {
            return;
        }
        self.set_status("Отменяю скачивание…");
    }

    fn reveal_downloads(&mut self) {
        let dir = download_dir();
        match open_external(dir.as_os_str()) {
            Ok(child) => drop(child),
            Err(err) => self.set_status(format!("Не удалось открыть папку: {err}")),
        }
    }

    fn open_download(&mut self, recording: Recording) {
        if self.download_busy.load(Ordering::Acquire) {
            self.set_status("Уже скачивается другой файл");
            return;
        }
        match self.device_credentials(self.archive_device) {
            Ok((Vendor::Hikvision, _, _, _, _)) => {
                self.set_status("Архив Hikvision пока не подключён");
                return;
            }
            Err(err) => {
                self.set_status(err);
                return;
            }
            Ok(_) => {}
        }
        self.archive_picker = false;
        self.download_draft = Some(DownloadDraft {
            channel: recording.channel,
            path: recording.path,
            file_begin: recording.begin.clone(),
            file_end: recording.end.clone(),
            limit_begin: self.archive_from.clone(),
            limit_end: self.archive_to.clone(),
            begin: recording.begin,
            end: recording.end,
            bytes: recording.bytes,
        });
    }

    fn play_neighbor(&mut self, recordings: &[Recording], delta: isize) {
        if recordings.is_empty() || delta == 0 {
            return;
        }
        let mut order: Vec<usize> = (0..recordings.len()).collect();
        order.sort_by_key(|index| stamp_epoch(&recordings[*index].begin).unwrap_or(0));
        let current = self.archive_playing.as_ref().map(|clip| clip.path.as_str());
        let position = order
            .iter()
            .position(|index| current == Some(recordings[*index].path.as_str()));
        let next = match position {
            Some(pos) => pos as isize + delta,
            None if delta < 0 => order.len() as isize - 1,
            None => 0,
        };
        if next < 0 || next >= order.len() as isize {
            return;
        }
        self.play(recordings[order[next as usize]].clone());
    }

    fn snapshot_selected(&mut self) {
        let index = self.focus.unwrap_or(self.selected_cell);
        let Some(cell) = self.cells.get(index) else {
            return;
        };
        let picture = cell.slot.picture.lock().unwrap();
        if picture.rgba.is_empty() || picture.width == 0 {
            drop(picture);
            self.set_status("В выбранной ячейке нет кадра");
            return;
        }
        let width = picture.width;
        let height = picture.height;
        let rgba = Arc::clone(&picture.rgba);
        drop(picture);
        let dir = snapshot_dir();
        let name = format!("cell-{}-{:03}.jpg", index + 1, picture_stamp());
        let path = dir.join(name);
        match save_jpg(&path, width, height, &rgba) {
            Ok(()) => self.set_status(format!("Снимок {}", path.display())),
            Err(err) => self.set_status(format!("Не удалось сохранить снимок: {err}")),
        }
    }

    fn refresh_textures(&mut self, ctx: &egui::Context) {
        let visible: Vec<usize> = if self.page == Page::Live {
            if let Some(index) = self.focus {
                vec![index]
            } else {
                self.grid_indices()
            }
        } else {
            Vec::new()
        };
        for (index, cell) in self.cells.iter().enumerate() {
            cell.slot
                .awake
                .store(visible.contains(&index), Ordering::Release);
        }
        self.archive
            .awake
            .store(self.page == Page::Archive, Ordering::Release);
        if self.page == Page::Archive {
            refresh_slot(
                ctx,
                &self.archive,
                &mut self.archive_seen,
                &mut self.archive_texture,
                "archive",
            );
            return;
        }
        if visible.is_empty() {
            return;
        }
        let priority = self.focus.unwrap_or(self.selected_cell);
        let mut budget = 4u32;
        if visible.contains(&priority) {
            let name = format!("cell{priority}");
            let cell = &mut self.cells[priority];
            if refresh_slot(ctx, &cell.slot, &mut cell.seen, &mut cell.texture, &name) {
                budget = budget.saturating_sub(1);
            }
        }
        let count = visible.len();
        let start = self.texture_cursor % count;
        for step in 0..count {
            if budget == 0 {
                break;
            }
            let index = visible[(start + step) % count];
            if index == priority {
                continue;
            }
            let name = format!("cell{index}");
            let cell = &mut self.cells[index];
            if refresh_slot(ctx, &cell.slot, &mut cell.seen, &mut cell.texture, &name) {
                budget = budget.saturating_sub(1);
            }
        }
        self.texture_cursor = self.texture_cursor.wrapping_add(1);
    }
}

impl eframe::App for Viewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_style(ctx, self.accent);
        self.observe_door(ctx);
        let viewport_full = viewport_fullscreen(ctx);
        let live_full = self.page == Page::Live && viewport_full;
        let archive_full = self.page == Page::Archive && viewport_full;
        if viewport_full && !matches!(self.page, Page::Live | Page::Archive) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        } else if matches!(self.page, Page::Live | Page::Archive)
            && ctx.input(|input| input.key_pressed(egui::Key::F11))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!viewport_full));
        } else if (live_full || archive_full)
            && self.door_confirm.is_none()
            && self.rename.is_none()
            && self.device_dialog.is_none()
            && self.download_draft.is_none()
            && self.config_confirm.is_none()
            && !self.archive_picker
            && ctx.input(|input| input.key_pressed(egui::Key::Escape))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        } else if self.call_hold.is_some()
            && self.door_confirm.is_none()
            && self.rename.is_none()
            && self.device_dialog.is_none()
            && self.download_draft.is_none()
            && self.config_confirm.is_none()
            && !self.archive_picker
            && ctx.input(|input| input.key_pressed(egui::Key::Escape))
        {
            self.dismiss_call();
        }
        self.finish_list_click(ctx);
        self.refresh_links();
        self.refresh_textures(ctx);
        let status = self.status_line();
        let devices = self.devices.lock().unwrap().clone();
        let recordings = if self.page == Page::Archive {
            self.recordings.lock().unwrap().clone()
        } else {
            Vec::new()
        };

        if !live_full && !archive_full {
        egui::TopBottomPanel::top("top")
            .exact_height(TOP_BAR)
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);
                    if let Some(logo) = &self.logo {
                        let size = logo.size();
                        let aspect = size[0] as f32 / size[1].max(1) as f32;
                        let height = 34.0;
                        let width = height * aspect;
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            let _ = open_external("https://exunity.uz/?ref=xvms".as_ref());
                        }
                        ui.painter().image(
                            logo.id(),
                            rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add_space(14.0);
                    let mut switch = None;
                    let mut close = None;
                    egui::ScrollArea::horizontal()
                        .id_salt("screen-tabs")
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if tab(ui, ph::HOUSE, "Главная", self.page == Page::Home, false)
                                == Some(TabHit::Open)
                            {
                                switch = Some(Page::Home);
                            }
                            for page in &self.open_tabs {
                                match tab(
                                    ui,
                                    page_icon(*page),
                                    page_label(*page),
                                    *page == self.page,
                                    true,
                                ) {
                                    Some(TabHit::Open) => switch = Some(*page),
                                    Some(TabHit::Close) => close = Some(*page),
                                    None => {}
                                }
                            }
                        });
                    if let Some(page) = close {
                        self.close_tab(page);
                    } else if let Some(page) = switch {
                        self.set_page(page);
                    }
                    });
                });
            });
        }

        if !live_full && !archive_full {
        egui::TopBottomPanel::bottom("bottom")
            .exact_height(match self.page {
                Page::Live | Page::Device => LIVE_BOTTOM_BAR,
                Page::Archive => self.archive_bar_height(),
                _ => 36.0,
            })
            .show_separator_line(false)
            .show(ctx, |ui| {
                if self.page == Page::Archive {
                    self.archive_console(ui, &recordings);
                    return;
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("Observer v0.9.1 [By eXUnity LAB]")
                        .color(MUTED),
                    );
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(&status).color(MUTED));
                    if self.page == Page::Live {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            polish_toolbar(ui);
                            ui.add_space(14.0);
                            if ui
                                .button(ph::ARROWS_OUT)
                                .on_hover_text("На весь экран. Esc — выйти")
                                .clicked()
                            {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                            }
                            ui.add_space(8.0);
                            if ui.button(with_icon(ph::CAMERA, "Снимок")).clicked() {
                                self.snapshot_selected();
                            }
                            ui.add_space(12.0);
                            self.stream_combo(ui);
                            ui.add_space(12.0);
                            self.layout_combo(ui);
                            ui.add_space(12.0);
                            self.sound_button(ui);
                            self.aspect_combo(ui, "aspect-bar");
                        });
                    } else if self.page == Page::Device {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            polish_toolbar(ui);
                            ui.add_space(14.0);
                            if ui.button(with_icon(ph::PLUS, "Добавить")).clicked() {
                                self.open_device_form(None);
                            }
                        });
                    }
                });
            });
        }

        if (self.page == Page::Live && !live_full) || (self.page == Page::Archive && !archive_full) {
            egui::SidePanel::right("devices")
                .exact_width(SIDEBAR)
                .resizable(false)
                .show_separator_line(false)
                .show(ctx, |ui| {
                    ui.add_space(10.0);
                    ui.label(accent_label(ui, with_icon(ph::HARD_DRIVES, "Устройства"), 15.0));
                    ui.add_space(8.0);
                    if devices.is_empty() {
                        ui.label(egui::RichText::new("Регистраторы не добавлены.").color(MUTED));
                        if ui.button(with_icon(ph::PLUS, "Добавить устройство")).clicked() {
                            self.set_page(Page::Device);
                        }
                    } else {
                        let total: u16 = devices.iter().map(device_channel_count).sum();
                        let archive = self.page == Page::Archive;
                        let available = ui.available_height();
                        let tree_height = if archive {
                            (available - 220.0).clamp(96.0, 240.0)
                        } else {
                            (available - 78.0).max(120.0)
                        };
                        egui::ScrollArea::vertical()
                            .id_salt("tree")
                            .max_height(tree_height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for device in &devices {
                                    self.device_branch(ui, device, archive);
                                }
                            });
                        ui.add_space(8.0);
                        if !archive
                            && ui
                                .add_sized(
                                    [ui.available_width(), 30.0],
                                    egui::Button::new(with_icon(ph::SQUARES_FOUR, "Открыть все каналы")),
                                )
                                .clicked()
                        {
                            self.fill_grid();
                        }
                        if !archive {
                            ui.add_space(6.0);
                        }
                        let live: usize = devices.iter().map(live_channel_count).sum();
                        ui.label(
                            egui::RichText::new(format!(
                                "{live} на связи · {total} каналов"
                            ))
                            .color(MUTED),
                        );
                        if archive {
                            ui.add_space(12.0);
                            ui.label(accent_label(ui, with_icon(ph::LIST_BULLETS, "Файлы"), 15.0));
                            ui.add_space(6.0);
                            self.archive_files(ui, &recordings);
                        }
                    }
                });
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(CANVAS)
                    .inner_margin(egui::Margin::same(if live_full || archive_full { 0 } else { 8 })),
            )
            .show(ctx, |ui| {
            match self.page {
                Page::Home => self.page_home(ui),
                Page::Live => self.page_live(ui, live_full),
                Page::Device => self.page_device(ui, &devices),
                Page::Archive => self.page_archive(ui, &devices, &recordings, archive_full),
                Page::Journal => self.page_journal(ui),
                Page::Settings => self.page_settings(ui),
                Page::Config => self.page_config(ui, &devices),
                Page::Later(title, text) => self.page_later(ui, title, text),
            }
        });
        if (self.page == Page::Live && !live_full) || (self.page == Page::Archive && !archive_full) {
            let bottom = if self.page == Page::Archive {
                self.archive_bar_height()
            } else {
                LIVE_BOTTOM_BAR
            };
            paint_sidebar_joints(ctx, bottom);
        }

        let delay = if self.download_busy.load(Ordering::Acquire)
            || matches!(self.page, Page::Live | Page::Archive | Page::Journal)
            || self.journal_busy.load(Ordering::Acquire)
            || self.config_busy.load(Ordering::Acquire)
        {
            33
        } else if self.page == Page::Config && self.config_clock.is_some() && !self.config_time_editing {
            200
        } else if self.door_call || self.call_hold.is_some() {
            400
        } else {
            250
        };
        self.rename_window(ctx);
        self.call_bar(ctx);
        self.door_window(ctx);
        self.device_window(ctx);
        self.config_window(ctx);
        self.archive_picker_window(ctx);
        self.download_window(ctx);
        if self.page != Page::Live || self.focus.is_none() || !self.ptz_open {
            self.end_ptz();
        }
        ctx.request_repaint_after(Duration::from_millis(delay));
        self.persist();
    }
}

impl Viewer {
    fn open_rename(&mut self, device_id: u64, channel: u16, text: String) {
        self.rename = Some(RenameDraft {
            device_id,
            channel,
            text,
            focus: true,
        });
    }

    fn rename_window(&mut self, ctx: &egui::Context) {
        if self.rename.is_none() {
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let busy = self.rename_busy.load(Ordering::SeqCst);
        egui::Window::new("Имя канала")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                let Some(draft) = self.rename.as_mut() else {
                    return;
                };
                ui.label("Новое имя запишется на регистратор.");
                ui.add_space(8.0);
                if draft.text.chars().count() > 32 {
                    draft.text = draft.text.chars().take(32).collect();
                }
                let edit = text_field(ui, &mut draft.text, false, 320.0, "до 32 символов");
                if draft.focus {
                    edit.request_focus();
                    draft.focus = false;
                }
                if edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    save = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let button = egui::Button::new(if busy {
                        with_icon(ph::FLOPPY_DISK, "Запись…")
                    } else {
                        with_icon(ph::CHECK, "Сохранить")
                    });
                    if ui.add_sized([150.0, 30.0], button).clicked() && !busy {
                        save = true;
                    }
                    if ui.button(with_icon(ph::X, "Отмена")).clicked() {
                        cancel = true;
                    }
                });
            });
        if !open || cancel {
            self.rename = None;
            return;
        }
        if save {
            if let Some(draft) = self.rename.take() {
                self.rename_channel(draft.device_id, draft.channel, draft.text);
            }
        }
    }

    fn rename_channel(&mut self, device_id: u64, channel: u16, title: String) {
        let title = title.trim().to_string();
        if title.is_empty() {
            self.set_status("Введите имя канала");
            return;
        }
        if title.chars().count() > 32 {
            self.set_status("Имя длиннее 32 символов");
            return;
        }
        if self.rename_busy.swap(true, Ordering::SeqCst) {
            self.set_status("Предыдущее имя ещё записывается");
            return;
        }
        let found = self.devices.lock().unwrap().iter().find_map(|device| {
            (device.id == device_id).then(|| {
                (
                    device.kind,
                    device.host.clone(),
                    device.port,
                    device.user.clone(),
                    device.password.clone(),
                )
            })
        });
        let Some((kind, host, port, user, password)) = found else {
            self.rename_busy.store(false, Ordering::SeqCst);
            self.set_status("Устройство не найдено");
            return;
        };
        if kind == Vendor::Hikvision {
            self.rename_busy.store(false, Ordering::SeqCst);
            self.set_status("Имя канала Hikvision задаётся на самом устройстве");
            return;
        }
        let devices = Arc::clone(&self.devices);
        let status = Arc::clone(&self.status);
        let busy = Arc::clone(&self.rename_busy);
        *status.lock().unwrap() = "Запись имени на регистратор…".into();
        thread::spawn(move || {
            let written = (|| {
                let mut session = Session::connect(&host, port, &user, &password)?;
                session.set_channel_title(channel, &title)
            })();
            match written {
                Ok(titles) => {
                    if let Some(device) = devices
                        .lock()
                        .unwrap()
                        .iter_mut()
                        .find(|device| device.id == device_id)
                    {
                        device.titles = titles;
                    }
                    *status.lock().unwrap() = "Имя канала записано на регистратор".into();
                }
                Err(err) => {
                    *status.lock().unwrap() = format!("Имя не записалось: {err}");
                }
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    fn page_home(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(22))
            .show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut chosen = None;
            for (index, &(title, items)) in home_groups().iter().enumerate() {
                if index > 0 {
                    ui.add_space(18.0);
                }
                ui.label(egui::RichText::new(title).color(MUTED).size(13.0));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                    for &(label, page) in items {
                        if home_tile(ui, page_icon(page), label).clicked() {
                            chosen = Some(page);
                        }
                    }
                });
            }
            if let Some(page) = chosen {
                self.set_page(page);
            }
        });
            });
    }

    fn page_live(&mut self, ui: &mut egui::Ui, fullscreen: bool) {
        let rect = if fullscreen {
            ui.max_rect()
        } else {
            ui.max_rect().shrink(8.0)
        };
        let mut drag_index = None;
        if let Some(index) = self.focus {
            let _ = self.paint_cell(ui, rect, index, fullscreen);
            if self.focus.is_some() && !fullscreen {
                self.paint_focus_tools(ui, rect);
            }
        } else {
            let (cols, rows) = grid_shape(self.layout);
            let gap = if fullscreen { 0.0 } else { 6.0 };
            let cell_w = (rect.width() - gap * (cols as f32 - 1.0)) / cols as f32;
            let cell_h = (rect.height() - gap * (rows as f32 - 1.0)) / rows as f32;
            for row in 0..rows {
                for col in 0..cols {
                    let index = row * cols + col;
                    let origin = egui::pos2(
                        rect.left() + col as f32 * (cell_w + gap),
                        rect.top() + row as f32 * (cell_h + gap),
                    );
                    let cell_rect = egui::Rect::from_min_size(origin, egui::vec2(cell_w, cell_h));
                    if self.paint_cell(ui, cell_rect, index, fullscreen) {
                        drag_index = Some(index);
                    }
                }
            }
        }
        self.paint_drag_ghost(ui, drag_index);
    }

    fn paint_drag_ghost(&self, ui: &egui::Ui, index: Option<usize>) {
        let Some(index) = index else {
            return;
        };
        let Some(pos) = ui.input(|input| input.pointer.latest_pos()) else {
            return;
        };
        let title = self
            .cells
            .get(index)
            .and_then(|cell| cell.source)
            .map(|source| self.source_label(source))
            .unwrap_or_else(|| "Пусто".into());
        let galley = ui.painter().layout_no_wrap(
            title,
            egui::FontId::proportional(13.0),
            TEXT,
        );
        let origin = pos + egui::vec2(16.0, 18.0);
        let rect = egui::Rect::from_min_size(origin, galley.size() + egui::vec2(16.0, 10.0));
        ui.painter()
            .rect_filled(rect, 8.0, PANEL);
        ui.painter().rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(1.0, ui_accent(ui)),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .galley(origin + egui::vec2(8.0, 5.0), galley, TEXT);
    }

    fn paint_cell(&mut self, ui: &mut egui::Ui, rect: egui::Rect, index: usize, bare: bool) -> bool {
        let response = ui.interact(
            rect,
            egui::Id::new(("cell", index)),
            egui::Sense::click_and_drag(),
        );
        if response.double_clicked() {
            self.selected_cell = index;
            self.focus = if self.focus == Some(index) {
                None
            } else {
                Some(index)
            };
        } else if response.clicked() {
            self.selected_cell = index;
        }
        let can_drag = self.focus.is_none();
        if can_drag && response.dragged() {
            response.dnd_set_drag_payload(index);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if can_drag && !bare && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if can_drag {
            if let Some(from) = response.dnd_release_payload::<usize>() {
                self.swap_cells(*from, index);
            }
        }
        if let Some(source) = self.cells.get(index).and_then(|cell| cell.source) {
            if response.secondary_clicked() {
                self.selected_cell = index;
            }
            response.context_menu(|ui| {
                tune_menu(ui);
                if ui.button(with_icon(ph::PENCIL_SIMPLE, "Переименовать")).clicked() {
                    let text = {
                        let devices = self.devices.lock().unwrap();
                        let device = devices.iter().find(|item| item.id == source.device_id);
                        channel_title(device, source.channel)
                    };
                    self.open_rename(source.device_id, source.channel, text);
                    ui.close_menu();
                }
            });
        }
        let drop_target = can_drag
            && response
                .dnd_hover_payload::<usize>()
                .is_some_and(|from| *from != index);
        let radius = if bare { 0.0 } else { 8.0 };
        ui.painter().rect_filled(rect, radius, VIDEO);
        let video = if bare {
            rect
        } else {
            let header = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 28.0));
            let header_fill = if self.focus.is_none() && self.selected_cell == index {
                accent_soft(ui_accent(ui))
            } else {
                HEADER
            };
            ui.painter().rect_filled(
                header,
                egui::CornerRadius {
                    nw: 8,
                    ne: 8,
                    sw: 0,
                    se: 0,
                },
                header_fill,
            );
            let title = self
                .cells
                .get(index)
                .and_then(|cell| cell.source)
                .map(|source| self.source_label(source))
                .unwrap_or_else(|| "Пусто".into());
            let listening = self.audio_cell == Some(index);
            let title_color = if self.cells.get(index).and_then(|cell| cell.source).is_some() {
                TEXT
            } else {
                MUTED
            };
            let right_pad = if self.focus == Some(index) {
                if self.cell_door(index) { 464.0 } else { 324.0 }
            } else if listening {
                28.0
            } else {
                8.0
            };
            let title_rect = egui::Rect::from_min_max(
                header.left_top(),
                egui::pos2((header.right() - right_pad).max(header.left()), header.bottom()),
            );
            ui.painter().with_clip_rect(title_rect).text(
                header.left_center() + egui::vec2(10.0, 0.0),
                egui::Align2::LEFT_CENTER,
                title,
                egui::FontId::proportional(13.0),
                title_color,
            );
            if listening && self.focus != Some(index) {
                ui.painter().text(
                    egui::pos2(header.right() - 16.0, header.center().y),
                    egui::Align2::CENTER_CENTER,
                    ph::SPEAKER_HIGH,
                    egui::FontId::proportional(15.0),
                    ui_accent(ui),
                );
            }
            egui::Rect::from_min_max(egui::pos2(rect.left(), header.bottom()), rect.max)
        };
        if let Some(texture) = self.cells.get(index).and_then(|cell| cell.texture.as_ref()) {
            let fitted = place_video(
                video,
                texture.size()[0] as f32,
                texture.size()[1] as f32,
                if bare { Aspect::Auto } else { self.aspect },
            );
            let picture_radius = if bare {
                egui::CornerRadius::ZERO
            } else {
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 8,
                    se: 8,
                }
            };
            paint_picture(ui, fitted.intersect(video), picture_radius, texture);
        } else if self.cells.get(index).and_then(|cell| cell.source).is_none() {
            ui.painter().text(
                video.center(),
                egui::Align2::CENTER_CENTER,
                "Выберите канал справа",
                egui::FontId::proportional(13.0),
                MUTED,
            );
        }
        if response.dragged() {
            ui.painter()
                .rect_filled(rect, radius, egui::Color32::from_black_alpha(110));
        }
        if drop_target && !bare {
            ui.painter().rect_stroke(
                rect,
                8.0,
                egui::Stroke::new(1.5, ui_accent(ui)),
                egui::StrokeKind::Inside,
            );
        }
        response.dragged()
    }

    fn paint_focus_tools(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let index = self.focus.unwrap_or(self.selected_cell);
        let door = self.cell_door(index);
        let kind = self.cell_vendor(index);
        let span = if door { 456.0 } else { 316.0 };
        let tools = egui::Rect::from_min_max(
            egui::pos2(rect.right() - span, rect.top()),
            egui::pos2(rect.right() - 8.0, rect.top() + 28.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(tools), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let radius = egui::CornerRadius::same(4);
                let style = ui.style_mut();
                style.spacing.button_padding = egui::vec2(10.0, 3.0);
                style.spacing.interact_size.y = 24.0;
                style.spacing.item_spacing.x = 8.0;
                for widgets in [
                    &mut style.visuals.widgets.noninteractive,
                    &mut style.visuals.widgets.inactive,
                    &mut style.visuals.widgets.hovered,
                    &mut style.visuals.widgets.active,
                    &mut style.visuals.widgets.open,
                ] {
                    widgets.corner_radius = radius;
                }
                self.sound_button(ui);
                self.aspect_combo(ui, "aspect-focus");
                if kind == Vendor::Xm {
                    self.ptz_toggle(ui);
                }
                if door {
                    self.door_button(ui);
                }
            });
        });
        if kind == Vendor::Xm {
            self.paint_ptz(ui, rect);
        } else if self.ptz_open {
            self.ptz_open = false;
            self.end_ptz();
        }
    }

    fn cell_vendor(&self, index: usize) -> Vendor {
        let Some(source) = self.cells.get(index).and_then(|cell| cell.source) else {
            return Vendor::Xm;
        };
        self.devices
            .lock()
            .unwrap()
            .iter()
            .find(|device| device.id == source.device_id)
            .map(|device| device.kind)
            .unwrap_or(Vendor::Xm)
    }

    fn cell_door(&self, index: usize) -> bool {
        let Some(source) = self.cells.get(index).and_then(|cell| cell.source) else {
            return false;
        };
        self.devices
            .lock()
            .unwrap()
            .iter()
            .any(|device| device.id == source.device_id && device.door)
    }

    fn door_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .button(with_icon(ph::LOCK_KEY_OPEN, "Открыть замок"))
            .on_hover_text("Открыть замок домофона")
            .clicked()
        {
            let index = self.focus.unwrap_or(self.selected_cell);
            if let Some(source) = self.cells.get(index).and_then(|cell| cell.source) {
                self.ask_unlock(source.device_id);
            }
        }
    }

    fn ask_unlock(&mut self, id: u64) {
        self.door_confirm = Some(id);
        self.door_slide = 0.0;
        self.door_drag = false;
        self.door_moved = false;
    }

    fn camera_choices(&self) -> Vec<(Source, String)> {
        let devices = self.devices.lock().unwrap();
        let many = devices.len() > 1;
        let mut choices = Vec::new();
        for device in devices.iter() {
            for channel in 0..device_channel_count(device) {
                let title = channel_title(Some(device), channel);
                let label = if many {
                    format!("{} · {title}", device.display_name())
                } else {
                    title
                };
                choices.push((
                    Source {
                        device_id: device.id,
                        channel,
                    },
                    label,
                ));
            }
        }
        choices
    }

    fn observe_door(&mut self, ctx: &egui::Context) {
        self.ensure_door_watch();
        if !self.door_call {
            if self.call_hold.is_some() {
                self.dismiss_call();
            }
            self.call_dismissed = false;
            return;
        }
        let report = self.door_report.lock().unwrap().clone();
        if report.phase == DoorPhase::Idle {
            if self.call_hold.is_some() {
                self.call_ended = true;
            } else {
                self.call_dismissed = false;
            }
            return;
        }
        self.call_ended = false;
        self.call_phase = report.phase;
        self.call_door_name = report.name;
        if self.call_dismissed {
            return;
        }
        if self.call_hold.is_none() {
            self.present_call(ctx, report.device_id);
        } else if let Some(hold) = self.call_hold.as_mut() {
            hold.door_id = report.device_id;
        }
    }

    fn ensure_door_watch(&mut self) {
        if !self.door_call {
            if self.door_watch_alive.load(Ordering::Acquire) {
                self.door_watch_gen.fetch_add(1, Ordering::AcqRel);
            }
            return;
        }
        if self.door_watch_alive.load(Ordering::Acquire) {
            return;
        }
        self.door_watch_alive.store(true, Ordering::Release);
        let gen = self.door_watch_gen.load(Ordering::Acquire);
        let generation = Arc::clone(&self.door_watch_gen);
        let alive = Arc::clone(&self.door_watch_alive);
        let devices = Arc::clone(&self.devices);
        let report = Arc::clone(&self.door_report);
        thread::spawn(move || {
            watch_doors(gen, generation, devices, report);
            alive.store(false, Ordering::Release);
        });
    }

    fn present_call(&mut self, ctx: &egui::Context, door_id: u64) {
        let camera = self.door_camera.filter(|source| {
            self.devices
                .lock()
                .unwrap()
                .iter()
                .any(|device| device.id == source.device_id)
        });
        let page = self.page;
        let focus = self.focus;
        let selected = self.selected_cell;
        let (index, previous, replaced) = if let Some(camera) = camera {
            self.set_page(Page::Live);
            let index = self.find_shown(camera).unwrap_or_else(|| {
                self.selected_cell
                    .min((self.layout as usize).saturating_sub(1))
            });
            let previous = self.cells.get(index).and_then(|cell| cell.source);
            let replaced = previous != Some(camera);
            if replaced {
                self.open_channel(index, camera);
            }
            self.focus = Some(index);
            self.selected_cell = index;
            (index, previous, replaced)
        } else {
            (selected, None, false)
        };
        self.call_hold = Some(CallHold {
            page,
            focus,
            selected,
            index,
            previous,
            replaced,
            door_id,
        });
        self.call_ended = false;
        self.set_status("Звонок в домофон");
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn dismiss_call(&mut self) {
        self.call_dismissed = true;
        let Some(hold) = self.call_hold.take() else {
            return;
        };
        self.call_ended = false;
        if hold.replaced {
            if let Some(cell) = self.cells.get_mut(hold.index) {
                cell.source = hold.previous;
                cell.seen = 0;
                cell.texture = None;
                if hold.previous.is_none() {
                    cell.slot.stop();
                }
            }
            if let Some(source) = hold.previous {
                if self.page == Page::Live || hold.page == Page::Live {
                    self.start_cell(hold.index, source, self.stream_kind());
                }
            }
        }
        self.focus = hold.focus;
        self.selected_cell = hold.selected;
        if self.page != hold.page {
            self.set_page(hold.page);
        }
    }

    fn call_bar(&mut self, ctx: &egui::Context) {
        let Some(door_id) = self.call_hold.as_ref().map(|hold| hold.door_id) else {
            return;
        };
        let ended = self.call_ended;
        let phase = self.call_phase;
        let name = if self.call_door_name.is_empty() {
            "Домофон".to_string()
        } else {
            self.call_door_name.clone()
        };
        let title = if ended {
            "Вызов завершён"
        } else if phase == DoorPhase::Talk {
            "Разговор"
        } else {
            "Звонок в домофон"
        };
        let screen = ctx.screen_rect();
        let width = (screen.width() - 48.0).clamp(320.0, 560.0);
        let pos = egui::pos2(
            screen.center().x - width / 2.0,
            (screen.bottom() - 128.0).max(screen.top() + 16.0),
        );
        let mut close = false;
        let mut unlock = false;
        egui::Area::new(egui::Id::new("door-call"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                let accent = ui_accent(ui);
                egui::Frame::new()
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(1.0, accent))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_min_width(width - 26.0);
                        ui.horizontal(|ui| {
                            ui.label(accent_label(ui, with_icon(ph::BELL, title), 15.0));
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new(name).color(MUTED));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Закрыть").clicked() {
                                    close = true;
                                }
                                if ui
                                    .button(with_icon(ph::LOCK_KEY_OPEN, "Открыть замок"))
                                    .clicked()
                                {
                                    unlock = true;
                                }
                            });
                        });
                    });
            });
        if unlock {
            self.ask_unlock(door_id);
        }
        if close {
            self.dismiss_call();
        }
    }

    fn door_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.door_confirm else {
            return;
        };
        let (name, host) = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .find(|device| device.id == id)
            .map(|device| (device.display_name(), device.host.clone()))
            .unwrap_or_else(|| ("Домофон".into(), String::new()));
        let where_text = if host.is_empty() || name == host {
            name
        } else {
            format!("{name} · {host}")
        };
        let screen = ctx.screen_rect();
        let mut confirm = false;
        let mut cancel = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        egui::Area::new(egui::Id::new("door-modal"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(150));
                if response.clicked() {
                    cancel = true;
                }
                let slider =
                    egui::Rect::from_center_size(screen.center(), egui::vec2(468.0, 110.0));
                ui.scope_builder(egui::UiBuilder::new().max_rect(slider), |ui| {
                    let (caption, _) =
                        ui.allocate_exact_size(egui::vec2(468.0, 22.0), egui::Sense::click());
                    ui.painter().text(
                        caption.center(),
                        egui::Align2::CENTER_CENTER,
                        &where_text,
                        egui::FontId::proportional(13.0),
                        MUTED,
                    );
                    ui.add_space(10.0);
                    match self.door_slider(ui) {
                        DoorGesture::Open => confirm = true,
                        DoorGesture::Close => cancel = true,
                        DoorGesture::None => {}
                    }
                });
            });
        if cancel || confirm {
            self.door_slide = 0.0;
            self.door_drag = false;
            self.door_moved = false;
        }
        if cancel {
            self.door_confirm = None;
            return;
        }
        if confirm {
            self.door_confirm = None;
            self.unlock_door(id);
        }
    }

    fn door_slider(&mut self, ui: &mut egui::Ui) -> DoorGesture {
        let (track, _) =
            ui.allocate_exact_size(egui::vec2(468.0, 78.0), egui::Sense::click_and_drag());
        let knob_r = 30.0;
        let lock_r = 26.0;
        let left_x = track.left() + 8.0 + knob_r;
        let right_x = track.right() - 8.0 - lock_r;
        let mid_y = track.center().y;
        let travel = (right_x - left_x).max(1.0);
        let pointer = ui.input(|input| input.pointer.interact_pos());
        let pressed = ui.input(|input| input.pointer.primary_pressed());
        let released = ui.input(|input| input.pointer.primary_released());
        let down = ui.input(|input| input.pointer.primary_down());
        let origin = ui.input(|input| input.pointer.press_origin());
        let knob_x = left_x + self.door_slide * travel;
        if pressed {
            if let Some(origin) = origin {
                if origin.distance(egui::pos2(knob_x, mid_y)) <= knob_r + 4.0 {
                    self.door_drag = true;
                    self.door_moved = false;
                }
            }
        }
        if self.door_drag && down {
            if let (Some(pos), Some(origin)) = (pointer, origin) {
                if (pos.x - origin.x).abs() > 6.0 || (pos.y - origin.y).abs() > 6.0 {
                    self.door_moved = true;
                }
                if self.door_moved {
                    self.door_slide = ((pos.x - left_x) / travel).clamp(0.0, 1.0);
                }
            }
        }
        let mut gesture = DoorGesture::None;
        if released && self.door_drag {
            let reached = (1.0 - self.door_slide) * travel <= lock_r;
            self.door_drag = false;
            if !self.door_moved {
                gesture = DoorGesture::Close;
            } else if reached {
                gesture = DoorGesture::Open;
            }
        }
        if !self.door_drag && !matches!(gesture, DoorGesture::Open | DoorGesture::Close) {
            if self.door_slide > 0.0 {
                let dt = ui.input(|input| input.stable_dt).clamp(0.0, 0.05);
                self.door_slide *= (-16.0 * dt).exp();
                if self.door_slide < 0.008 {
                    self.door_slide = 0.0;
                }
            }
        }
        if self.door_drag || self.door_slide > 0.0 {
            ui.ctx().request_repaint();
        }
        let knob_x = left_x + self.door_slide * travel;
        let knob = egui::pos2(knob_x, mid_y);
        let lock = egui::pos2(right_x, mid_y);
        let painter = ui.painter();
        painter.rect_filled(track, track.height() * 0.5, egui::Color32::from_rgb(30, 34, 42));
        painter.rect_stroke(
            track,
            track.height() * 0.5,
            egui::Stroke::new(1.0, egui::Color32::from_rgb(62, 68, 78)),
            egui::StrokeKind::Inside,
        );
        painter.circle_filled(lock, lock_r, egui::Color32::from_rgb(48, 54, 64));
        painter.text(
            lock,
            egui::Align2::CENTER_CENTER,
            ph::LOCK_OPEN,
            egui::FontId::proportional(20.0),
            egui::Color32::from_rgb(132, 140, 152),
        );
        let text_center = egui::pos2((left_x + knob_r + right_x - lock_r) * 0.5, mid_y);
        paint_tracked(
            painter,
            text_center + egui::vec2(-18.0, 0.0),
            "ОТКРЫТЬ",
            15.0,
            egui::Color32::from_rgb(168, 176, 188),
            1.6,
        );
        for index in 0..3 {
            paint_slide_mark(
                painter,
                text_center + egui::vec2(46.0 + index as f32 * 11.0, 0.0),
                egui::Color32::from_rgb(150, 158, 170),
            );
        }
        let color = mix_rgb(
            egui::Color32::from_rgb(214, 48, 52),
            egui::Color32::from_rgb(232, 186, 42),
            self.door_slide,
        );
        painter.circle_filled(knob + egui::vec2(0.0, 2.0), knob_r, egui::Color32::from_black_alpha(70));
        painter.circle_filled(knob, knob_r, color);
        painter.text(
            knob,
            egui::Align2::CENTER_CENTER,
            ph::X,
            egui::FontId::proportional(22.0),
            mix_rgb(egui::Color32::WHITE, egui::Color32::from_rgb(42, 28, 8), self.door_slide),
        );
        if pointer.is_some_and(|pos| pos.distance(knob) <= knob_r) {
            ui.ctx().set_cursor_icon(if self.door_drag {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }
        gesture
    }

    fn unlock_door(&self, id: u64) {
        let found = self.devices.lock().unwrap().iter().find_map(|device| {
            (device.id == id && device.kind == Vendor::Hikvision).then(|| {
                (device.host.clone(), device.port, device.user.clone(), device.password.clone())
            })
        });
        let Some((host, port, user, password)) = found else {
            self.set_status("Это не домофон Hikvision");
            return;
        };
        self.set_status("Открываю замок…");
        let status = Arc::clone(&self.status);
        thread::spawn(move || {
            let message = match xm_nvr::hik_open_door(&host, port, &user, &password) {
                Ok(()) => "Замок открыт".to_string(),
                Err(err) => format!("Замок не открылся: {err}"),
            };
            *status.lock().unwrap() = message;
        });
    }

    fn layout_combo(&mut self, ui: &mut egui::Ui) {
        let mut layout = self.layout;
        egui::ComboBox::from_id_salt("layout-bar")
            .selected_text(format!("{}  {layout}", layout_icon(layout)))
            .width(96.0)
            .show_ui(ui, |ui| {
                for size in [1, 4, 9, 16, 25, 36, 64] {
                    ui.selectable_value(
                        &mut layout,
                        size,
                        format!("{}  {size}", layout_icon(size)),
                    );
                }
            })
            .response
            .on_hover_text("Размер сетки");
        if layout != self.layout {
            self.set_layout(layout);
        }
    }

    fn stream_combo(&mut self, ui: &mut egui::Ui) {
        let mut stream = self.stream;
        let (icon, name) = match stream {
            StreamKind::Main => (ph::MONITOR, "Основной"),
            StreamKind::Extra => (ph::PICTURE_IN_PICTURE, "Доп."),
        };
        egui::ComboBox::from_id_salt("stream-bar")
            .selected_text(with_icon(icon, name))
            .width(132.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut stream,
                    StreamKind::Extra,
                    with_icon(ph::PICTURE_IN_PICTURE, "Дополнительный"),
                );
                ui.selectable_value(
                    &mut stream,
                    StreamKind::Main,
                    with_icon(ph::MONITOR, "Основной"),
                );
            })
            .response
            .on_hover_text("Поток. Действует до закрытия программы.");
        if stream != self.stream {
            self.set_stream(stream);
        }
    }

    fn aspect_combo(&mut self, ui: &mut egui::Ui, salt: &str) {
        egui::ComboBox::from_id_salt(salt)
            .selected_text(aspect_name(self.aspect))
            .width(if salt == "aspect-focus" { 108.0 } else { 124.0 })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.aspect,
                    Aspect::Auto,
                    with_icon(ph::FRAME_CORNERS, "Авто"),
                );
                ui.selectable_value(
                    &mut self.aspect,
                    Aspect::Original,
                    with_icon(ph::ARROWS_OUT, "Оригинал"),
                );
                ui.selectable_value(&mut self.aspect, Aspect::R4x3, "4:3");
                ui.selectable_value(&mut self.aspect, Aspect::R16x9, "16:9");
            });
    }

    fn sound_button(&mut self, ui: &mut egui::Ui) {
        let index = self.focus.unwrap_or(self.selected_cell);
        let on = self.audio_cell == Some(index);
        let label = if on {
            with_icon(ph::SPEAKER_HIGH, "Звук")
        } else {
            with_icon(ph::SPEAKER_SLASH, "Звук")
        };
        if ui
            .selectable_label(on, label)
            .on_hover_text("Слушать микрофон выбранной камеры")
            .clicked()
        {
            if on {
                self.set_audio(None);
            } else {
                self.set_audio(Some(index));
            }
        }
    }

    fn set_audio(&mut self, index: Option<usize>) {
        for cell in &self.cells {
            *cell.slot.audio_out.lock().unwrap() = None;
        }
        self.audio_cell = None;
        let Some(index) = index else {
            self.set_status("Звук выключен");
            return;
        };
        if self.cells.get(index).and_then(|cell| cell.source).is_none() {
            self.set_status("Сначала выберите камеру");
            return;
        }
        let (tx, rx) = mpsc::sync_channel(24);
        let status = Arc::clone(&self.status);
        thread::spawn(move || play_audio(rx, status));
        *self.cells[index].slot.audio_out.lock().unwrap() = Some(tx);
        self.audio_cell = Some(index);
        self.set_status("Звук камеры");
    }

    fn ptz_toggle(&mut self, ui: &mut egui::Ui) {
        let response = ui
            .selectable_label(self.ptz_open, ph::CROSSHAIR)
            .on_hover_text("Управление камерой");
        if response.clicked() {
            self.ptz_open = !self.ptz_open;
            if !self.ptz_open {
                self.end_ptz();
            }
        }
    }

    fn paint_ptz(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !self.ptz_open {
            return;
        }
        let pad = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 12.0, rect.bottom() - 168.0),
            egui::vec2(176.0, 156.0),
        );
        let response = ui.interact(pad, egui::Id::new("ptz-panel"), egui::Sense::click_and_drag());
        let painter = ui.painter();
        painter.rect_filled(pad, 14.0, egui::Color32::from_rgba_unmultiplied(14, 18, 26, 230));

        let center = egui::pos2(pad.left() + 58.0, pad.top() + 56.0);
        let outer = 46.0;
        let inner = 17.0;
        painter.circle_filled(center, outer + 4.0, egui::Color32::from_rgb(20, 24, 32));
        let sectors = [
            (-std::f32::consts::FRAC_PI_2, "DirectionUp", egui::vec2(0.0, -1.0)),
            (0.0, "DirectionRight", egui::vec2(1.0, 0.0)),
            (std::f32::consts::FRAC_PI_2, "DirectionDown", egui::vec2(0.0, 1.0)),
            (std::f32::consts::PI, "DirectionLeft", egui::vec2(-1.0, 0.0)),
        ];
        let pointer = ui.input(|input| input.pointer.interact_pos());
        let hover = pointer.and_then(|pos| ptz_hit(center, inner, outer, zoom_column(pad), speed_track(pad), pos));
        for (mid, command, direction) in sectors {
            let active = self.ptz_hold == Some(command);
            let hot = matches!(hover, Some(PtzHit::Move(cmd)) if cmd == command);
            let fill = if active {
                egui::Color32::from_rgb(28, 78, 86)
            } else if hot {
                egui::Color32::from_rgb(58, 66, 80)
            } else {
                egui::Color32::from_rgb(44, 50, 62)
            };
            painter.add(egui::epaint::PathShape::convex_polygon(
                ring_sector(center, inner + 3.0, outer, mid, 1.18),
                fill,
                egui::Stroke::NONE,
            ));
            paint_chevron(
                painter,
                center + direction * ((inner + outer) * 0.5),
                direction,
                5.0,
                egui::Color32::WHITE,
            );
        }
        painter.circle_filled(center, 18.0, egui::Color32::from_rgba_unmultiplied(78, 196, 176, 48));
        painter.circle_filled(center, 15.0, egui::Color32::from_rgb(16, 24, 30));
        painter.circle_filled(center, 2.0, egui::Color32::WHITE);
        for direction in [egui::vec2(1.0, 0.0), egui::vec2(-1.0, 0.0), egui::vec2(0.0, 1.0), egui::vec2(0.0, -1.0)] {
            painter.line_segment(
                [center + direction * 4.0, center + direction * 7.5],
                egui::Stroke::new(1.3, egui::Color32::WHITE),
            );
        }

        let zoom = zoom_column(pad);
        paint_zoom_key(painter, zoom_key(zoom, true), matches!(hover, Some(PtzHit::Move("ZoomTile"))), self.ptz_hold == Some("ZoomTile"), true);
        paint_zoom_key(painter, zoom_key(zoom, false), matches!(hover, Some(PtzHit::Move("ZoomWide"))), self.ptz_hold == Some("ZoomWide"), false);

        let track = speed_track(pad);
        painter.text(
            egui::pos2(pad.left() + 12.0, track.center().y),
            egui::Align2::LEFT_CENTER,
            "Скорость",
            egui::FontId::proportional(12.0),
            MUTED,
        );
        let bar = egui::Rect::from_min_max(
            egui::pos2(pad.left() + 78.0, track.center().y - 2.5),
            egui::pos2(pad.right() - 36.0, track.center().y + 2.5),
        );
        painter.rect_filled(bar, 2.5, egui::Color32::from_rgb(32, 38, 48));
        let t = (self.ptz_step.clamp(1, 8) - 1) as f32 / 7.0;
        let knob_x = bar.left() + t * bar.width();
        if knob_x > bar.left() {
            painter.rect_filled(
                egui::Rect::from_min_max(bar.min, egui::pos2(knob_x, bar.bottom())),
                2.5,
                ui_accent(ui),
            );
        }
        painter.circle_filled(egui::pos2(knob_x, bar.center().y), 6.0, egui::Color32::WHITE);
        let pill = egui::Rect::from_center_size(
            egui::pos2(pad.right() - 18.0, track.center().y),
            egui::vec2(24.0, 20.0),
        );
        painter.rect_filled(pill, 6.0, egui::Color32::from_rgb(36, 42, 54));
        painter.text(
            pill.center(),
            egui::Align2::CENTER_CENTER,
            self.ptz_step.to_string(),
            egui::FontId::proportional(12.0),
            TEXT,
        );

        self.drive_ptz(&response, center, inner, outer, zoom, track.expand2(egui::vec2(0.0, 8.0)), bar);
    }

    fn drive_ptz(
        &mut self,
        response: &egui::Response,
        center: egui::Pos2,
        inner: f32,
        outer: f32,
        zoom: egui::Rect,
        speed_hit: egui::Rect,
        bar: egui::Rect,
    ) {
        let pointer = response.ctx.input(|input| input.pointer.interact_pos());
        let pressed = response.ctx.input(|input| input.pointer.primary_pressed());
        let released = response.ctx.input(|input| input.pointer.primary_released());
        let down = response.ctx.input(|input| input.pointer.primary_down());
        let origin = response.ctx.input(|input| input.pointer.press_origin());
        if released {
            self.end_ptz();
        }
        let Some(pos) = pointer else {
            return;
        };
        if origin.is_some_and(|start| speed_hit.contains(start)) && (response.dragged() || pressed) {
            let t = ((pos.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
            let step = (1.0 + t * 7.0).round().clamp(1.0, 8.0) as u8;
            if step != self.ptz_step {
                self.ptz_step = step;
                if let Some(command) = self.ptz_hold {
                    self.ptz_hold = None;
                    self.begin_ptz(command);
                }
            }
            return;
        }
        let hit = ptz_hit(center, inner, outer, zoom, speed_hit, pos);
        if pressed {
            match hit {
                Some(PtzHit::Move(command)) => self.begin_ptz(command),
                Some(PtzHit::Center) => {
                    if self.ptz_hold.is_some() {
                        self.end_ptz();
                    } else if let Some(command) = self.ptz_cmd {
                        self.enqueue_ptz(command, true);
                    }
                }
                _ => {}
            }
        } else if down {
            if let Some(PtzHit::Move(command)) = hit {
                if self.ptz_hold.is_some() && self.ptz_hold != Some(command) {
                    self.begin_ptz(command);
                }
            }
        }
    }

    fn begin_ptz(&mut self, command: &'static str) {
        if self.ptz_hold == Some(command) {
            return;
        }
        self.ptz_hold = Some(command);
        self.ptz_cmd = Some(command);
        self.enqueue_ptz(command, false);
    }

    fn end_ptz(&mut self) {
        let Some(command) = self.ptz_hold.take() else {
            return;
        };
        self.enqueue_ptz(command, true);
    }

    fn enqueue_ptz(&self, command: &str, halt: bool) {
        let index = self.focus.unwrap_or(self.selected_cell);
        let Some(source) = self.cells.get(index).and_then(|cell| cell.source) else {
            return;
        };
        let Ok((kind, host, port, user, password)) = self.device_credentials(source.device_id) else {
            return;
        };
        if kind != Vendor::Xm {
            return;
        }
        let _ = self.ptz_tx.send(PtzJob {
            host,
            port,
            user,
            password,
            channel: source.channel,
            command: command.to_string(),
            step: self.ptz_step.clamp(1, 8),
            halt,
        });
    }

    fn page_config(&mut self, ui: &mut egui::Ui, devices: &[LiveDevice]) {
        let xm: Vec<&LiveDevice> = devices.iter().filter(|device| device.kind == Vendor::Xm).collect();
        let hikvision = devices.iter().any(|device| device.kind == Vendor::Hikvision);
        egui::Frame::new()
            .inner_margin(egui::Margin::same(22))
            .show(ui, |ui| {
                ui.label(accent_label(ui, with_icon(ph::SLIDERS, "Конфигуратор"), 22.0));
                ui.add_space(4.0);
                if xm.is_empty() {
                    let text = if hikvision {
                        "Конфигуратор Hikvision пока не подключён"
                    } else {
                        "Добавьте регистратор XM"
                    };
                    ui.label(egui::RichText::new(text).color(MUTED));
                    return;
                }
                if self.config_bundle.lock().unwrap().is_none()
                    && !self.config_busy.load(Ordering::Acquire)
                {
                    self.load_config(false);
                }
                let mut reload = false;
                let mut save = false;
                ui.horizontal(|ui| {
                    polish_toolbar(ui);
                    let current = xm
                        .iter()
                        .find(|device| device.id == self.config_device)
                        .map(|device| device.display_name())
                        .unwrap_or_else(|| "Регистратор".into());
                    egui::ComboBox::from_id_salt("config-device")
                        .selected_text(current)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            tune_menu(ui);
                            for device in &xm {
                                if ui
                                    .selectable_value(
                                        &mut self.config_device,
                                        device.id,
                                        device.display_name(),
                                    )
                                    .clicked()
                                {
                                    reload = true;
                                }
                            }
                        });
                    if ui.button(with_icon(ph::ARROW_CLOCKWISE, "Обновить")).clicked() {
                        reload = true;
                    }
                    let section = self.config_section;
                    let can_save = !matches!(
                        section,
                        config::ConfigSection::Disk | config::ConfigSection::Maintain
                    );
                    if can_save
                        && ui
                            .add_enabled(
                                !self.config_busy.load(Ordering::Acquire),
                                egui::Button::new(with_icon(ph::CHECK, "Сохранить")),
                            )
                            .clicked()
                    {
                        save = true;
                    }
                });
                if hikvision {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Конфигуратор Hikvision пока не подключён").color(MUTED),
                    );
                }
                ui.add_space(12.0);
                if self.config_busy.load(Ordering::Acquire)
                    && self.config_bundle.lock().unwrap().is_none()
                {
                    ui.label(egui::RichText::new("Читаю настройки регистратора…").color(MUTED));
                    return;
                }
                let mut bundle = self.config_bundle.lock().unwrap().clone();
                if let Some(bundle) = bundle.as_mut() {
                    self.config_body(ui, bundle);
                }
                if let Some(bundle) = bundle {
                    *self.config_bundle.lock().unwrap() = Some(bundle);
                }
                if reload {
                    self.load_config(true);
                } else if save {
                    self.commit_config();
                }
            });
    }

    fn config_body(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        let sections = bundle.sections();
        if !sections.contains(&self.config_section) {
            if let Some(section) = sections.first() {
                self.config_section = *section;
            }
        }
        if !bundle.note.is_empty() {
            ui.label(egui::RichText::new(&bundle.note).color(MUTED));
            ui.add_space(8.0);
        }
        let height = ui.available_height().max(240.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(210.0, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        tune_menu(ui);
                        ui.set_width(210.0);
                        ui.spacing_mut().item_spacing.y = 8.0;
                        for section in &sections {
                            let on = self.config_section == *section;
                            if config_nav_item(ui, section.icon(), section.title(), on).clicked() {
                                self.config_section = *section;
                            }
                        }
                    },
                );
                ui.add_space(18.0);
                egui::ScrollArea::vertical()
                    .id_salt("config-form")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            tune_inputs(ui);
                            let width = ui.available_width().clamp(420.0, 720.0);
                            ui.set_width(width);
                            ui.spacing_mut().item_spacing.y = 10.0;
                            match self.config_section {
                                config::ConfigSection::Channels => self.config_channels(ui, bundle),
                                config::ConfigSection::Detect => self.config_detect(ui, bundle),
                                config::ConfigSection::Record => self.config_record(ui, bundle),
                                config::ConfigSection::General => self.config_general(ui, bundle),
                                config::ConfigSection::Picture => self.config_picture(ui, bundle),
                                config::ConfigSection::Network => self.config_network(ui, bundle),
                                config::ConfigSection::Disk => self.config_disk(ui, bundle),
                                config::ConfigSection::Users => self.config_users(ui, bundle),
                                config::ConfigSection::Maintain => self.config_maintain(ui),
                            }
                        });
                    });
            },
        );
    }

    fn config_channel_picker(&mut self, ui: &mut egui::Ui, count: usize, titles: &[String]) {
        if count == 0 {
            return;
        }
        if self.config_channel as usize >= count {
            self.config_channel = 0;
        }
        let label = titles
            .get(self.config_channel as usize)
            .filter(|text| !text.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("Канал {}", self.config_channel + 1));
        tune_inputs(ui);
        egui::ComboBox::from_id_salt("config-channel")
            .selected_text(format!("{} · {label}", self.config_channel + 1))
            .width(280.0)
            .show_ui(ui, |ui| {
                tune_menu(ui);
                for index in 0..count {
                    let title = titles
                        .get(index)
                        .filter(|text| !text.is_empty())
                        .cloned()
                        .unwrap_or_else(|| format!("Канал {}", index + 1));
                    ui.selectable_value(
                        &mut self.config_channel,
                        index as u16,
                        format!("{} · {title}", index + 1),
                    );
                }
            });
        ui.add_space(10.0);
    }

    fn config_channels(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        ui.label(accent_label(ui, "Имена каналов", 15.0));
        ui.add_space(8.0);
        if bundle.titles.is_empty() {
            ui.label(egui::RichText::new("Регистратор не отдал имена каналов.").color(MUTED));
        }
        for (index, title) in bundle.titles.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{}", index + 1)).color(MUTED));
                text_field(ui, title, false, 280.0, "");
            });
        }
        ui.add_space(16.0);
        if let Some(name) = bundle.encode_name() {
            ui.label(accent_label(ui, "Потоки", 15.0));
            ui.add_space(8.0);
            let channel = self.config_channel as usize;
            let count = bundle
                .values
                .get(name)
                .and_then(|value| value.as_array())
                .map(|items| items.len())
                .unwrap_or(0);
            self.config_channel_picker(ui, count, &bundle.titles);
            if let Some(item) = bundle
                .values
                .get_mut(name)
                .and_then(|value| value.as_array_mut())
                .and_then(|items| items.get_mut(channel))
            {
                config_encode_block(ui, item, "MainFormat", "Основной поток");
                ui.add_space(8.0);
                config_encode_block(ui, item, "ExtraFormat", "Дополнительный поток");
            }
        } else {
            ui.label(
                egui::RichText::new(
                    "Этот регистратор не отдаёт параметры потока. Имена каналов сохраняются.",
                )
                .color(MUTED),
            );
        }
    }

    fn config_detect(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        let count = ["Detect.MotionDetect", "Detect.BlindDetect", "Detect.LossDetect"]
            .iter()
            .filter_map(|name| bundle.values.get(*name).and_then(|value| value.as_array()))
            .map(|items| items.len())
            .max()
            .unwrap_or(0);
        self.config_channel_picker(ui, count, &bundle.titles);
        let channel = self.config_channel as usize;
        config_detect_block(ui, bundle, "Detect.MotionDetect", "Движение", channel, true);
        config_detect_block(ui, bundle, "Detect.BlindDetect", "Заслонение", channel, false);
        config_detect_block(ui, bundle, "Detect.LossDetect", "Потеря видео", channel, false);
    }

    fn config_record(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        let count = bundle
            .values
            .get("Record")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0);
        if count == 0 {
            ui.label(egui::RichText::new("Расписание записи не пришло.").color(MUTED));
            return;
        }
        let titles = bundle.titles.clone();
        self.config_channel_picker(ui, count, &titles);
        let Some(item) = bundle
            .values
            .get_mut("Record")
            .and_then(|value| value.as_array_mut())
            .and_then(|items| items.get_mut(self.config_channel as usize))
        else {
            return;
        };
        let mut mode = config::object_string(item, "RecordMode");
        ui.horizontal(|ui| {
            tune_inputs(ui);
            ui.label("Режим");
            egui::ComboBox::from_id_salt("record-mode")
                .selected_text(record_mode_label(&mode))
                .width(220.0)
                .show_ui(ui, |ui| {
                    tune_menu(ui);
                    for value in ["ConfigRecord", "ManualRecord", "ClosedRecord"] {
                        ui.selectable_value(&mut mode, value.to_string(), record_mode_label(value));
                    }
                });
        });
        config::set_object_string(item, "RecordMode", mode);
        ui.add_space(8.0);
        config_int_row(ui, item, "PacketLength", "Длина файла, мин", 1, 120);
        config_int_row(ui, item, "PreRecord", "Предзапись, с", 0, 30);
        ui.add_space(12.0);
        ui.label(accent_label(ui, "Расписание", 15.0));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for day in 0..7 {
                if ui
                    .selectable_label(self.config_day == day, config::weekday_short(day))
                    .clicked()
                {
                    self.config_day = day;
                }
            }
        });
        ui.label(egui::RichText::new(config::weekday_label(self.config_day)).color(MUTED));
        ui.add_space(8.0);
        let day = self.config_day;
        let Some(slots) = item
            .get_mut("TimeSection")
            .and_then(|value| value.as_array_mut())
            .and_then(|days| days.get_mut(day))
            .and_then(|value| value.as_array_mut())
        else {
            return;
        };
        for slot in slots.iter_mut() {
            let Some(text) = slot.as_str() else {
                continue;
            };
            let (mask, mut start, mut end) = config::parse_slot(text);
            let mut regular = mask & 1 != 0;
            let mut motion = mask & 2 != 0;
            let mut alarm = mask & 4 != 0;
            let mut changed = false;
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut regular, "Постоянно").changed();
                changed |= ui.checkbox(&mut motion, "Движение").changed();
                changed |= ui.checkbox(&mut alarm, "Тревога").changed();
                changed |= text_field(ui, &mut start, false, 96.0, "").changed();
                ui.label("—");
                changed |= text_field(ui, &mut end, false, 96.0, "").changed();
            });
            if changed {
                let mask = (mask & !7)
                    | u32::from(regular)
                    | (u32::from(motion) << 1)
                    | (u32::from(alarm) << 2);
                *slot = serde_json::json!(config::format_slot(mask, &start, &end));
            }
        }
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Нулевой тип выключает интервал. Конец 24:00:00 — до полуночи.")
                .color(MUTED),
        );
    }

    fn config_general(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        if let Some(general) = bundle.values.get_mut("General.General") {
            ui.label(accent_label(ui, "Устройство", 15.0));
            ui.add_space(8.0);
            labeled_config_text(ui, general, "MachineName", "Имя", 280.0);
            let mut overwrite = config::object_string(general, "OverWrite");
            ui.horizontal(|ui| {
                tune_inputs(ui);
                ui.label("Диск заполнен");
                egui::ComboBox::from_id_salt("overwrite")
                    .selected_text(overwrite_label(&overwrite))
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        tune_menu(ui);
                        for value in ["OverWrite", "StopRecord"] {
                            ui.selectable_value(&mut overwrite, value.to_string(), overwrite_label(value));
                        }
                    });
            });
            config::set_object_string(general, "OverWrite", overwrite);
            config_int_row(ui, general, "AutoLogout", "Автовыход, мин", 0, 120);
            config_choice(ui, general, "VideoOutPut", "Видеовыход", VIDEO_OUTPUT);
            ui.label(
                egui::RichText::new("Куда регистратор выводит свой экран: HDMI, VGA или выбирает сам.")
                    .color(MUTED),
            );
            ui.add_space(16.0);
        }
        if let Some(location) = bundle.values.get_mut("General.Location") {
            ui.label(accent_label(ui, "Регион", 15.0));
            ui.add_space(8.0);
            config_choice(ui, location, "Language", "Язык", LOCATION_LANGUAGES);
            config_choice(ui, location, "VideoFormat", "Формат видео", LOCATION_VIDEO);
            config_choice(ui, location, "DateFormat", "Формат даты", LOCATION_DATE);
            config_choice(ui, location, "TimeFormat", "Формат времени", LOCATION_TIME);
            config_choice(ui, location, "DateSeparator", "Разделитель даты", LOCATION_SEPARATOR);
            ui.add_space(16.0);
        }
        ui.label(accent_label(ui, "Время", 15.0));
        ui.add_space(8.0);
        self.config_clock_row(ui, bundle);
        ui.add_space(8.0);
        if let Some(zone) = bundle.values.get_mut("System.TimeZone") {
            config_timezone(ui, zone);
            ui.add_space(8.0);
        }
        if let Some(ntp) = bundle.values.get_mut("NetWork.NetNTP") {
            let mut enable = config::object_bool(ntp, "Enable");
            if ui.checkbox(&mut enable, "Синхронизация NTP").changed() {
                config::set_object_bool(ntp, "Enable", enable);
            }
            if let Some(server) = ntp.get_mut("Server") {
                labeled_config_text(ui, server, "Name", "Сервер", 280.0);
            }
            config_int_row(ui, ntp, "UpdatePeriod", "Период, мин", 1, 10_080);
        }
    }

    fn config_picture(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        let count = bundle
            .camera
            .len()
            .max(
                bundle
                    .values
                    .get("AVEnc.VideoWidget")
                    .and_then(|value| value.as_array())
                    .map(|items| items.len())
                    .unwrap_or(0),
            );
        self.config_channel_picker(ui, count, &bundle.titles);
        let channel = self.config_channel as usize;
        if let Some(widget) = bundle
            .values
            .get_mut("AVEnc.VideoWidget")
            .and_then(|value| value.as_array_mut())
            .and_then(|items| items.get_mut(channel))
        {
            ui.label(accent_label(ui, "Подпись на экране", 15.0));
            ui.add_space(8.0);
            if let Some(title) = widget.get_mut("ChannelTitle") {
                labeled_config_text(ui, title, "Name", "Текст", 280.0);
            }
            blend_pair(ui, widget, "ChannelTitleAttribute", "Имя канала");
            blend_pair(ui, widget, "TimeTitleAttribute", "Время");
            ui.add_space(16.0);
        }
        if let Some(Some(camera)) = bundle.camera.get_mut(channel) {
            ui.label(accent_label(ui, "Сенсор", 15.0));
            ui.add_space(8.0);
            config_int_row(ui, camera, "ElecLevel", "Уровень экспозиции", 0, 100);
            if let Some(gain) = camera.get_mut("GainParam") {
                config_int_row(ui, gain, "Gain", "Усиление", 0, 100);
            }
            hex_flag(ui, camera, "PictureMirror", "Зеркало");
            hex_flag(ui, camera, "PictureFlip", "Переворот");
            ui.label(
                egui::RichText::new(
                    "Яркость и контраст отдельной настройкой этот регистратор не отдаёт.",
                )
                .color(MUTED),
            );
        } else if !bundle.has("AVEnc.VideoWidget") {
            ui.label(egui::RichText::new("Картинка канала не пришла.").color(MUTED));
        }
    }

    fn config_network(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        ui.label(
            egui::RichText::new(
                "Сохранение может сменить адрес. Регистратор тогда пропадёт из программы, пока устройство не откроют заново.",
            )
            .color(MUTED),
        );
        ui.add_space(10.0);
        if bundle.has("NetWork.NetCommon") {
            labeled_draft(ui, &mut bundle.net_host, "Адрес");
            labeled_draft(ui, &mut bundle.net_mask, "Маска");
            labeled_draft(ui, &mut bundle.net_gate, "Шлюз");
            if let Some(net) = bundle.values.get_mut("NetWork.NetCommon") {
                labeled_config_text(ui, net, "HostName", "Имя", 220.0);
                config_int_row(ui, net, "HttpPort", "HTTP", 1, 65535);
                config_int_row(ui, net, "TCPPort", "TCP", 1, 65535);
                config_int_row(ui, net, "UDPPort", "UDP", 1, 65535);
                config_int_row(ui, net, "SSLPort", "SSL", 1, 65535);
                ui.label(
                    egui::RichText::new(format!(
                        "MAC {}",
                        config::object_string(net, "MAC")
                    ))
                    .color(MUTED),
                );
            }
        }
        ui.add_space(12.0);
        if let Some(dhcp) = bundle.values.get_mut("NetWork.NetDHCP").and_then(|value| value.as_array_mut()) {
            ui.label(accent_label(ui, "DHCP", 15.0));
            ui.add_space(6.0);
            for item in dhcp.iter_mut() {
                let name = config::object_string(item, "Interface");
                let mut enable = config::object_bool(item, "Enable");
                if ui.checkbox(&mut enable, name).changed() {
                    config::set_object_bool(item, "Enable", enable);
                }
            }
        }
        ui.add_space(12.0);
        ui.label(accent_label(ui, "DNS", 15.0));
        ui.add_space(6.0);
        labeled_draft(ui, &mut bundle.net_dns, "Основной");
        labeled_draft(ui, &mut bundle.net_dns2, "Запасной");
    }

    fn config_disk(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        ui.label(
            egui::RichText::new("Форматирование стирает записи на выбранном разделе.").color(MUTED),
        );
        ui.add_space(10.0);
        let mut format = None;
        if let Some(disks) = bundle.storage.as_array() {
            for disk in disks {
                let physical = config::object_i64(disk, "PlysicalNo").max(0) as usize;
                ui.label(accent_label(
                    ui,
                    format!("Диск {}", physical + 1),
                    15.0,
                ));
                ui.add_space(6.0);
                let Some(parts) = disk.get("Partition").and_then(|value| value.as_array()) else {
                    continue;
                };
                for (part, item) in parts.iter().enumerate() {
                    let total = config::object_string(item, "TotalSpace");
                    if config::hex_megabytes(&total).unwrap_or(0) == 0 {
                        continue;
                    }
                    let status = if config::object_i64(item, "Status") == 0 {
                        "исправен"
                    } else {
                        "проверьте"
                    };
                    ui.label(format!(
                        "Раздел {} · {} свободно из {} · {status}",
                        part + 1,
                        config::format_space(&config::object_string(item, "RemainSpace")),
                        config::format_space(&total),
                    ));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} — {}",
                            config::object_string(item, "NewStartTime"),
                            config::object_string(item, "NewEndTime")
                        ))
                        .color(MUTED),
                    );
                    if ui.button(with_icon(ph::TRASH, "Форматировать")).clicked() {
                        format = Some((physical, part));
                    }
                    ui.add_space(8.0);
                }
            }
        }
        if let Some((disk, part)) = format {
            self.config_format_name.clear();
            self.config_confirm = Some(ConfigConfirm::Format {
                disk,
                part,
                armed: false,
            });
        }
    }

    fn config_users(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        let Some(users) = bundle.users.as_array() else {
            ui.label(egui::RichText::new("Список пользователей не пришёл.").color(MUTED));
            return;
        };
        if users.is_empty() {
            return;
        }
        if self.config_user >= users.len() {
            self.config_user = 0;
        }
        let selected = users
            .get(self.config_user)
            .map(|user| config::object_string(user, "Name"))
            .unwrap_or_default();
        tune_inputs(ui);
        egui::ComboBox::from_id_salt("config-user")
            .selected_text(selected)
            .width(220.0)
            .show_ui(ui, |ui| {
                tune_menu(ui);
                for (index, user) in users.iter().enumerate() {
                    ui.selectable_value(
                        &mut self.config_user,
                        index,
                        config::object_string(user, "Name"),
                    );
                }
            });
        if let Some(user) = users.get(self.config_user) {
            ui.add_space(8.0);
            ui.label(format!("Группа {}", config::object_string(user, "Group")));
        }
        ui.add_space(8.0);
        field_label(ui, "Новый пароль");
        text_field(ui, &mut self.config_password, true, 220.0, "");
        ui.label(
            egui::RichText::new("Пустое поле оставляет текущий пароль. В статус он не пишется.")
                .color(MUTED),
        );
    }

    fn config_maintain(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Перезагрузка на время оборвёт просмотр и запись.").color(MUTED),
        );
        ui.add_space(10.0);
        if ui.button(with_icon(ph::ARROW_CLOCKWISE, "Перезагрузить регистратор")).clicked() {
            self.config_confirm = Some(ConfigConfirm::Reboot);
        }
    }

    fn load_config(&mut self, force: bool) {
        let xm: Vec<LiveDevice> = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .filter(|device| device.kind == Vendor::Xm)
            .cloned()
            .collect();
        if xm.is_empty() {
            self.config_device = 0;
            *self.config_bundle.lock().unwrap() = None;
            return;
        }
        let mut force = force;
        if !xm.iter().any(|device| device.id == self.config_device) {
            self.config_device = xm[0].id;
            force = true;
        }
        if !force && self.config_bundle.lock().unwrap().is_some() {
            return;
        }
        if self
            .config_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let Some(device) = xm.into_iter().find(|item| item.id == self.config_device) else {
            self.config_busy.store(false, Ordering::Release);
            return;
        };
        let token = self.config_token.fetch_add(1, Ordering::AcqRel) + 1;
        let tokens = Arc::clone(&self.config_token);
        let bundle_slot = Arc::clone(&self.config_bundle);
        let busy = Arc::clone(&self.config_busy);
        let status = Arc::clone(&self.status);
        self.config_clock = None;
        self.config_time_draft.clear();
        self.config_time_editing = false;
        self.config_password.clear();
        *status.lock().unwrap() = "Читаю настройки регистратора…".into();
        thread::spawn(move || {
            let loaded = read_config_bundle(&device);
            if tokens.load(Ordering::Acquire) == token {
                *bundle_slot.lock().unwrap() = Some(loaded);
            }
            busy.store(false, Ordering::Release);
        });
    }

    fn commit_config(&mut self) {
        if self.config_busy.load(Ordering::Acquire) {
            return;
        }
        match self.config_section {
            config::ConfigSection::Network => self.config_confirm = Some(ConfigConfirm::Network),
            config::ConfigSection::Users => self.config_confirm = Some(ConfigConfirm::Users),
            config::ConfigSection::Disk | config::ConfigSection::Maintain => {}
            section => self.write_config(section),
        }
    }

    fn write_config(&mut self, section: config::ConfigSection) {
        if section == config::ConfigSection::Users && !self.config_password.is_empty() {
            let hash = sofia_hash(&self.config_password);
            if let Some(bundle) = self.config_bundle.lock().unwrap().as_mut() {
                if let Some(user) = bundle
                    .users
                    .as_array_mut()
                    .and_then(|items| items.get_mut(self.config_user))
                {
                    config::set_object_string(user, "Password", hash);
                }
            }
            self.config_password.clear();
        }
        let Some(bundle) = self.config_bundle.lock().unwrap().clone() else {
            return;
        };
        let Some(device) = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .find(|item| item.id == self.config_device && item.kind == Vendor::Xm)
            .cloned()
        else {
            self.set_status("Регистратор не найден");
            return;
        };
        if section == config::ConfigSection::General
            && self.config_time_editing
            && stamp_epoch(&self.config_time_draft).is_none()
        {
            self.set_status("Время указано не полностью");
            return;
        }
        let password = std::mem::take(&mut self.config_password);
        let clock = self.config_clock;
        let user_index = self.config_user;
        let devices = Arc::clone(&self.devices);
        self.spawn_config(move || {
            apply_config(section, &device, &bundle, &password, clock, user_index, &devices)
        });
    }

    fn config_clock_row(&mut self, ui: &mut egui::Ui, bundle: &mut config::ConfigBundle) {
        if self.config_clock.is_none() {
            if let Some(epoch) = stamp_epoch(&bundle.time) {
                self.config_clock = Some(ConfigClock {
                    epoch,
                    at: bundle.time_at.unwrap_or_else(Instant::now),
                });
            }
        }
        let running = self.config_clock.map(running_clock);
        let mut shown = if self.config_time_editing {
            self.config_time_draft.clone()
        } else {
            running.clone().unwrap_or_else(|| bundle.time.clone())
        };
        ui.horizontal(|ui| {
            ui.label("Часы регистратора");
            let response = text_field(ui, &mut shown, false, 180.0, "");
            if response.changed() {
                self.config_time_draft = shown.clone();
                self.config_time_editing = true;
                if let Some(epoch) = stamp_epoch(&shown) {
                    self.config_clock = Some(ConfigClock {
                        epoch,
                        at: Instant::now(),
                    });
                    bundle.time = shown;
                }
            } else if response.has_focus() {
                if !self.config_time_editing {
                    self.config_time_draft = shown;
                }
                self.config_time_editing = true;
            } else {
                self.config_time_editing = false;
                if let Some(text) = running {
                    bundle.time = text;
                }
            }
        });
        ui.label(
            egui::RichText::new("С момента чтения идут по часам компьютера, сохранение отправляет текущее значение.")
                .color(MUTED),
        );
    }

    fn spawn_config(&mut self, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        if self
            .config_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.set_status("Настройки ещё читаются");
            return;
        }
        let busy = Arc::clone(&self.config_busy);
        let status = Arc::clone(&self.status);
        thread::spawn(move || {
            let text = match work() {
                Ok(text) => text,
                Err(err) => format!("Не сохранилось: {err}"),
            };
            *status.lock().unwrap() = text;
            busy.store(false, Ordering::Release);
        });
    }

    fn config_window(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.config_confirm else {
            return;
        };
        let device_name = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .find(|device| device.id == self.config_device)
            .map(LiveDevice::display_name)
            .unwrap_or_else(|| "регистратор".into());
        let screen = ctx.screen_rect();
        let mut accept = false;
        let mut cancel = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        let mut arm = false;
        egui::Window::new("Подтверждение")
            .id(egui::Id::new("config-confirm"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(1.0, LINE))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label(accent_label(ui, confirm_title(confirm), 16.0));
                ui.add_space(8.0);
                ui.label(confirm_text(confirm, &device_name));
                if let ConfigConfirm::Format { armed, .. } = confirm {
                    ui.add_space(8.0);
                    if !armed {
                        ui.label("Дальше нужно вписать имя устройства.");
                    } else {
                        field_label(ui, "Имя устройства");
                        text_field(ui, &mut self.config_format_name, false, 280.0, "");
                    }
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    polish_toolbar(ui);
                    let ready = match confirm {
                        ConfigConfirm::Format { armed: true, .. } => {
                            self.config_format_name.trim() == device_name
                        }
                        ConfigConfirm::Format { armed: false, .. } => true,
                        _ => true,
                    };
                    let label = match confirm {
                        ConfigConfirm::Format { armed: false, .. } => {
                            with_icon(ph::ARROW_RIGHT, "Продолжить")
                        }
                        ConfigConfirm::Format { armed: true, .. } => {
                            with_icon(ph::TRASH, "Стереть записи")
                        }
                        ConfigConfirm::Reboot => with_icon(ph::ARROW_CLOCKWISE, "Перезагрузить"),
                        _ => with_icon(ph::CHECK, "Сохранить"),
                    };
                    if ui.add_enabled(ready, egui::Button::new(label)).clicked() {
                        if matches!(confirm, ConfigConfirm::Format { armed: false, .. }) {
                            arm = true;
                        } else {
                            accept = true;
                        }
                    }
                    if ui.button(with_icon(ph::X, "Отмена")).clicked() {
                        cancel = true;
                    }
                });
                let _ = screen;
            });
        if arm {
            if let ConfigConfirm::Format { disk, part, .. } = confirm {
                self.config_confirm = Some(ConfigConfirm::Format {
                    disk,
                    part,
                    armed: true,
                });
            }
            return;
        }
        if cancel {
            self.config_confirm = None;
            self.config_format_name.clear();
            return;
        }
        if accept {
            self.config_confirm = None;
            match confirm {
                ConfigConfirm::Network => self.write_config(config::ConfigSection::Network),
                ConfigConfirm::Users => self.write_config(config::ConfigSection::Users),
                ConfigConfirm::Reboot => self.reboot_config(),
                ConfigConfirm::Format { disk, part, .. } => {
                    self.config_format_name.clear();
                    self.format_config_disk(disk, part);
                }
            }
        }
    }

    fn reboot_config(&mut self) {
        let Some(device) = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .find(|item| item.id == self.config_device && item.kind == Vendor::Xm)
            .cloned()
        else {
            return;
        };
        self.spawn_config(move || {
            let mut session = Session::connect(&device.host, device.port, &device.user, &device.password)
                .map_err(|err| err.to_string())?;
            session.reboot().map_err(|err| err.to_string())?;
            Ok("Регистратор перезагружается".into())
        });
    }

    fn format_config_disk(&mut self, disk: usize, part: usize) {
        let Some(device) = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .find(|item| item.id == self.config_device && item.kind == Vendor::Xm)
            .cloned()
        else {
            return;
        };
        self.spawn_config(move || {
            let mut session = Session::connect(&device.host, device.port, &device.user, &device.password)
                .map_err(|err| err.to_string())?;
            session
                .format_disk(disk as u32, part as u32)
                .map_err(|err| err.to_string())?;
            Ok("Форматирование запущено".into())
        });
    }

    fn page_settings(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(24.0);
            ui.set_max_width(640.0);
            ui.label(accent_label(ui, with_icon(ph::GEAR, "Программа"), 22.0));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Параметры этого клиента. На регистратор не отправляются.")
                    .color(MUTED),
            );
            ui.add_space(18.0);
            settings_section(ui, with_icon(ph::BROADCAST, "Поток по умолчанию"), |ui| {
                ui.label(
                    egui::RichText::new(
                        "Этим потоком камеры открываются при запуске. Переключатель внизу экрана просмотра меняет поток только до закрытия программы.",
                    )
                    .color(MUTED),
                );
                ui.add_space(12.0);
                if ui
                    .radio_value(
                        &mut self.default_stream,
                        StreamKind::Main,
                        with_icon(ph::MONITOR, "Основной"),
                    )
                    .changed()
                {
                    self.stream = StreamKind::Main;
                }
                setting_note(ui, "Полное разрешение. Больше нагрузка на сеть и процессор.");
                ui.add_space(8.0);
                if ui
                    .radio_value(
                        &mut self.default_stream,
                        StreamKind::Extra,
                        with_icon(ph::PICTURE_IN_PICTURE, "Дополнительный"),
                    )
                    .changed()
                {
                    self.stream = StreamKind::Extra;
                }
                setting_note(ui, "Меньшее разрешение. Удобен, когда на экране много камер.");
            });
            settings_section(ui, with_icon(ph::CPU, "Декодирование"), |ui| {
                ui.label(
                    egui::RichText::new(if cfg!(windows) {
                        "Видеокарта распаковывает H.264 и H.265 через D3D11. Кадр всё равно копируется в окно. Если поток не пойдёт, эта камера сама вернётся на процессор. MPEG-4 всегда на процессоре."
                    } else {
                        "Видеокарта распаковывает H.264 и H.265 через VA-API, VDPAU или CUDA — что найдёт ffmpeg. Кадр всё равно копируется в окно. Если поток не пойдёт, эта камера сама вернётся на процессор. MPEG-4 всегда на процессоре."
                    })
                    .color(MUTED),
                );
                ui.add_space(10.0);
                if ui
                    .checkbox(
                        &mut self.hwaccel,
                        with_icon(ph::GRAPHICS_CARD, "Аппаратное ускорение"),
                    )
                    .changed()
                {
                    self.set_status(if self.hwaccel {
                        "Декодирование на видеокарте. Потоки перезапускаются."
                    } else {
                        "Декодирование на процессоре. Потоки перезапускаются."
                    });
                    if self.page == Page::Live {
                        self.sync_streams();
                    }
                }
            });
            settings_section(ui, with_icon(ph::BELL, "Звонок домофона"), |ui| {
                ui.label(
                    egui::RichText::new(
                        "Домофон сообщает, когда нажали кнопку вызова. На экран выводится выбранная камера — удобный ракурс, а не глазок. Замок открывается с панели звонка.",
                    )
                    .color(MUTED),
                );
                ui.add_space(10.0);
                ui.checkbox(
                    &mut self.door_call,
                    with_icon(ph::BELL, "Открывать камеру при звонке"),
                );
                let doors = self
                    .devices
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|device| device.kind == Vendor::Hikvision && device.door)
                    .count();
                if doors == 0 {
                    ui.add_space(6.0);
                    setting_note(ui, "Среди устройств нет подключённого домофона Hikvision.");
                }
                ui.add_space(12.0);
                field_label(ui, "Камера на экране");
                let choices = self.camera_choices();
                let selected = self
                    .door_camera
                    .and_then(|source| {
                        choices
                            .iter()
                            .find(|(item, _)| *item == source)
                            .map(|(_, label)| label.clone())
                    })
                    .unwrap_or_else(|| "Не выбрана".into());
                egui::ComboBox::from_id_salt("door-camera")
                    .selected_text(selected)
                    .width(420.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.door_camera, None, "Не выбрана");
                        for (source, label) in &choices {
                            ui.selectable_value(&mut self.door_camera, Some(*source), label);
                        }
                    });
                if self.door_call && self.door_camera.is_none() {
                    ui.add_space(6.0);
                    setting_note(
                        ui,
                        "Камера не выбрана: при звонке останется панель, с которой можно открыть замок.",
                    );
                }
            });
            settings_section(ui, "Цвет акцента".into(), |ui| {
                ui.label(
                    egui::RichText::new("Кнопки, заголовки разделов и подсветка.")
                        .color(MUTED),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (name, rgb) in accent_presets() {
                        let color = accent_from_rgb(rgb);
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::click());
                        ui.painter().circle_filled(rect.center(), 11.0, color);
                        if self.accent == color {
                            ui.painter().circle_stroke(
                                rect.center(),
                                14.0,
                                egui::Stroke::new(1.5, TEXT),
                            );
                        }
                        if response.on_hover_text(name).clicked() {
                            self.accent = color;
                        }
                    }
                    ui.add_space(8.0);
                    let mut rgb = [self.accent.r(), self.accent.g(), self.accent.b()];
                    if egui::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
                        self.accent = accent_from_rgb(rgb);
                    }
                });
            });
        });
    }

    fn set_draft_kind(&mut self, kind: Vendor) {
        if self.draft_kind == kind {
            return;
        }
        let port = self.port.trim();
        if self.draft_kind == Vendor::Xm
            && kind == Vendor::Hikvision
            && (port.is_empty() || port == "34567")
        {
            self.port = "80".into();
        } else if self.draft_kind == Vendor::Hikvision
            && kind == Vendor::Xm
            && (port.is_empty() || port == "80")
        {
            self.port = "34567".into();
        }
        self.draft_kind = kind;
    }

    fn open_device_form(&mut self, edit: Option<u64>) {
        if let Some(id) = edit {
            let Some(device) = self
                .devices
                .lock()
                .unwrap()
                .iter()
                .find(|device| device.id == id)
                .cloned()
            else {
                return;
            };
            self.draft_name = device.name;
            self.draft_kind = device.kind;
            self.host = device.host;
            self.port = device.port.to_string();
            self.user = device.user;
            self.password = device.password;
            self.device_dialog = Some(DeviceDialog::Edit(id));
        } else {
            self.draft_name.clear();
            self.draft_kind = Vendor::Xm;
            self.host.clear();
            self.port = "34567".into();
            if self.user.trim().is_empty() {
                self.user = "admin".into();
            }
            self.password.clear();
            self.device_dialog = Some(DeviceDialog::Add);
        }
    }

    fn close_device_dialog(&mut self) {
        self.device_dialog = None;
        self.draft_name.clear();
        self.host.clear();
        self.password.clear();
    }

    fn page_device(&mut self, ui: &mut egui::Ui, devices: &[LiveDevice]) {
        let rect = ui.max_rect().shrink(8.0);
        ui.painter().rect_filled(rect, CELL_RADIUS, VIDEO);
        let header = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 40.0));
        ui.painter().rect_filled(
            header,
            egui::CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            },
            HEADER,
        );
        let titles = [
            "Отображаемое имя",
            "Адрес",
            "Модель",
            "Протокол",
            "Статус",
        ];
        for (column, title) in device_columns(header).iter().zip(titles) {
            paint_row_text(ui, *column, title, MUTED);
        }
        let body = egui::Rect::from_min_max(egui::pos2(rect.left(), header.bottom()), rect.max);
        let mut remove_id = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
            if devices.is_empty() {
                ui.painter().text(
                    body.center(),
                    egui::Align2::CENTER_CENTER,
                    "Нет устройств",
                    egui::FontId::proportional(15.0),
                    MUTED,
                );
                let _ = ui.allocate_rect(body, egui::Sense::hover());
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("device-table")
                .show(ui, |ui| {
                    for device in devices {
                        let (row, response) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 42.0),
                            egui::Sense::click(),
                        );
                        if response.hovered() {
                            ui.painter().rect_filled(row, 0.0, HOVER);
                        }
                        let status = device_status_text(device);
                        let name_color = if device.connected {
                            egui::Color32::from_rgb(86, 176, 112)
                        } else {
                            egui::Color32::from_rgb(196, 86, 78)
                        };
                        let status_color = if device.connected {
                            name_color
                        } else if device.error.is_empty() {
                            MUTED
                        } else {
                            egui::Color32::from_rgb(196, 86, 78)
                        };
                        let cells = [
                            (device.display_name(), name_color),
                            (format!("{}:{}", device.host, device.port), TEXT),
                            (device_model(device), TEXT),
                            (vendor_name(device.kind).to_owned(), TEXT),
                            (status.clone(), status_color),
                        ];
                        for (column, (text, color)) in device_columns(row).iter().zip(cells) {
                            paint_row_text(ui, *column, &text, color);
                        }
                        response.on_hover_text(&status).context_menu(|ui| {
                            tune_menu(ui);
                            if ui.button(with_icon(ph::LIST_BULLETS, "Подробности")).clicked() {
                                self.device_dialog = Some(DeviceDialog::Details(device.id));
                                ui.close_menu();
                            }
                            if ui.button(with_icon(ph::PENCIL_SIMPLE, "Редактировать")).clicked() {
                                self.open_device_form(Some(device.id));
                                ui.close_menu();
                            }
                            if ui.button(with_icon(ph::TRASH, "Удалить")).clicked() {
                                remove_id = Some(device.id);
                                ui.close_menu();
                            }
                        });
                    }
                });
        });
        if let Some(id) = remove_id {
            self.remove_device(id);
            if matches!(
                self.device_dialog,
                Some(DeviceDialog::Edit(open) | DeviceDialog::Details(open)) if open == id
            ) {
                self.close_device_dialog();
            }
        }
    }

    fn device_window(&mut self, ctx: &egui::Context) {
        if self.page != Page::Device {
            if self.device_dialog.is_some() {
                self.close_device_dialog();
            }
            return;
        }
        let Some(dialog) = self.device_dialog else {
            return;
        };
        let details = if let DeviceDialog::Details(id) = dialog {
            self.devices
                .lock()
                .unwrap()
                .iter()
                .find(|device| device.id == id)
                .cloned()
        } else {
            None
        };
        if matches!(dialog, DeviceDialog::Details(_)) && details.is_none() {
            self.close_device_dialog();
            return;
        }
        let screen = ctx.screen_rect();
        let mut cancel = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        egui::Area::new(egui::Id::new("device-shade"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(150));
                if response.clicked() {
                    cancel = true;
                }
            });
        let mut save = false;
        let title = match dialog {
            DeviceDialog::Add => "Новое устройство",
            DeviceDialog::Edit(_) => "Изменить устройство",
            DeviceDialog::Details(_) => "Подробности",
        };
        egui::Window::new(title)
            .id(egui::Id::new("device-popup"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .movable(false)
            .order(egui::Order::Foreground)
            .fade_in(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(22))
                    .corner_radius(egui::CornerRadius::same(12))
                    .stroke(egui::Stroke::new(1.0, LINE)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label(accent_label(ui, title, 18.0));
                ui.add_space(12.0);
                match dialog {
                    DeviceDialog::Details(_) => {
                        if let Some(device) = &details {
                            device_detail_rows(ui, device);
                        }
                        ui.add_space(12.0);
                        if ui.button(with_icon(ph::X, "Закрыть")).clicked() {
                            cancel = true;
                        }
                    }
                    DeviceDialog::Add | DeviceDialog::Edit(_) => {
                        self.device_form(ui);
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            let caption = if matches!(dialog, DeviceDialog::Add) {
                                with_icon(ph::PLUS, "Добавить")
                            } else {
                                with_icon(ph::CHECK, "Сохранить")
                            };
                            if ui.add_sized([168.0, 34.0], egui::Button::new(caption)).clicked() {
                                save = true;
                            }
                            if ui.button(with_icon(ph::X, "Отмена")).clicked() {
                                cancel = true;
                            }
                        });
                    }
                }
            });
        if cancel {
            self.close_device_dialog();
            return;
        }
        if save {
            let ok = match dialog {
                DeviceDialog::Add => self.add_device(),
                DeviceDialog::Edit(id) => self.write_device(id),
                DeviceDialog::Details(_) => false,
            };
            if ok {
                self.close_device_dialog();
            }
        }
    }

    fn device_form(&mut self, ui: &mut egui::Ui) {
        tune_inputs(ui);
        let width = 420.0;
        field_label(ui, "Отображаемое имя");
        text_field(ui, &mut self.draft_name, false, width, "");
        ui.add_space(14.0);
        field_label(ui, "Протокол");
        let mut kind = self.draft_kind;
        egui::ComboBox::from_id_salt("device-kind")
            .selected_text(vendor_name(kind))
            .width(width)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut kind, Vendor::Xm, "Xiongmai");
                ui.selectable_value(&mut kind, Vendor::Hikvision, "Hikvision");
            });
        if kind != self.draft_kind {
            self.set_draft_kind(kind);
        }
        ui.add_space(14.0);
        field_label(ui, "Адрес");
        text_field(ui, &mut self.host, false, width, "");
        ui.add_space(14.0);
        field_label(ui, "Порт");
        text_field(ui, &mut self.port, false, width, "");
        ui.add_space(6.0);
        let hint = match self.draft_kind {
            Vendor::Xm => "Стандартный порт Xiongmai — 34567",
            Vendor::Hikvision => "Стандартный порт Hikvision — 80. Видео идёт по RTSP, порт 554.",
        };
        ui.label(egui::RichText::new(hint).color(MUTED).size(12.0));
        ui.add_space(14.0);
        field_label(ui, "Пользователь");
        text_field(ui, &mut self.user, false, width, "");
        ui.add_space(14.0);
        field_label(ui, "Пароль");
        text_field(ui, &mut self.password, true, width, "");
    }

    fn device_branch(&mut self, ui: &mut egui::Ui, device: &LiveDevice, archive: bool) {
        let count = device_channel_count(device);
        let title = if device.name.trim().is_empty() {
            device.host.clone()
        } else {
            format!("{} ({})", device.display_name(), device.host)
        };
        egui::CollapsingHeader::new(with_icon(ph::HARD_DRIVE, &title))
            .default_open(true)
            .id_salt(("device", device.id))
            .show(ui, |ui| {
                if !device.connected && !device.error.is_empty() {
                    ui.label(egui::RichText::new(&device.error).color(MUTED));
                    if ui.button(with_icon(ph::ARROW_CLOCKWISE, "Повторить")).clicked() {
                        self.probe(device.id);
                    }
                } else if count == 0 {
                    ui.label(egui::RichText::new("Каналы ещё не получены").color(MUTED));
                }
                let selected = self
                    .cells
                    .get(self.focus.unwrap_or(self.selected_cell))
                    .and_then(|cell| cell.source);
                for channel in 0..count {
                    let source = Source {
                        device_id: device.id,
                        channel,
                    };
                    let name = channel_title(Some(device), channel);
                    let status = channel_link(device, channel);
                    let placed = self.channel_placed(source);
                    let chosen = if archive {
                        self.archive_device == device.id && self.archive_channel == channel
                    } else {
                        selected == Some(source)
                    };
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::hover());
                        ui.painter()
                            .circle_filled(
                                rect.center(),
                                3.5,
                                link_color(status, if archive { chosen } else { placed }),
                            );
                        let label = egui::RichText::new(&name).color(if chosen {
                            TEXT
                        } else if !archive && placed && link_is_live(status) {
                            TEXT
                        } else {
                            MUTED
                        });
                        let hint = if archive {
                            format!("{}\nПоказать записи за выбранный день", link_hint(status))
                        } else {
                            format!(
                                "{}\nОдин клик — перейти к окну, двойной — открыть в текущем\nПравая кнопка — переименовать",
                                list_hint(status, placed)
                            )
                        };
                        let row = ui
                            .scope(|ui| {
                                tune_list_selection(ui);
                                ui.selectable_label(chosen, label)
                            })
                            .inner
                            .on_hover_text(hint);
                        if archive {
                            if row.clicked() {
                                self.pick_archive(device.id, channel);
                            }
                        } else if row.double_clicked() {
                            let cell = self
                                .list_click
                                .take()
                                .map(|click| click.cell)
                                .unwrap_or_else(|| self.focus.unwrap_or(self.selected_cell));
                            self.open_channel(cell, source);
                        } else if row.clicked() {
                            self.list_click = Some(ListClick {
                                source,
                                cell: self.focus.unwrap_or(self.selected_cell),
                                at: std::time::Instant::now(),
                            });
                        }
                        let current = name.clone();
                        let device_id = device.id;
                        row.context_menu(|ui| {
                            tune_menu(ui);
                            if ui.button(with_icon(ph::PENCIL_SIMPLE, "Переименовать")).clicked() {
                                self.open_rename(device_id, channel, current.clone());
                                ui.close_menu();
                            }
                        });
                    });
                }
            });
    }

    fn pick_archive(&mut self, device: u64, channel: u16) {
        self.archive_device = device;
        self.archive_channel = channel;
        self.search();
    }

    fn archive_bar_height(&self) -> f32 {
        ARCHIVE_BAR
    }

    fn step_range(&mut self, delta: i64) {
        let Some(from) = shift_stamp(&self.archive_from, delta) else {
            return;
        };
        let Some(to) = shift_stamp(&self.archive_to, delta) else {
            return;
        };
        self.archive_from = from;
        self.archive_to = to;
        self.search();
    }

    fn jump_today(&mut self) {
        let day = today_day();
        self.archive_from = format!("{day} 00:00:00");
        self.archive_to = format!("{day} 23:59:59");
        self.search();
    }

    fn archive_clock(&self) -> Option<i64> {
        self.archive.clock.lock().unwrap().map(wall_epoch)
    }

    fn play_at(&mut self, at: i64, recordings: &[Recording]) {
        let contains = recordings.iter().find(|item| {
            matches!(
                (stamp_epoch(&item.begin), stamp_epoch(&item.end)),
                (Some(begin), Some(end)) if at >= begin && at <= end.max(begin)
            )
        });
        let next = recordings
            .iter()
            .filter(|item| stamp_epoch(&item.begin).is_some_and(|begin| begin >= at))
            .min_by_key(|item| stamp_epoch(&item.begin).unwrap_or(i64::MAX));
        if let Some(recording) = contains.or(next) {
            self.play(recording.clone());
        }
    }

    fn archive_console(&mut self, ui: &mut egui::Ui, recordings: &[Recording]) {
        self.archive
            .play_speed
            .store(normalize_speed(self.archive_speed), Ordering::Relaxed);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            polish_toolbar(ui);
            if ui
                .button(ph::ARROW_LEFT)
                .on_hover_text("Сдвинуть период на сутки назад")
                .clicked()
            {
                self.step_range(-1);
            }
            let label = format_range_label(&self.archive_from, &self.archive_to);
            if ui
                .button(with_icon(ph::CALENDAR_BLANK, &label))
                .on_hover_text("Выбрать дату и время")
                .clicked()
            {
                self.open_archive_picker();
            }
            if ui
                .button(ph::ARROW_RIGHT)
                .on_hover_text("Сдвинуть период на сутки вперёд")
                .clicked()
            {
                self.step_range(1);
            }
            let today = today_day();
            let full_today = self.archive_from == format!("{today} 00:00:00")
                && self.archive_to == format!("{today} 23:59:59");
            if !full_today && ui.button("Сегодня").clicked() {
                self.jump_today();
            }
            ui.add_space(8.0);
            if ui.button(with_icon(ph::MAGNIFYING_GLASS, "Найти")).clicked() {
                self.search();
            }
            if ui
                .button(ph::SKIP_BACK)
                .on_hover_text("Предыдущий файл")
                .clicked()
            {
                self.play_neighbor(recordings, -1);
            }
            if ui.button(ph::PLAY).on_hover_text("Смотреть фрагмент").clicked() {
                let at = self
                    .archive_clock()
                    .or_else(|| stamp_epoch(&self.archive_from))
                    .unwrap_or(0);
                self.play_at(at, recordings);
            }
            if ui.button(ph::STOP).on_hover_text("Остановить").clicked() {
                self.archive.stop();
                self.archive_seen = 0;
                self.archive_texture = None;
                self.archive_playing = None;
                self.seek_preview = None;
                self.set_status("Воспроизведение остановлено");
            }
            if ui
                .button(ph::SKIP_FORWARD)
                .on_hover_text("Следующий файл")
                .clicked()
            {
                self.play_neighbor(recordings, 1);
            }
            egui::ComboBox::from_id_salt("archive-speed")
                .selected_text(speed_label(self.archive_speed))
                .width(68.0)
                .show_ui(ui, |ui| {
                    for speed in [1, 2, 4, 8, 16] {
                        ui.selectable_value(&mut self.archive_speed, speed, speed_label(speed));
                    }
                })
                .response
                .on_hover_text("Скорость воспроизведения");
            if let Some(at) = self.archive_clock() {
                ui.add_space(8.0);
                ui.label(accent_label(
                    ui,
                    format_mark(at, &self.archive_from),
                    15.0,
                ));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                if ui
                    .button(with_icon(ph::FOLDER_OPEN, "Показать файлы"))
                    .on_hover_text("Открыть папку с сохранёнными файлами")
                    .clicked()
                {
                    self.reveal_downloads();
                }
                if self.download_busy.load(Ordering::Acquire)
                    && ui
                        .button(with_icon(ph::X, "Отмена"))
                        .on_hover_text("Остановить скачивание")
                        .clicked()
                {
                    self.cancel_download();
                }
                let status = self.status_line();
                ui.label(egui::RichText::new(status).color(MUTED));
            });
        });
        if self.download_busy.load(Ordering::Acquire) {
            ui.add_space(6.0);
            let percent = self.download_percent();
            paint_download_bar(ui, percent.map(|value| value as f32 / 100.0));
        }
        ui.add_space(6.0);
        self.archive_timeline(ui, recordings);
    }

    fn archive_files(&mut self, ui: &mut egui::Ui, recordings: &[Recording]) {
        let height = (ui.available_height() - 8.0).max(48.0);
        egui::ScrollArea::both()
            .id_salt("archive-files")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if recordings.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("За выбранный период записей нет").color(MUTED),
                    );
                    return;
                }
                for recording in recordings {
                    ui.horizontal(|ui| {
                        ui.add_space(4.0);
                        let busy = self.download_busy.load(Ordering::Acquire);
                        let this_file = busy
                            && self.download_path.lock().unwrap().as_str() == recording.path;
                        if this_file {
                            let caption = self
                                .download_percent()
                                .map(|value| format!("{value}%"))
                                .unwrap_or_else(|| "…".into());
                            ui.label(egui::RichText::new(caption).color(ui_accent(ui)));
                            if ui
                                .button(ph::X)
                                .on_hover_text("Остановить скачивание")
                                .clicked()
                            {
                                self.cancel_download();
                            }
                        } else if ui
                            .add_enabled(!busy, egui::Button::new(ph::DOWNLOAD_SIMPLE))
                            .on_hover_text("Скачать фрагмент")
                            .clicked()
                        {
                            self.open_download(recording.clone());
                        }
                        let span = match (stamp_epoch(&recording.begin), stamp_epoch(&recording.end)) {
                            (Some(begin), Some(end)) => format_span((end - begin).max(0)),
                            _ => "—".into(),
                        };
                        let clip = format_clip(&recording.begin, &recording.end);
                        let label = if recording.bytes == 0 {
                            format!("{clip}   {span}")
                        } else {
                            format!("{clip}   {span} · {}", format_bytes(recording.bytes))
                        };
                        let playing = self
                            .archive_playing
                            .as_ref()
                            .is_some_and(|clip| clip.path == recording.path);
                        let color = if playing { ui_accent(ui) } else { TEXT };
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new(label).color(color))
                                    .frame(false),
                            )
                            .on_hover_text("Смотреть")
                            .clicked()
                        {
                            self.play(recording.clone());
                        }
                    });
                }
            });
    }

    fn archive_timeline(&mut self, ui: &mut egui::Ui, recordings: &[Recording]) {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 78.0),
            egui::Sense::hover(),
        );
        let label = rect.with_max_x((rect.left() + 160.0).min(rect.right()));
        let scale = egui::Rect::from_min_max(
            egui::pos2(label.right() + 8.0, rect.top()),
            rect.right_bottom(),
        );
        if scale.width() < 40.0 {
            return;
        }
        let camera = {
            let devices = self.devices.lock().unwrap();
            let device = devices.iter().find(|item| item.id == self.archive_device);
            device
                .map(|item| channel_title(Some(item), self.archive_channel))
                .unwrap_or_else(|| "Камера".into())
        };
        paint_row_text(ui, label, &camera, TEXT);
        let Some(start) = stamp_epoch(&self.archive_from) else {
            return;
        };
        let Some(range_end) = stamp_epoch(&self.archive_to) else {
            return;
        };
        let span = (range_end - start).max(1);
        let ruler = scale.with_max_y(scale.top() + 22.0);
        let track = egui::Rect::from_min_max(
            egui::pos2(scale.left(), ruler.bottom() + 4.0),
            scale.right_bottom(),
        );
        ui.painter().rect_filled(track, 4.0, VIDEO);
        let accent = ui_accent(ui);
        let step = if span <= 6 * 3600 {
            3600
        } else if span <= 30 * 3600 {
            2 * 3600
        } else {
            6 * 3600
        };
        let mut tick = if start % step == 0 {
            start
        } else {
            start.div_euclid(step) * step + step
        };
        let mut first = true;
        while tick <= range_end {
            let x = scale_at(scale, start, span, tick);
            let stamp = epoch_stamp(tick);
            let caption = if span > 26 * 3600 {
                format!(
                    "{} {}",
                    stamp.get(8..10).unwrap_or(""),
                    stamp.get(11..13).unwrap_or("")
                )
            } else {
                stamp.get(11..13).unwrap_or("").to_string()
            };
            ui.painter().text(
                egui::pos2(x, ruler.center().y),
                if first {
                    egui::Align2::LEFT_CENTER
                } else {
                    egui::Align2::CENTER_CENTER
                },
                caption,
                egui::FontId::proportional(12.0),
                MUTED,
            );
            ui.painter().line_segment(
                [egui::pos2(x, track.top()), egui::pos2(x, track.top() + 6.0)],
                egui::Stroke::new(1.0, LINE),
            );
            first = false;
            tick += step;
        }
        let bar_y = track.center().y;
        for recording in recordings {
            let Some(begin) = stamp_epoch(&recording.begin) else {
                continue;
            };
            let Some(end) = stamp_epoch(&recording.end) else {
                continue;
            };
            if end < start || begin > range_end {
                continue;
            }
            let left = scale_at(scale, start, span, begin);
            let right = scale_at(scale, start, span, end).max(left + 3.0);
            let bar = egui::Rect::from_min_max(
                egui::pos2(left, bar_y - 7.0),
                egui::pos2(right.min(scale.right()), bar_y + 7.0),
            );
            ui.painter().rect_filled(bar, 3.0, accent);
        }
        if let Some(at) = self.archive_clock() {
            if (start..=range_end).contains(&at) {
                let x = scale_at(scale, start, span, at);
                ui.painter().line_segment(
                    [egui::pos2(x, ruler.top()), egui::pos2(x, track.bottom())],
                    egui::Stroke::new(1.5, TEXT),
                );
            }
        }
        let response = ui.interact(scale, egui::Id::new("archive-scale"), egui::Sense::click());
        let clicked = response.clicked();
        let pointer = response.interact_pointer_pos();
        if let Some(pos) = response.hover_pos() {
            ui.painter().line_segment(
                [egui::pos2(pos.x, track.top()), egui::pos2(pos.x, track.bottom())],
                egui::Stroke::new(1.0, MUTED),
            );
            response.on_hover_text(format_mark(
                at_from_x(scale, start, span, pos.x),
                &self.archive_from,
            ));
        }
        if clicked {
            if let Some(pos) = pointer {
                self.play_at(at_from_x(scale, start, span, pos.x), recordings);
            }
        }
    }

    fn open_archive_picker(&mut self) {
        self.picker_journal = false;
        self.picker_from = self.archive_from.clone();
        self.picker_to = self.archive_to.clone();
        self.picker_editing_from = true;
        self.follow_picker_month();
        self.sync_time_fields();
        self.archive_picker = true;
    }

    fn follow_picker_month(&mut self) {
        let stamp = if self.picker_editing_from {
            &self.picker_from
        } else {
            &self.picker_to
        };
        if let Some((year, month, _, _, _, _)) = parse_stamp(stamp) {
            self.picker_year = year;
            self.picker_month = month;
        }
    }

    fn archive_picker_window(&mut self, ctx: &egui::Context) {
        if self.page != Page::Archive && self.page != Page::Journal {
            self.archive_picker = false;
            self.picker_journal = false;
            return;
        }
        if !self.archive_picker {
            return;
        }
        let screen = ctx.screen_rect();
        let mut cancel = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        egui::Area::new(egui::Id::new("archive-shade"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(150));
                if response.clicked() {
                    cancel = true;
                }
            });
        let mut apply = false;
        let mut picked_day = None;
        egui::Window::new("Период")
            .id(egui::Id::new("archive-picker"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .movable(false)
            .order(egui::Order::Foreground)
            .fade_in(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(20))
                    .corner_radius(egui::CornerRadius::same(12))
                    .stroke(egui::Stroke::new(1.0, LINE)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(372.0);
                ui.horizontal(|ui| {
                    ui.label(accent_label(ui, "Период", 18.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("до 2 суток").size(13.0).color(MUTED));
                    });
                });
                ui.add_space(14.0);
                let card_width = (ui.available_width() - 8.0) / 2.0;
                ui.horizontal(|ui| {
                    if endpoint_card(ui, "С", &self.picker_from, self.picker_editing_from, card_width)
                    {
                        self.picker_editing_from = true;
                        self.reveal_stamp(&self.picker_from.clone());
                        self.sync_time_fields();
                    }
                    if endpoint_card(ui, "До", &self.picker_to, !self.picker_editing_from, card_width)
                    {
                        self.picker_editing_from = false;
                        self.reveal_stamp(&self.picker_to.clone());
                        self.sync_time_fields();
                    }
                });
                ui.add_space(16.0);
                self.picker_month_bar(ui);
                ui.add_space(6.0);
                picked_day = self.picker_calendar(ui);
                ui.add_space(14.0);
                self.picker_time(ui);
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 8.0) / 2.0;
                    let accent = ui_accent(ui);
                    ui.scope(|ui| {
                        let ink = accent_ink(accent);
                        ui.visuals_mut().override_text_color = Some(ink);
                        ui.visuals_mut().widgets.inactive.fg_stroke.color = ink;
                        ui.visuals_mut().widgets.hovered.fg_stroke.color = ink;
                        ui.visuals_mut().widgets.hovered.bg_fill = accent;
                        ui.visuals_mut().widgets.active.fg_stroke.color = ink;
                        if ui
                            .add_sized([width, 36.0], egui::Button::new("Найти").fill(accent))
                            .clicked()
                        {
                            apply = true;
                        }
                    });
                    if ui
                        .add_sized([width, 36.0], egui::Button::new("Отмена"))
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });
        if let Some(day) = picked_day {
            self.pick_day(self.picker_year, self.picker_month, day);
        }
        if apply {
            if let Ok((from, to)) = prepare_range(&self.picker_from, &self.picker_to) {
                if self.picker_journal {
                    self.journal_from = from;
                    self.journal_to = to;
                    self.picker_journal = false;
                    self.archive_picker = false;
                    self.search_journal();
                } else {
                    self.archive_from = from;
                    self.archive_to = to;
                    self.archive_picker = false;
                    self.search();
                }
            }
        } else if cancel {
            self.picker_journal = false;
            self.archive_picker = false;
        }
    }

    fn reveal_stamp(&mut self, stamp: &str) {
        if let Some((year, month, _, _, _, _)) = parse_stamp(stamp) {
            if self.picker_year != year || self.picker_month != month {
                self.picker_year = year;
                self.picker_month = month;
            }
        }
    }

    fn sync_time_fields(&mut self) {
        let stamp = if self.picker_editing_from {
            &self.picker_from
        } else {
            &self.picker_to
        };
        if let Some((_, _, _, hour, minute, _)) = parse_stamp(stamp) {
            self.picker_hour = format!("{hour:02}");
            self.picker_minute = format!("{minute:02}");
        }
    }

    fn write_active(&mut self, epoch: i64) {
        let other = stamp_epoch(if self.picker_editing_from {
            &self.picker_to
        } else {
            &self.picker_from
        });
        let Some(other) = other else {
            return;
        };
        let stamp = epoch_stamp(snap_endpoint(epoch, other, self.picker_editing_from));
        if self.picker_editing_from {
            self.picker_from = stamp;
            self.reveal_stamp(&self.picker_from.clone());
        } else {
            self.picker_to = stamp;
            self.reveal_stamp(&self.picker_to.clone());
        }
        self.sync_time_fields();
    }

    fn pick_day(&mut self, year: i32, month: u32, day: u32) {
        let current = if self.picker_editing_from {
            self.picker_from.clone()
        } else {
            self.picker_to.clone()
        };
        let (_, _, _, hour, minute, second) =
            parse_stamp(&current).unwrap_or((year, month, day, 0, 0, 0));
        let epoch = days_from_civil(year, month, day) * 86400
            + i64::from(hour) * 3600
            + i64::from(minute) * 60
            + i64::from(second);
        let choosing_start = self.picker_editing_from;
        self.write_active(epoch);
        if choosing_start {
            self.picker_editing_from = false;
            self.sync_time_fields();
        }
    }

    fn step_active_time(&mut self, hours: i32, minutes: i32) {
        let stamp = if self.picker_editing_from {
            &self.picker_from
        } else {
            &self.picker_to
        };
        let Some(epoch) = stamp_epoch(stamp) else {
            return;
        };
        self.write_active(epoch + i64::from(hours) * 3600 + i64::from(minutes) * 60);
    }

    fn commit_time_text(&mut self) -> bool {
        let Some(hour) = self.picker_hour.parse::<u32>().ok().filter(|value| *value <= 23) else {
            return false;
        };
        let Some(minute) = self.picker_minute.parse::<u32>().ok().filter(|value| *value <= 59)
        else {
            return false;
        };
        let stamp = if self.picker_editing_from {
            self.picker_from.clone()
        } else {
            self.picker_to.clone()
        };
        let Some((year, month, day, _, _, _)) = parse_stamp(&stamp) else {
            return false;
        };
        let second = if self.picker_editing_from { 0 } else { 59 };
        let epoch = days_from_civil(year, month, day) * 86400
            + i64::from(hour) * 3600
            + i64::from(minute) * 60
            + i64::from(second);
        self.write_active(epoch);
        true
    }

    fn picker_month_bar(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let origin = ui.cursor().min;
        let rect = egui::Rect::from_min_size(origin, egui::vec2(width, 36.0));
        ui.allocate_rect(rect, egui::Sense::hover());
        let left = egui::Rect::from_min_size(rect.min, egui::vec2(36.0, 36.0));
        let right = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 36.0, rect.top()),
            egui::vec2(36.0, 36.0),
        );
        if ui.put(left, egui::Button::new(ph::CARET_LEFT)).clicked() {
            shift_month(&mut self.picker_year, &mut self.picker_month, -1);
        }
        if ui.put(right, egui::Button::new(ph::CARET_RIGHT)).clicked() {
            shift_month(&mut self.picker_year, &mut self.picker_month, 1);
        }
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{} {}", month_title(self.picker_month), self.picker_year),
            egui::FontId::proportional(16.0),
            TEXT,
        );
    }

    fn picker_calendar(&mut self, ui: &mut egui::Ui) -> Option<u32> {
        let year = self.picker_year;
        let month = self.picker_month;
        let width = ui.available_width();
        let cell_w = width / 7.0;
        let row_h = 40.0;
        let lead = weekday_monday(year, month, 1) as usize;
        let count = days_in_month(year, month);
        let mut slots = vec![None; lead];
        for day in 1..=count {
            slots.push(Some(day));
        }
        while slots.len() % 7 != 0 {
            slots.push(None);
        }
        let weeks = slots.len() / 7;
        let height = 28.0 + row_h * weeks as f32;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let Some(start_epoch) = stamp_epoch(&self.picker_from) else {
            return None;
        };
        let Some(end_epoch) = stamp_epoch(&self.picker_to) else {
            return None;
        };
        let start_day = start_epoch.div_euclid(86400);
        let end_day = end_epoch.div_euclid(86400);
        let pointer = ui.input(|input| input.pointer.hover_pos());
        let mut hover_day = None;
        for week in 0..weeks {
            for col in 0..7 {
                let Some(day) = slots[week * 7 + col] else {
                    continue;
                };
                let cell = calendar_cell(rect, cell_w, row_h, week, col);
                if pointer.is_some_and(|pos| cell.contains(pos)) {
                    hover_day = Some(day);
                }
            }
        }
        let mut band_end = end_day;
        if !self.picker_editing_from {
            if let Some(day) = hover_day {
                let (_, _, _, hour, minute, second) =
                    parse_stamp(&self.picker_to).unwrap_or((0, 0, 0, 23, 59, 59));
                let hovered = days_from_civil(year, month, day) * 86400
                    + i64::from(hour) * 3600
                    + i64::from(minute) * 60
                    + i64::from(second);
                band_end = snap_endpoint(hovered, start_epoch, false).div_euclid(86400);
            }
        }
        let (band_from, band_to) = if start_day <= band_end {
            (start_day, band_end)
        } else {
            (band_end, start_day)
        };
        let accent = ui_accent(ui);
        let band = tint(TILE, accent, 42);
        let ink = accent_ink(accent);
        let today = stamp_epoch(&format!("{} 12:00:00", today_day())).map(|value| value.div_euclid(86400));
        for (col, name) in ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"].iter().enumerate() {
            ui.painter().text(
                egui::pos2(rect.left() + (col as f32 + 0.5) * cell_w, rect.top() + 10.0),
                egui::Align2::CENTER_CENTER,
                *name,
                egui::FontId::proportional(12.0),
                MUTED,
            );
        }
        let mut picked = None;
        for week in 0..weeks {
            for col in 0..7 {
                let Some(day) = slots[week * 7 + col] else {
                    continue;
                };
                let cell = calendar_cell(rect, cell_w, row_h, week, col);
                let index = days_from_civil(year, month, day);
                let in_band = index >= band_from && index <= band_to && band_from != band_to;
                if in_band {
                    let left_cap = col == 0 || index == band_from;
                    let right_cap = col == 6 || index == band_to;
                    let radius = egui::CornerRadius {
                        nw: if left_cap { 16 } else { 0 },
                        sw: if left_cap { 16 } else { 0 },
                        ne: if right_cap { 16 } else { 0 },
                        se: if right_cap { 16 } else { 0 },
                    };
                    let bar = egui::Rect::from_center_size(cell.center(), egui::vec2(cell.width(), 32.0));
                    ui.painter().rect_filled(bar, radius, band);
                }
                let endpoint = index == start_day || index == band_end;
                let mark = egui::Rect::from_center_size(cell.center(), egui::vec2(32.0, 32.0));
                if endpoint {
                    ui.painter().rect_filled(mark, 16.0, accent);
                } else if hover_day == Some(day) {
                    ui.painter().rect_filled(mark, 16.0, HOVER);
                } else if today == Some(index) {
                    ui.painter().rect_stroke(
                        mark,
                        16.0,
                        egui::Stroke::new(1.0, accent),
                        egui::StrokeKind::Inside,
                    );
                }
                ui.painter().text(
                    cell.center(),
                    egui::Align2::CENTER_CENTER,
                    day.to_string(),
                    egui::FontId::proportional(14.0),
                    if endpoint { ink } else { TEXT },
                );
                let response = ui.interact(
                    cell,
                    egui::Id::new(("picker-day", year, month, day)),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    picked = Some(day);
                }
            }
        }
        picked
    }

    fn picker_time(&mut self, ui: &mut egui::Ui) {
        let caption = if self.picker_editing_from {
            "Время начала"
        } else {
            "Время конца"
        };
        ui.label(egui::RichText::new(caption).size(13.0).color(MUTED));
        ui.add_space(6.0);
        let mut hour = std::mem::take(&mut self.picker_hour);
        let mut minute = std::mem::take(&mut self.picker_minute);
        let mut hour_delta = 0i32;
        let mut minute_delta = 0i32;
        let mut hour_edit = None;
        let mut minute_edit = None;
        ui.horizontal(|ui| {
            tune_inputs(ui);
            if ui
                .add_sized([32.0, 36.0], egui::Button::new(ph::MINUS))
                .clicked()
            {
                hour_delta = -1;
            }
            hour_edit = Some(time_box(ui, &mut hour, "picker-hour"));
            if ui
                .add_sized([32.0, 36.0], egui::Button::new(ph::PLUS))
                .clicked()
            {
                hour_delta = 1;
            }
            ui.add_space(4.0);
            ui.label(egui::RichText::new(":").size(18.0).color(MUTED));
            ui.add_space(4.0);
            if ui
                .add_sized([32.0, 36.0], egui::Button::new(ph::MINUS))
                .clicked()
            {
                minute_delta = -1;
            }
            minute_edit = Some(time_box(ui, &mut minute, "picker-minute"));
            if ui
                .add_sized([32.0, 36.0], egui::Button::new(ph::PLUS))
                .clicked()
            {
                minute_delta = 1;
            }
        });
        hour.retain(|ch| ch.is_ascii_digit());
        minute.retain(|ch| ch.is_ascii_digit());
        hour.truncate(2);
        minute.truncate(2);
        self.picker_hour = hour;
        self.picker_minute = minute;
        if hour_delta != 0 || minute_delta != 0 {
            self.step_active_time(hour_delta, minute_delta);
            return;
        }
        let hour_ready = hour_edit.is_some_and(|response| {
            response.lost_focus() || (response.changed() && self.picker_hour.len() == 2)
        });
        let minute_ready = minute_edit.is_some_and(|response| {
            response.lost_focus() || (response.changed() && self.picker_minute.len() == 2)
        });
        if (hour_ready || minute_ready) && !self.commit_time_text() {
            self.sync_time_fields();
        }
    }

    fn download_window(&mut self, ctx: &egui::Context) {
        if self.page != Page::Archive {
            self.download_draft = None;
            return;
        }
        let Some(mut draft) = self.download_draft.clone() else {
            return;
        };
        let screen = ctx.screen_rect();
        let mut cancel = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        egui::Area::new(egui::Id::new("download-shade"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(150));
                if response.clicked() {
                    cancel = true;
                }
            });
        let mut confirm = false;
        egui::Window::new("Скачать")
            .id(egui::Id::new("download-popup"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .movable(false)
            .order(egui::Order::Foreground)
            .fade_in(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(20))
                    .corner_radius(egui::CornerRadius::same(12))
                    .stroke(egui::Stroke::new(1.0, LINE)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label(accent_label(ui, "Скачать фрагмент", 18.0));
                ui.add_space(4.0);
                let file_span = stamp_epoch(&draft.file_end)
                    .zip(stamp_epoch(&draft.file_begin))
                    .map(|(end, begin)| (end - begin).max(0))
                    .unwrap_or(0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {} · {}",
                        format_clip(&draft.file_begin, &draft.file_end),
                        format_span(file_span),
                        format_bytes(draft.bytes)
                    ))
                    .color(MUTED),
                );
                ui.add_space(14.0);
                field_label(ui, "Начало");
                let begin_edit = slice_time_row(ui, &mut draft.begin);
                ui.add_space(10.0);
                field_label(ui, "Конец");
                let end_edit = slice_time_row(ui, &mut draft.end);
                if let Some(delta) = begin_edit.step {
                    let current = draft.begin.clone();
                    draft.begin = step_download_edge(&current, delta, &draft, true);
                } else if begin_edit.lost_focus {
                    let current = draft.begin.clone();
                    draft.begin = snap_download_edge(&current, &draft, true);
                }
                if let Some(delta) = end_edit.step {
                    let current = draft.end.clone();
                    draft.end = step_download_edge(&current, delta, &draft, false);
                } else if end_edit.lost_focus {
                    let current = draft.end.clone();
                    draft.end = snap_download_edge(&current, &draft, false);
                }
                ui.add_space(12.0);
                let (begin, end) = resolved_slice(&draft);
                let seconds = (end - begin).max(0);
                ui.label(
                    egui::RichText::new(format!(
                        "К скачиванию: {} · {}",
                        format_clip(&epoch_stamp(begin), &epoch_stamp(end)),
                        format_span(seconds)
                    ))
                    .color(TEXT),
                );
                ui.add_space(12.0);
                ui.checkbox(&mut self.download_mp4, "Сконвертировать в MP4")
                    .on_hover_text("Картинка копируется как есть, без перекодирования. Если в записи есть звук, он укладывается в AAC, чтобы файл открывался в обычных плеерах.");
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 8.0) / 2.0;
                    let accent = ui_accent(ui);
                    ui.scope(|ui| {
                        let ink = accent_ink(accent);
                        ui.visuals_mut().override_text_color = Some(ink);
                        ui.visuals_mut().widgets.inactive.fg_stroke.color = ink;
                        ui.visuals_mut().widgets.hovered.fg_stroke.color = ink;
                        ui.visuals_mut().widgets.hovered.bg_fill = accent;
                        ui.visuals_mut().widgets.active.fg_stroke.color = ink;
                        if ui
                            .add_sized([width, 36.0], egui::Button::new("Скачать").fill(accent))
                            .clicked()
                        {
                            confirm = true;
                        }
                    });
                    if ui
                        .add_sized([width, 36.0], egui::Button::new("Отмена"))
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });
        if confirm {
            let (begin, end) = resolved_slice(&draft);
            let recordings = self.recordings.lock().unwrap().clone();
            let covering = recordings.iter().find(|item| {
                matches!(
                    (stamp_epoch(&item.begin), stamp_epoch(&item.end)),
                    (Some(file_begin), Some(file_end))
                        if begin >= file_begin && end <= file_end
                )
            });
            let (path, bytes) = if let Some(file) = covering {
                (
                    file.path.clone(),
                    slice_bytes(file.bytes, &file.begin, &file.end, begin, end),
                )
            } else {
                *self.download_path.lock().unwrap() = draft.path.clone();
                (String::new(), overlap_bytes(&recordings, begin, end))
            };
            self.download_draft = None;
            self.download_recording(Recording {
                channel: draft.channel,
                begin: epoch_stamp(begin),
                end: epoch_stamp(end),
                path,
                bytes,
            });
        } else if cancel {
            self.download_draft = None;
        } else {
            self.download_draft = Some(draft);
        }
    }

    fn page_archive(
        &mut self,
        ui: &mut egui::Ui,
        devices: &[LiveDevice],
        _recordings: &[Recording],
        bare: bool,
    ) {
        let rect = if bare {
            ui.max_rect()
        } else {
            ui.max_rect().shrink(8.0)
        };
        let frame_radius = if bare { 0.0 } else { CELL_RADIUS };
        ui.painter().rect_filled(rect, frame_radius, VIDEO);
        let content_top = if bare {
            rect.top()
        } else {
            let header = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 32.0));
            ui.painter().rect_filled(
                header,
                egui::CornerRadius {
                    nw: 8,
                    ne: 8,
                    sw: 0,
                    se: 0,
                },
                HEADER,
            );
            let device = devices.iter().find(|item| item.id == self.archive_device);
            let mut title = device
                .map(|item| {
                    let camera = channel_title(Some(item), self.archive_channel);
                    if devices.len() > 1 {
                        format!("{} · {camera}", item.display_name())
                    } else {
                        camera
                    }
                })
                .unwrap_or_else(|| "Архив".into());
            if let Some(at) = self.archive_clock() {
                title = format!("{title} · {}", format_mark(at, &self.archive_from));
            }
            paint_row_text(ui, header, &title, TEXT);
            header.bottom()
        };
        let scrubber = self.archive_playing.as_ref().map(|_| {
            egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 48.0), rect.max)
        });
        let video = egui::Rect::from_min_max(
            egui::pos2(rect.left(), content_top),
            scrubber
                .map(|strip| egui::pos2(rect.right(), strip.top()))
                .unwrap_or(rect.max),
        );
        let bottom = if bare || scrubber.is_some() { 0 } else { 8 };
        if let Some(texture) = &self.archive_texture {
            let fitted = place_video(
                video,
                texture.size()[0] as f32,
                texture.size()[1] as f32,
                if bare { Aspect::Original } else { self.aspect },
            );
            paint_picture(
                ui,
                fitted.intersect(video),
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: bottom,
                    se: bottom,
                },
                texture,
            );
        } else {
            let message = if devices.is_empty() {
                "Добавьте регистратор"
            } else if _recordings.is_empty() {
                "За выбранный период записей нет"
            } else {
                "Выберите файл справа или нажмите на шкалу"
            };
            ui.painter().text(
                video.center(),
                egui::Align2::CENTER_CENTER,
                message,
                egui::FontId::proportional(15.0),
                MUTED,
            );
        }
        let stage = ui.interact(video, egui::Id::new("archive-stage"), egui::Sense::click());
        if stage.double_clicked() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(!bare));
        }
        stage.on_hover_text(if bare {
            "Двойной щелчок или Esc — обычный размер"
        } else {
            "Двойной щелчок — на весь экран"
        });
        if let Some(strip) = scrubber {
            self.paint_scrubber(ui, strip, bare);
        }
    }

    fn paint_scrubber(&mut self, ui: &mut egui::Ui, rect: egui::Rect, bare: bool) {
        let Some(clip) = self.archive_playing.clone() else {
            return;
        };
        let (Some(begin), Some(end)) = (stamp_epoch(&clip.begin), stamp_epoch(&clip.end)) else {
            return;
        };
        let end = end.max(begin + 1);
        let corner = if bare { 0 } else { 8 };
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius {
                nw: 0,
                ne: 0,
                sw: corner,
                se: corner,
            },
            HEADER,
        );
        let track = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 16.0, rect.bottom() - 18.0),
            egui::pos2(rect.right() - 16.0, rect.bottom() - 12.0),
        );
        let span = end - begin;
        let response = ui.interact(
            rect,
            egui::Id::new("archive-scrub"),
            egui::Sense::click_and_drag(),
        );
        let pointer = ui.input(|input| input.pointer.interact_pos().or(input.pointer.latest_pos()));
        let clock_now = self.archive_clock().unwrap_or(begin);
        if response.dragged() {
            if let Some(pos) = pointer {
                let at = at_from_x(track, begin, span, pos.x).clamp(begin, end);
                self.seek_preview = Some((at, Instant::now(), clock_now));
            }
        }
        if response.clicked() {
            if let Some(pos) = pointer {
                let at = at_from_x(track, begin, span, pos.x).clamp(begin, end);
                self.seek_preview = Some((at, Instant::now(), clock_now));
                *self.archive.seek_to.lock().unwrap() = Some(epoch_stamp(at));
            }
        }
        if response.drag_stopped() {
            if let Some((at, _, origin)) = self.seek_preview {
                *self.archive.seek_to.lock().unwrap() = Some(epoch_stamp(at));
                self.seek_preview = Some((at, Instant::now(), origin));
            }
        }
        if !response.dragged() {
            if let Some((preview, at, origin)) = self.seek_preview {
                let arrived = self.archive_clock().is_some_and(|time| {
                    (time - preview).abs() <= 2 && (time - origin).abs() > 0
                });
                if arrived || at.elapsed() > Duration::from_secs(8) {
                    self.seek_preview = None;
                }
            }
        }
        let playhead = self
            .seek_preview
            .map(|(time, _, _)| time)
            .or_else(|| self.archive_clock())
            .unwrap_or(begin)
            .clamp(begin, end);
        ui.painter().rect_filled(track, 3.0, TILE);
        let accent = ui_accent(ui);
        let head_x = scale_at(track, begin, span, playhead);
        let filled = egui::Rect::from_min_max(track.min, egui::pos2(head_x, track.bottom()));
        if filled.width() > 0.0 {
            ui.painter().rect_filled(filled, 3.0, accent);
        }
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(head_x, track.center().y),
                egui::vec2(2.0, 16.0),
            ),
            1.0,
            TEXT,
        );
        let label_y = rect.top() + 16.0;
        ui.painter().text(
            egui::pos2(track.left(), label_y),
            egui::Align2::LEFT_CENTER,
            format_mark(begin, &clip.begin),
            egui::FontId::proportional(12.0),
            MUTED,
        );
        ui.painter().text(
            egui::pos2(track.center().x, label_y),
            egui::Align2::CENTER_CENTER,
            format_mark(playhead, &clip.begin),
            egui::FontId::proportional(13.0),
            TEXT,
        );
        ui.painter().text(
            egui::pos2(track.right(), label_y),
            egui::Align2::RIGHT_CENTER,
            format_mark(end, &clip.begin),
            egui::FontId::proportional(12.0),
            MUTED,
        );
        if response.hovered() {
            if let Some(pos) = pointer {
                let hover_at = at_from_x(track, begin, span, pos.x).clamp(begin, end);
                let hover_x = scale_at(track, begin, span, hover_at);
                ui.painter().vline(
                    hover_x,
                    track.y_range(),
                    egui::Stroke::new(1.0, MUTED),
                );
                response.on_hover_text(format!("Перемотать на {}", format_mark(hover_at, &clip.begin)));
            }
        }
    }

    fn page_journal(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(22))
            .show(ui, |ui| {
                ui.label(accent_label(ui, with_icon(ph::CLIPBOARD_TEXT, "Журнал"), 22.0));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "События из журнала регистратора: тревоги камер, запись, диск и система.",
                    )
                    .color(MUTED),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    polish_toolbar(ui);
                    if ui
                        .button(ph::ARROW_LEFT)
                        .on_hover_text("Сутки назад")
                        .clicked()
                    {
                        self.step_journal(-1);
                    }
                    let label = format_range_label(&self.journal_from, &self.journal_to);
                    if ui
                        .button(with_icon(ph::CALENDAR_BLANK, &label))
                        .on_hover_text("Выбрать дату и время")
                        .clicked()
                    {
                        self.open_journal_picker();
                    }
                    if ui
                        .button(ph::ARROW_RIGHT)
                        .on_hover_text("Сутки вперёд")
                        .clicked()
                    {
                        self.step_journal(1);
                    }
                    let today = today_day();
                    let full_today = self.journal_from == format!("{today} 00:00:00")
                        && self.journal_to == format!("{today} 23:59:59");
                    if !full_today && ui.button("Сегодня").clicked() {
                        self.journal_from = format!("{today} 00:00:00");
                        self.journal_to = format!("{today} 23:59:59");
                        self.search_journal();
                    }
                    if ui.button(with_icon(ph::MAGNIFYING_GLASS, "Найти")).clicked() {
                        self.search_journal();
                    }
                    ui.add_space(8.0);
                    egui::ComboBox::from_id_salt("journal-filter")
                        .selected_text(journal_filter_label(self.journal_filter))
                        .width(168.0)
                        .show_ui(ui, |ui| {
                            for filter in [1, 0, 2] {
                                ui.selectable_value(
                                    &mut self.journal_filter,
                                    filter,
                                    journal_filter_label(filter),
                                );
                            }
                        });
                });
                ui.add_space(12.0);
                let rows = self.journal.lock().unwrap().clone();
                let shown: Vec<&JournalRow> = rows
                    .iter()
                    .filter(|row| journal_row_visible(row, self.journal_filter))
                    .collect();
                if self.journal_busy.load(Ordering::Acquire) && rows.is_empty() {
                    ui.label(egui::RichText::new("Читаю журнал регистраторов…").color(MUTED));
                    return;
                }
                if shown.is_empty() {
                    let text = if rows.is_empty() {
                        "За выбранный период событий нет"
                    } else {
                        "Таких событий нет. В списке выше можно показать все записи журнала."
                    };
                    ui.label(egui::RichText::new(text).color(MUTED));
                    return;
                }
                journal_header(ui);
                ui.add_space(4.0);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for row in shown {
                        journal_line(ui, row);
                    }
                });
            });
    }

    fn step_journal(&mut self, delta: i64) {
        let Some(from) = shift_stamp(&self.journal_from, delta) else {
            return;
        };
        let Some(to) = shift_stamp(&self.journal_to, delta) else {
            return;
        };
        self.journal_from = from;
        self.journal_to = to;
        self.search_journal();
    }

    fn open_journal_picker(&mut self) {
        self.picker_journal = true;
        self.picker_from = self.journal_from.clone();
        self.picker_to = self.journal_to.clone();
        self.picker_editing_from = true;
        self.follow_picker_month();
        self.sync_time_fields();
        self.archive_picker = true;
    }

    fn page_later(&mut self, ui: &mut egui::Ui, title: &str, text: &str) {
        ui.add_space(28.0);
        ui.label(accent_label(ui, with_icon(later_icon(title), title), 22.0));
        ui.add_space(10.0);
        ui.set_max_width(640.0);
        ui.label(egui::RichText::new(text).color(MUTED));
        ui.add_space(16.0);
        if ui.button(with_icon(ph::HOUSE, "На главную")).clicked() {
            self.set_page(Page::Home);
        }
    }
}

fn read_config_bundle(device: &LiveDevice) -> config::ConfigBundle {
    let mut bundle = config::ConfigBundle::empty();
    let mut session = match Session::connect(&device.host, device.port, &device.user, &device.password) {
        Ok(session) => session,
        Err(err) => {
            bundle.note = err.to_string();
            return bundle;
        }
    };
    match session.channel_titles() {
        Ok(titles) => bundle.titles = titles,
        Err(err) if !unsupported(&err) => bundle.note = format!("имена каналов: {err}"),
        Err(_) => {}
    }
    let mut notes = Vec::new();
    for name in [
        "Simplify.Encode",
        "AVEnc.Encode",
        "Detect.MotionDetect",
        "Detect.BlindDetect",
        "Detect.LossDetect",
        "Record",
        "General.General",
        "General.Location",
        "AVEnc.VideoWidget",
        "NetWork.NetCommon",
        "NetWork.NetDHCP",
        "NetWork.NetDNS",
        "NetWork.NetNTP",
        "System.TimeZone",
    ] {
        pull_named(&mut session, name, &mut bundle, &mut notes);
    }
    let cameras = bundle.titles.len().clamp(1, 32);
    bundle.camera = Vec::with_capacity(cameras);
    for index in 0..cameras {
        let name = format!("Camera.Param.[{index}]");
        match session.config_get(&name) {
            Ok(value) => bundle.camera.push(Some(value)),
            Err(err) if unsupported(&err) => {
                if index == 0 {
                    break;
                }
                bundle.camera.push(None);
            }
            Err(err) => {
                notes.push(format!("{name}: {err}"));
                bundle.camera.push(None);
            }
        }
    }
    match session.query_time() {
        Ok(time) => {
            bundle.time_at = Some(std::time::Instant::now());
            bundle.time = time;
        }
        Err(err) if !unsupported(&err) => notes.push(format!("время: {err}")),
        Err(_) => {}
    }
    match session.users() {
        Ok(users) => bundle.users = users,
        Err(err) if !unsupported(&err) => notes.push(format!("пользователи: {err}")),
        Err(_) => {}
    }
    match session.system_query("StorageInfo") {
        Ok(storage) => bundle.storage = storage,
        Err(err) if !unsupported(&err) => notes.push(format!("диск: {err}")),
        Err(_) => {}
    }
    if let Some(net) = bundle.values.get("NetWork.NetCommon") {
        bundle.net_host = config::hex_to_ip(&config::object_string(net, "HostIP")).unwrap_or_default();
        bundle.net_mask = config::hex_to_ip(&config::object_string(net, "Submask")).unwrap_or_default();
        bundle.net_gate = config::hex_to_ip(&config::object_string(net, "GateWay")).unwrap_or_default();
    }
    if let Some(dns) = bundle.values.get("NetWork.NetDNS") {
        bundle.net_dns = config::hex_to_ip(&config::object_string(dns, "Address")).unwrap_or_default();
        bundle.net_dns2 =
            config::hex_to_ip(&config::object_string(dns, "SpareAddress")).unwrap_or_default();
    }
    if !notes.is_empty() {
        bundle.note = notes.join(" · ");
    }
    bundle
}

fn pull_named(
    session: &mut Session,
    name: &str,
    bundle: &mut config::ConfigBundle,
    notes: &mut Vec<String>,
) {
    match session.config_get(name) {
        Ok(value) => {
            bundle.values.insert(name.to_string(), value);
        }
        Err(err) if unsupported(&err) => {}
        Err(err) => notes.push(format!("{name}: {err}")),
    }
}

fn unsupported(err: &xm_nvr::Error) -> bool {
    matches!(err, xm_nvr::Error::Device { code: 607, .. })
}

fn apply_config(
    section: config::ConfigSection,
    device: &LiveDevice,
    bundle: &config::ConfigBundle,
    password: &str,
    clock: Option<ConfigClock>,
    user_index: usize,
    devices: &Arc<Mutex<Vec<LiveDevice>>>,
) -> Result<String, String> {
    let mut session = Session::connect(&device.host, device.port, &device.user, &device.password)
        .map_err(|err| err.to_string())?;
    match section {
        config::ConfigSection::Channels => {
            for (index, title) in bundle.titles.iter().enumerate() {
                let title = title.trim();
                if title.is_empty() {
                    continue;
                }
                session
                    .set_channel_title(index as u16, title)
                    .map_err(|err| err.to_string())?;
            }
            if let Some(name) = bundle.encode_name() {
                if let Some(value) = bundle.values.get(name) {
                    session.config_set(name, value).map_err(|err| err.to_string())?;
                }
            }
            if let Ok(mut list) = devices.lock() {
                if let Some(live) = list.iter_mut().find(|item| item.id == device.id) {
                    live.titles = bundle.titles.clone();
                }
            }
            Ok("Имена каналов записаны".into())
        }
        config::ConfigSection::Detect => {
            store_keys(
                &mut session,
                bundle,
                &["Detect.MotionDetect", "Detect.BlindDetect", "Detect.LossDetect"],
            )?;
            Ok("Детекция записана".into())
        }
        config::ConfigSection::Record => {
            store_keys(&mut session, bundle, &["Record"])?;
            Ok("Расписание записано".into())
        }
        config::ConfigSection::General => {
            store_keys(
                &mut session,
                bundle,
                &["General.General", "General.Location", "NetWork.NetNTP", "System.TimeZone"],
            )?;
            if let Some(clock) = clock {
                session
                    .set_time(&running_clock(clock))
                    .map_err(|err| err.to_string())?;
            }
            Ok("Общие настройки записаны".into())
        }
        config::ConfigSection::Picture => {
            store_keys(&mut session, bundle, &["AVEnc.VideoWidget"])?;
            for (index, camera) in bundle.camera.iter().enumerate() {
                if let Some(value) = camera {
                    session
                        .config_set(&format!("Camera.Param.[{index}]"), value)
                        .map_err(|err| err.to_string())?;
                }
            }
            Ok("Картинка записана".into())
        }
        config::ConfigSection::Network => {
            let mut values = bundle.values.clone();
            if let Some(net) = values.get_mut("NetWork.NetCommon") {
                write_ip(net, "HostIP", &bundle.net_host)?;
                write_ip(net, "Submask", &bundle.net_mask)?;
                write_ip(net, "GateWay", &bundle.net_gate)?;
            }
            if let Some(dns) = values.get_mut("NetWork.NetDNS") {
                write_ip(dns, "Address", &bundle.net_dns)?;
                write_ip(dns, "SpareAddress", &bundle.net_dns2)?;
            }
            for name in ["NetWork.NetCommon", "NetWork.NetDHCP", "NetWork.NetDNS"] {
                if let Some(value) = values.get(name) {
                    session.config_set(name, value).map_err(|err| err.to_string())?;
                }
            }
            Ok("Сеть записана. Если адрес сменился, откройте регистратор заново.".into())
        }
        config::ConfigSection::Users => {
            let mut users = bundle.users.clone();
            if !password.is_empty() {
                let hash = sofia_hash(password);
                let Some(user) = users.as_array_mut().and_then(|items| items.get_mut(user_index)) else {
                    return Err("пользователь не выбран".into());
                };
                config::set_object_string(user, "Password", hash);
            }
            session.set_users(&users).map_err(|err| err.to_string())?;
            Ok("Пользователи записаны".into())
        }
        config::ConfigSection::Disk | config::ConfigSection::Maintain => {
            Err("для этого раздела нет общей записи".into())
        }
    }
}

fn store_keys(session: &mut Session, bundle: &config::ConfigBundle, names: &[&str]) -> Result<(), String> {
    for name in names {
        if let Some(value) = bundle.values.get(*name) {
            session.config_set(name, value).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn write_ip(value: &mut serde_json::Value, key: &str, text: &str) -> Result<(), String> {
    let Some(hex) = config::ip_to_hex(text) else {
        return Err(format!("адрес {text} не разобран"));
    };
    config::set_object_string(value, key, hex);
    Ok(())
}

fn config_encode_block(ui: &mut egui::Ui, item: &mut serde_json::Value, key: &str, title: &str) {
    let Some(block) = item.get_mut(key) else {
        return;
    };
    let Some(video) = block.get_mut("Video") else {
        return;
    };
    ui.label(accent_label(ui, title, 15.0));
    ui.add_space(6.0);
    labeled_config_text(ui, video, "Compression", "Кодек", 140.0);
    labeled_config_text(ui, video, "Resolution", "Разрешение", 140.0);
    config_int_row(ui, video, "FPS", "Кадры", 1, 30);
    config_int_row(ui, video, "BitRate", "Битрейт", 32, 20_000);
    labeled_config_text(ui, video, "BitRateControl", "Тип", 80.0);
    ui.add_space(8.0);
}

fn config_detect_block(
    ui: &mut egui::Ui,
    bundle: &mut config::ConfigBundle,
    name: &str,
    title: &str,
    channel: usize,
    region: bool,
) {
    let Some(item) = bundle
        .values
        .get_mut(name)
        .and_then(|value| value.as_array_mut())
        .and_then(|items| items.get_mut(channel))
    else {
        return;
    };
    ui.label(accent_label(ui, title, 15.0));
    ui.add_space(6.0);
    let mut enable = config::object_bool(item, "Enable");
    if ui.checkbox(&mut enable, "Включено").changed() {
        config::set_object_bool(item, "Enable", enable);
    }
    if item.get("Level").is_some() {
        config_int_row(ui, item, "Level", "Чувствительность", 1, 6);
    }
    if let Some(handler) = item.get_mut("EventHandler") {
        let mut record = config::object_bool(handler, "RecordEnable");
        let mut log = config::object_bool(handler, "LogEnable");
        if ui.checkbox(&mut record, "Писать видео").changed() {
            config::set_object_bool(handler, "RecordEnable", record);
        }
        if ui.checkbox(&mut log, "Писать в журнал").changed() {
            config::set_object_bool(handler, "LogEnable", log);
        }
    }
    if region {
        if let Some(grid) = item.get_mut("Region") {
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Область. Клетка включает зону.").color(MUTED));
            paint_region(ui, grid);
        }
    }
    ui.add_space(14.0);
}

fn paint_region(ui: &mut egui::Ui, region: &mut serde_json::Value) {
    let Some(rows) = region.as_array_mut() else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    let cell = 9.0;
    let cols = 32usize;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(cols as f32 * cell, rows.len() as f32 * cell),
        egui::Sense::click(),
    );
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let column = ((pos.x - rect.left()) / cell) as usize;
            let row = ((pos.y - rect.top()) / cell) as usize;
            if row < rows.len() && column < cols {
                if let Some(text) = rows[row].as_str() {
                    rows[row] = serde_json::json!(config::toggle_region_bit(text, column));
                }
            }
        }
    }
    let accent = ui_accent(ui);
    for (row, value) in rows.iter().enumerate() {
        let bits = config::region_row(value.as_str().unwrap_or("0"));
        for column in 0..cols {
            let on = bits & (1 << column) != 0;
            let cell_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left() + column as f32 * cell, rect.top() + row as f32 * cell),
                egui::vec2(cell - 1.0, cell - 1.0),
            );
            ui.painter()
                .rect_filled(cell_rect, 1.0, if on { accent } else { TILE });
        }
    }
}

fn blend_pair(ui: &mut egui::Ui, widget: &mut serde_json::Value, key: &str, title: &str) {
    let Some(attr) = widget.get_mut(key) else {
        return;
    };
    ui.label(title);
    let mut preview = config::object_bool(attr, "PreviewBlend");
    let mut encode = config::object_bool(attr, "EncodeBlend");
    if ui.checkbox(&mut preview, "В просмотре").changed() {
        config::set_object_bool(attr, "PreviewBlend", preview);
    }
    if ui.checkbox(&mut encode, "В записи").changed() {
        config::set_object_bool(attr, "EncodeBlend", encode);
    }
}

fn hex_flag(ui: &mut egui::Ui, value: &mut serde_json::Value, key: &str, label: &str) {
    let current = config::object_string(value, key);
    if current != "0x00000000" && current != "0x00000001" {
        return;
    }
    let mut on = current == "0x00000001";
    if ui.checkbox(&mut on, label).changed() {
        config::set_object_string(
            value,
            key,
            if on { "0x00000001" } else { "0x00000000" }.into(),
        );
    }
}

const LOCATION_LANGUAGES: &[(&str, &str)] = &[
    ("English", "English"),
    ("Russian", "Русский"),
    ("Ukrainian", "Українська"),
    ("SimpChinese", "中文 (упр.)"),
    ("TradChinese", "中文 (трад.)"),
    ("French", "Français"),
    ("German", "Deutsch"),
    ("Spanish", "Español"),
    ("Italian", "Italiano"),
    ("Portugal", "Português"),
    ("Polish", "Polski"),
    ("Czech", "Čeština"),
    ("Turkish", "Türkçe"),
    ("Arabic", "العربية"),
    ("Farsi", "فارسی"),
    ("Hebrew", "עברית"),
    ("Japanese", "日本語"),
    ("Korean", "한국어"),
    ("Thai", "ไทย"),
    ("Vietnamese", "Tiếng Việt"),
];

const LOCATION_VIDEO: &[(&str, &str)] = &[("PAL", "PAL"), ("NTSC", "NTSC")];

const VIDEO_OUTPUT: &[(&str, &str)] = &[
    ("Auto", "Авто"),
    ("VGA", "VGA"),
    ("HDMI", "HDMI"),
];

const LOCATION_DATE: &[(&str, &str)] = &[
    ("DDMMYY", "ДД-ММ-ГГ"),
    ("MMDDYY", "ММ-ДД-ГГ"),
    ("YYMMDD", "ГГ-ММ-ДД"),
];

const LOCATION_TIME: &[(&str, &str)] = &[("24", "24 часа"), ("12", "12 часов")];

const LOCATION_SEPARATOR: &[(&str, &str)] = &[("-", "-"), ("/", "/"), (".", ".")];

fn config_choice(
    ui: &mut egui::Ui,
    value: &mut serde_json::Value,
    key: &str,
    label: &str,
    options: &[(&str, &str)],
) {
    if value.get(key).is_none() {
        return;
    }
    let mut current = config::object_string(value, key);
    let known = options.iter().any(|(token, _)| *token == current);
    let selected = options
        .iter()
        .find(|(token, _)| *token == current)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| current.clone());
    ui.horizontal(|ui| {
        tune_inputs(ui);
        ui.label(label);
        egui::ComboBox::from_id_salt(("config-choice", key))
            .selected_text(selected)
            .width(220.0)
            .show_ui(ui, |ui| {
                tune_menu(ui);
                if !known && !current.is_empty() {
                    let extra = current.clone();
                    ui.selectable_value(&mut current, extra.clone(), extra);
                }
                for (token, name) in options {
                    ui.selectable_value(&mut current, (*token).to_string(), *name);
                }
            });
    });
    config::set_object_string(value, key, current);
}

fn labeled_config_text(ui: &mut egui::Ui, value: &mut serde_json::Value, key: &str, label: &str, width: f32) {
    if value.get(key).is_none() {
        return;
    }
    let mut text = config::object_string(value, key);
    ui.horizontal(|ui| {
        ui.label(label);
        if text_field(ui, &mut text, false, width, "").changed() {
            config::set_object_string(value, key, text);
        }
    });
}

fn labeled_draft(ui: &mut egui::Ui, value: &mut String, label: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        text_field(ui, value, false, 160.0, "");
    });
}

const XM_TIMEZONES: &[(i64, &str)] = &[
    (12 * 60, "[UTC-12:00] — линия перемены дат"),
    (11 * 60, "[UTC-11:00] — Самоа"),
    (10 * 60, "[UTC-10:00] — Гавайи"),
    (9 * 60, "[UTC-09:00] — Аляска"),
    (8 * 60, "[UTC-08:00] — тихоокеанское время"),
    (7 * 60, "[UTC-07:00] — горное время"),
    (6 * 60, "[UTC-06:00] — центральное время"),
    (5 * 60, "[UTC-05:00] — восточное время"),
    (4 * 60, "[UTC-04:00] — атлантическое время"),
    (210, "[UTC-03:30] — Ньюфаундленд"),
    (3 * 60, "[UTC-03:00] — Буэнос-Айрес, Бразилиа"),
    (2 * 60, "[UTC-02:00] — среднеатлантическое"),
    (60, "[UTC-01:00] — Азорские острова"),
    (0, "[UTC+00:00] — Лондон, Лиссабон"),
    (-1 * 60, "[UTC+01:00] — Берлин, Париж, Рим"),
    (-2 * 60, "[UTC+02:00] — Киев, Хельсинки, Каир"),
    (-3 * 60, "[UTC+03:00] — Москва"),
    (-210, "[UTC+03:30] — Тегеран"),
    (-4 * 60, "[UTC+04:00] — Баку, Тбилиси, Дубай"),
    (-270, "[UTC+04:30] — Кабул"),
    (-5 * 60, "[UTC+05:00] — Екатеринбург, Ташкент, Астана"),
    (-330, "[UTC+05:30] — Индия"),
    (-345, "[UTC+05:45] — Катманду"),
    (-6 * 60, "[UTC+06:00] — Омск, Дакка"),
    (-390, "[UTC+06:30] — Янгон"),
    (-7 * 60, "[UTC+07:00] — Бангкок, Красноярск"),
    (-8 * 60, "[UTC+08:00] — Пекин, Сингапур, Иркутск"),
    (-9 * 60, "[UTC+09:00] — Токио, Якутск"),
    (-570, "[UTC+09:30] — Аделаида"),
    (-10 * 60, "[UTC+10:00] — Сидней, Владивосток"),
    (-11 * 60, "[UTC+11:00] — Магадан"),
    (-12 * 60, "[UTC+12:00] — Окленд, Фиджи"),
    (-13 * 60, "[UTC+13:00] — Тонга"),
];

fn bias_label(bias: i64) -> String {
    let offset = -bias;
    let sign = if offset < 0 { '-' } else { '+' };
    let abs = offset.abs();
    format!("[UTC{sign}{:02}:{:02}]", abs / 60, abs % 60)
}

fn config_timezone(ui: &mut egui::Ui, zone: &mut serde_json::Value) {
    if zone.get("timeMin").is_none() {
        return;
    }
    let mut bias = config::object_i64(zone, "timeMin");
    let selected = XM_TIMEZONES
        .iter()
        .find(|(value, _)| *value == bias)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| bias_label(bias));
    ui.horizontal(|ui| {
        tune_inputs(ui);
        ui.label("Часовой пояс");
        let width = (ui.available_width() - 4.0).clamp(240.0, 460.0);
        egui::ComboBox::from_id_salt("config-timezone")
            .selected_text(selected)
            .width(width)
            .show_ui(ui, |ui| {
                tune_menu(ui);
                if !XM_TIMEZONES.iter().any(|(value, _)| *value == bias) {
                    let extra = bias;
                    ui.selectable_value(&mut bias, extra, bias_label(extra));
                }
                for (value, name) in XM_TIMEZONES {
                    ui.selectable_value(&mut bias, *value, *name);
                }
            });
    });
    config::set_object_i64(zone, "timeMin", bias);
}

fn config_int_row(ui: &mut egui::Ui, value: &mut serde_json::Value, key: &str, label: &str, min: i64, max: i64) {
    if value.get(key).is_none() {
        return;
    }
    let mut number = config::object_i64(value, key);
    ui.horizontal(|ui| {
        tune_inputs(ui);
        ui.label(label);
        if ui
            .add(
                egui::DragValue::new(&mut number)
                    .range(min..=max)
                    .speed(1.0),
            )
            .changed()
        {
            config::set_object_i64(value, key, number);
        }
    });
}

fn record_mode_label(mode: &str) -> &'static str {
    match mode {
        "ConfigRecord" => "По расписанию",
        "ManualRecord" => "Вручную",
        "ClosedRecord" => "Выключена",
        _ => "Как в регистраторе",
    }
}

fn overwrite_label(mode: &str) -> &'static str {
    match mode {
        "OverWrite" => "Перезаписывать",
        "StopRecord" => "Остановить запись",
        _ => "Как в регистраторе",
    }
}

fn confirm_title(confirm: ConfigConfirm) -> &'static str {
    match confirm {
        ConfigConfirm::Network => "Записать сеть",
        ConfigConfirm::Users => "Записать пользователей",
        ConfigConfirm::Reboot => "Перезагрузка",
        ConfigConfirm::Format { .. } => "Форматирование",
    }
}

fn confirm_text(confirm: ConfigConfirm, device: &str) -> String {
    match confirm {
        ConfigConfirm::Network => {
            "Регистратор может пропасть из программы, если изменятся адрес или порт.".into()
        }
        ConfigConfirm::Users => {
            "Пароль будет заменён на регистраторе. Пустое поле оставляет текущий.".into()
        }
        ConfigConfirm::Reboot => format!("{device} перезагрузится и на время пропадёт из просмотра."),
        ConfigConfirm::Format { .. } => {
            format!("Записи на разделе {device} будут стёрты. Это нельзя отменить.")
        }
    }
}

fn home_groups() -> &'static [(&'static str, &'static [(&'static str, Page)])] {
    &[
        (
            "Основные",
            &[
                ("Просмотр", Page::Live),
                ("Архив", Page::Archive),
                ("Журнал", Page::Journal),
            ],
        ),
        (
            "Настройки",
            &[
                ("Устройства", Page::Device),
                ("Конфигуратор", Page::Config),
                ("Программа", Page::Settings),
            ],
        ),
    ]
}

fn today_day() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn format_day(day: &str) -> String {
    let Some(year) = day.get(0..4) else {
        return day.to_string();
    };
    let Ok(month) = day.get(5..7).unwrap_or("").parse::<usize>() else {
        return day.to_string();
    };
    let Ok(date) = day.get(8..10).unwrap_or("").parse::<u32>() else {
        return day.to_string();
    };
    let name = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ]
    .get(month.wrapping_sub(1))
    .copied()
    .unwrap_or("");
    if name.is_empty() {
        day.to_string()
    } else {
        format!("{date} {name} {year}")
    }
}

fn running_clock(clock: ConfigClock) -> String {
    let elapsed = i64::try_from(clock.at.elapsed().as_secs()).unwrap_or(0);
    epoch_stamp(clock.epoch.saturating_add(elapsed))
}

fn epoch_stamp(epoch: i64) -> String {
    let days = epoch.div_euclid(86400);
    let mut secs = epoch.rem_euclid(86400) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = secs / 3600;
    secs %= 3600;
    let minute = secs / 60;
    let second = secs % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

fn parse_stamp(text: &str) -> Option<(i32, u32, u32, u32, u32, u32)> {
    if text.len() < 19 {
        return None;
    }
    let year = text.get(0..4)?.parse().ok()?;
    let month = text.get(5..7)?.parse().ok()?;
    let day = text.get(8..10)?.parse().ok()?;
    let hour = text.get(11..13)?.parse().ok()?;
    let minute = text.get(14..16)?.parse().ok()?;
    let second = text.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 59
    {
        return None;
    }
    Some((year, month, day, hour, minute, second))
}

fn stamp_epoch(text: &str) -> Option<i64> {
    let (year, month, day, hour, minute, second) = parse_stamp(text)?;
    Some(
        days_from_civil(year, month, day) * 86400
            + i64::from(hour) * 3600
            + i64::from(minute) * 60
            + i64::from(second),
    )
}

fn wall_epoch(time: WallTime) -> i64 {
    days_from_civil(i32::from(time.year), u32::from(time.month), u32::from(time.day)) * 86400
        + i64::from(time.hour) * 3600
        + i64::from(time.minute) * 60
        + i64::from(time.second)
}

fn prepare_range(from: &str, to: &str) -> Result<(String, String), &'static str> {
    let (Some(mut start), Some(mut end)) = (stamp_epoch(from), stamp_epoch(to)) else {
        return Err("Укажите дату и время");
    };
    if end < start {
        std::mem::swap(&mut start, &mut end);
    }
    if end - start > MAX_RANGE_SECS {
        end = start + MAX_RANGE_SECS;
    }
    if end <= start {
        end = start + 60;
    }
    Ok((epoch_stamp(start), epoch_stamp(end)))
}

fn normalize_range(from: &str, to: &str, day: &str) -> (String, String) {
    if let Ok(range) = prepare_range(from, to) {
        return range;
    }
    let day = if day.len() == 10 {
        day.to_string()
    } else {
        today_day()
    };
    (format!("{day} 00:00:00"), format!("{day} 23:59:59"))
}

fn shift_stamp(text: &str, delta_days: i64) -> Option<String> {
    let epoch = stamp_epoch(text)?;
    Some(epoch_stamp(epoch + delta_days * 86400))
}

fn format_range_label(from: &str, to: &str) -> String {
    let same_day = from.get(..10) == to.get(..10);
    if same_day {
        let day = format_day(from.get(..10).unwrap_or(""));
        let start = from.get(11..16).unwrap_or("");
        let end = to.get(11..16).unwrap_or("");
        return format!("{day}, {start}–{end}");
    }
    fn short(stamp: &str) -> String {
        let day = stamp
            .get(8..10)
            .and_then(|text| text.parse::<u32>().ok())
            .unwrap_or(0);
        let month = stamp
            .get(5..7)
            .and_then(|text| text.parse::<usize>().ok())
            .unwrap_or(1);
        let name = [
            "янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
        ]
        .get(month.wrapping_sub(1))
        .copied()
        .unwrap_or("");
        let hm = stamp.get(11..16).unwrap_or("");
        format!("{day} {name} {hm}")
    }
    format!("{} — {}", short(from), short(to))
}

fn format_mark(epoch: i64, anchor: &str) -> String {
    let stamp = epoch_stamp(epoch);
    if stamp.get(..10) == anchor.get(..10) {
        stamp.get(11..19).unwrap_or("").to_string()
    } else {
        format!(
            "{}.{} {}",
            stamp.get(8..10).unwrap_or(""),
            stamp.get(5..7).unwrap_or(""),
            stamp.get(11..19).unwrap_or("")
        )
    }
}

struct SliceEdit {
    step: Option<i64>,
    lost_focus: bool,
}

fn slice_time_row(ui: &mut egui::Ui, value: &mut String) -> SliceEdit {
    let mut step = None;
    let mut lost_focus = false;
    ui.horizontal(|ui| {
        let response = text_field(ui, value, false, 250.0, "2026-10-07 00:00:00");
        lost_focus = response.lost_focus();
        if ui
            .add_sized([32.0, 36.0], egui::Button::new(ph::MINUS))
            .on_hover_text("На секунду раньше")
            .clicked()
        {
            step = Some(-1);
        }
        if ui
            .add_sized([32.0, 36.0], egui::Button::new(ph::PLUS))
            .on_hover_text("На секунду позже")
            .clicked()
        {
            step = Some(1);
        }
    });
    SliceEdit { step, lost_focus }
}

fn snap_inside(edited: i64, other: i64, file_begin: i64, file_end: i64, editing_from: bool) -> i64 {
    let file_end = file_end.max(file_begin + 1);
    if editing_from {
        let latest = (other - 1).min(file_end - 1).max(file_begin);
        edited.clamp(file_begin, latest)
    } else {
        let earliest = (other + 1).max(file_begin + 1).min(file_end);
        edited.clamp(earliest, file_end)
    }
}

fn file_bounds(draft: &DownloadDraft) -> Option<(i64, i64)> {
    Some((
        stamp_epoch(&draft.limit_begin)?,
        stamp_epoch(&draft.limit_end)?,
    ))
}

fn overlap_bytes(recordings: &[Recording], begin: i64, end: i64) -> u64 {
    let mut total = 0u64;
    for item in recordings {
        let (Some(start), Some(stop)) = (stamp_epoch(&item.begin), stamp_epoch(&item.end)) else {
            continue;
        };
        let from = begin.max(start);
        let to = end.min(stop);
        if to <= from || item.bytes == 0 {
            continue;
        }
        let span = (stop - start).max(1);
        total += (item.bytes as u128 * (to - from) as u128 / span as u128) as u64;
    }
    total
}

fn snap_download_edge(text: &str, draft: &DownloadDraft, editing_from: bool) -> String {
    let Some((file_begin, file_end)) = file_bounds(draft) else {
        return text.to_string();
    };
    let other_text = if editing_from { &draft.end } else { &draft.begin };
    let other = stamp_epoch(other_text).unwrap_or(if editing_from { file_end } else { file_begin });
    let edited = stamp_epoch(text).unwrap_or(if editing_from { file_begin } else { file_end });
    epoch_stamp(snap_inside(
        edited,
        other,
        file_begin,
        file_end,
        editing_from,
    ))
}

fn step_download_edge(text: &str, delta: i64, draft: &DownloadDraft, editing_from: bool) -> String {
    let Some((file_begin, file_end)) = file_bounds(draft) else {
        return text.to_string();
    };
    let other_text = if editing_from { &draft.end } else { &draft.begin };
    let other = stamp_epoch(other_text).unwrap_or(if editing_from { file_end } else { file_begin });
    let edited = stamp_epoch(text).unwrap_or(if editing_from { file_begin } else { file_end });
    epoch_stamp(snap_inside(
        edited + delta,
        other,
        file_begin,
        file_end,
        editing_from,
    ))
}

fn resolved_slice(draft: &DownloadDraft) -> (i64, i64) {
    let file_begin = stamp_epoch(&draft.file_begin).unwrap_or(0);
    let file_end = stamp_epoch(&draft.file_end)
        .unwrap_or(file_begin + 1)
        .max(file_begin + 1);
    let begin = snap_inside(
        stamp_epoch(&draft.begin).unwrap_or(file_begin),
        stamp_epoch(&draft.end).unwrap_or(file_end),
        file_begin,
        file_end,
        true,
    );
    let end = snap_inside(
        stamp_epoch(&draft.end).unwrap_or(file_end),
        begin,
        file_begin,
        file_end,
        false,
    );
    (begin, end)
}

fn slice_bytes(file_bytes: u64, file_begin: &str, file_end: &str, begin: i64, end: i64) -> u64 {
    if file_bytes == 0 {
        return 0;
    }
    let Some(span) = stamp_epoch(file_end)
        .zip(stamp_epoch(file_begin))
        .map(|(stop, start)| (stop - start).max(1))
    else {
        return file_bytes;
    };
    let slice = (end - begin).max(0);
    (file_bytes as u128 * u128::from(slice as u64) / u128::from(span as u64)) as u64
}

fn journal_filter_label(filter: u8) -> &'static str {
    match filter {
        2 => "Система",
        0 => "Все записи",
        _ => "Камеры и тревоги",
    }
}

fn journal_row_visible(row: &JournalRow, filter: u8) -> bool {
    match filter {
        0 => true,
        2 => !log_is_camera(&row.kind),
        _ => log_is_camera(&row.kind),
    }
}

fn log_is_camera(kind: &str) -> bool {
    let kind = kind.to_ascii_lowercase();
    [
        "motion", "loss", "blind", "alarm", "event", "record", "video", "ipc", "human", "face",
        "cross", "intru", "trip", "audio", "scene", "abandon", "missing", "loiter", "park",
        "camera",
    ]
    .iter()
    .any(|needle| kind.contains(needle))
}

fn log_title(kind: &str) -> String {
    let key = kind.to_ascii_lowercase();
    let label = match key.as_str() {
        "eventstart" => "Начало события",
        "eventstop" => "Конец события",
        "videomotion" | "motiondetect" | "motiondetection" => "Движение",
        "videoloss" | "lossdetect" => "Потеря видео",
        "videoblind" | "blinddetect" => "Заслонение",
        "localalarm" | "alarmlocal" | "alarmin" | "localalram" => "Тревожный вход",
        "netalarm" | "alarmnet" => "Сетевая тревога",
        "ipcalarm" => "Тревога камеры",
        "recordstart" | "startrecord" => "Начало записи",
        "recordstop" | "stoprecord" => "Конец записи",
        "storagefailure" | "storageerror" | "diskerror" | "diskfull" | "nodisk" => "Диск",
        "netabort" | "netdisconnect" => "Обрыв сети",
        "ipdisconnect" | "ipcdisconnect" => "Камера недоступна",
        "login" | "loginin" => "Вход",
        "logout" => "Выход",
        "reboot" => "Перезагрузка",
        "shutdown" | "shutdow" => "Выключение",
        "saveconfig" | "setconfig" | "config" => "Изменение настроек",
        "settime" => "Смена времени",
        _ if key.contains("motion") => "Движение",
        _ if key.contains("loss") => "Потеря видео",
        _ if key.contains("blind") => "Заслонение",
        _ if key.contains("alarm") => "Тревога",
        _ if key.contains("record") => "Запись",
        _ if key.contains("login") => "Вход",
        _ if key.contains("logout") => "Выход",
        _ if key.contains("reboot") => "Перезагрузка",
        _ if key.contains("disk") || key.contains("storage") => "Диск",
        _ if key.contains("config") => "Настройки",
        _ => "",
    };
    if label.is_empty() {
        if kind.is_empty() {
            "Событие".into()
        } else {
            kind.to_string()
        }
    } else {
        label.into()
    }
}

fn interpret_log(log: &DeviceLog) -> (String, Option<u16>, String) {
    let mut kind = log.kind.clone();
    let mut channel = None;
    let mut detail = String::new();
    let data = log.data.trim();
    if data.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(event) = value.get("Event").and_then(|item| item.as_str()) {
                if !event.is_empty() {
                    kind = event.to_string();
                }
            }
            channel = value
                .get("Channel")
                .and_then(|item| item.as_u64())
                .map(|item| item as u16);
            if let Some(text) = value.get("Status").and_then(|item| item.as_str()) {
                detail = text.to_string();
            }
        }
    } else if let Ok(number) = data.parse::<u16>() {
        if number <= 256 {
            channel = Some(number);
        }
    } else if !data.is_empty() {
        detail = data.to_string();
        channel = trailing_channel(data);
    }
    if !log.user.is_empty()
        && !log.user.eq_ignore_ascii_case("system")
        && !log.user.eq_ignore_ascii_case("admin")
        && detail.is_empty()
        && channel.is_none()
    {
        detail = log.user.clone();
    } else if !log.user.is_empty()
        && !log.user.eq_ignore_ascii_case("system")
        && !detail.is_empty()
        && !detail.contains(&log.user)
    {
        detail = format!("{} · {detail}", log.user);
    }
    (kind, channel, detail)
}

fn trailing_channel(data: &str) -> Option<u16> {
    let digits: String = data
        .chars()
        .rev()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let number = digits.parse::<u16>().ok()?;
    if number <= 256 { Some(number) } else { None }
}

fn journal_header(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        journal_cell(ui, 168.0, "Время", MUTED, false);
        journal_cell(ui, 150.0, "Устройство", MUTED, false);
        journal_cell(ui, 180.0, "Событие", MUTED, false);
        journal_cell(ui, 88.0, "Канал", MUTED, false);
        ui.label(egui::RichText::new("Подробности").color(MUTED).size(13.0));
    });
}

fn journal_line(ui: &mut egui::Ui, row: &JournalRow) {
    let channel = row
        .channel
        .map(|index| format!("{}", index + 1))
        .unwrap_or_else(|| "—".into());
    let detail = if row.detail.is_empty() { "—" } else { row.detail.as_str() };
    let event = log_title(&row.kind);
    let color = if log_is_camera(&row.kind) {
        ui_accent(ui)
    } else {
        TEXT
    };
    ui.horizontal(|ui| {
        journal_cell(ui, 168.0, &row.time, TEXT, false);
        journal_cell(ui, 150.0, &row.device, TEXT, false);
        journal_cell(ui, 180.0, &event, color, true);
        journal_cell(ui, 88.0, &channel, TEXT, false);
        ui.label(egui::RichText::new(detail).color(MUTED));
    });
}

fn journal_cell(ui: &mut egui::Ui, width: f32, text: &str, color: egui::Color32, strong: bool) {
    let mut label = egui::RichText::new(text).color(color);
    if strong {
        label = label.strong();
    }
    ui.allocate_ui_with_layout(
        egui::vec2(width, 22.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.label(label);
        },
    );
}

fn format_span(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds}\"")
    } else if seconds < 3600 {
        let minutes = seconds / 60;
        let rest = seconds % 60;
        if rest == 0 {
            format!("{minutes}'")
        } else {
            format!("{minutes}'{rest:02}\"")
        }
    } else {
        let hours = seconds / 3600;
        let minutes = (seconds / 60) % 60;
        let rest = seconds % 60;
        if rest == 0 {
            format!("{hours}ч {minutes:02}'")
        } else {
            format!("{hours}ч {minutes:02}'{rest:02}\"")
        }
    }
}

fn format_clip(begin: &str, end: &str) -> String {
    if begin.get(..10) == end.get(..10) {
        format!(
            "{}–{}",
            begin.get(11..19).unwrap_or(begin),
            end.get(11..19).unwrap_or(end)
        )
    } else {
        format!("{begin} — {end}")
    }
}

fn paint_download_bar(ui: &mut egui::Ui, fraction: Option<f32>) {
    let width = (ui.available_width() - 24.0).max(40.0);
    let (outer, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), egui::Sense::hover());
    let rect = egui::Rect::from_min_size(
        egui::pos2(outer.left() + 12.0, outer.top()),
        egui::vec2(width.min(outer.width() - 12.0), 4.0),
    );
    ui.painter().rect_filled(rect, 2.0, TILE);
    let accent = ui_accent(ui);
    let fill = match fraction {
        Some(part) => rect.with_max_x(rect.left() + rect.width() * part),
        None => {
            let shift = (ui.input(|input| input.time).fract() as f32) * (rect.width() + 80.0);
            let left = (rect.left() + shift - 80.0).max(rect.left());
            let right = (rect.left() + shift).min(rect.right());
            egui::Rect::from_min_max(egui::pos2(left, rect.top()), egui::pos2(right, rect.bottom()))
        }
    };
    if fill.width() > 0.0 {
        ui.painter().rect_filled(fill, 2.0, accent);
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} ГБ", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} МБ", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} КБ", bytes as f64 / 1024.0)
    } else if bytes == 0 {
        "—".into()
    } else {
        format!("{bytes} Б")
    }
}

fn speed_label(speed: u8) -> &'static str {
    match speed {
        2 => "2×",
        4 => "4×",
        8 => "8×",
        16 => "16×",
        _ => "1×",
    }
}

fn normalize_speed(speed: u8) -> u8 {
    match speed {
        2 | 4 | 8 | 16 => speed,
        _ => 1,
    }
}

fn download_dir() -> PathBuf {
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let dir = std::env::var_os(home)
        .map(PathBuf::from)
        .map(|home| home.join("Downloads").join("Observer"))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.join("downloads")))
        })
        .unwrap_or_else(|| PathBuf::from("downloads"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn open_external(target: &std::ffi::OsStr) -> std::io::Result<Child> {
    if cfg!(windows) {
        if Path::new(target).is_dir() {
            Command::new("explorer").arg(target).spawn()
        } else {
            Command::new("cmd").args(["/C", "start", ""]).arg(target).spawn()
        }
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(target).spawn()
    } else {
        Command::new("xdg-open").arg(target).spawn()
    }
}

fn download_stem(recording: &Recording) -> String {
    let stamp: String = recording
        .begin
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    format!("ch{:02}-{stamp}", recording.channel + 1)
}

fn sniff_saved(path: &Path) -> Option<Codec> {
    let mut file = File::open(path).ok()?;
    let mut buf = [0u8; 4096];
    let count = file.read(&mut buf).ok()?;
    sniff_codec(&buf[..count])
}

fn pack_mp4(
    video: &Path,
    audio: Option<&Path>,
    sample_rate: u32,
    fps: u8,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let codec = sniff_saved(video).unwrap_or(Codec::H264);
    let format = match codec {
        Codec::H264 => "h264",
        Codec::H265 => "hevc",
        Codec::Mpeg4 | Codec::Jpeg => {
            return Err("этот кодек нельзя упаковать без перекодирования".into());
        }
    };
    let rate = fps.max(1).to_string();
    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-fflags".into(),
        "+genpts".into(),
        "-framerate".into(),
        rate,
        "-f".into(),
        format.into(),
        "-i".into(),
        video.display().to_string(),
    ];
    if let Some(pcm) = audio {
        args.extend([
            "-f".into(),
            "s16le".into(),
            "-ar".into(),
            sample_rate.to_string(),
            "-ac".into(),
            "1".into(),
            "-i".into(),
            pcm.display().to_string(),
        ]);
    }
    args.extend(["-c:v".into(), "copy".into()]);
    if audio.is_some() {
        args.extend([
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "128k".into(),
            "-ar".into(),
            "48000".into(),
            "-ac".into(),
            "1".into(),
            "-shortest".into(),
        ]);
    }
    args.extend([
        "-movflags".into(),
        "+faststart".into(),
        output.display().to_string(),
    ]);
    let mut command = Command::new(ffmpeg_path());
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|err| {
        format!("не удалось запустить ffmpeg ({}): {err}", ffmpeg_path())
    })?;
    let mut stderr = child.stderr.take();
    let logged = thread::spawn(move || {
        let mut text = String::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    });
    let status = loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = logged.join();
            return Err("скачивание отменено".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(err) => return Err(err.to_string()),
        }
    };
    let log = logged.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let detail = log.trim().lines().last().unwrap_or("").chars().take(180).collect::<String>();
        if detail.is_empty() {
            Err("ffmpeg не собрал файл".into())
        } else {
            Err(detail)
        }
    }
}

fn snap_endpoint(edited: i64, other: i64, editing_from: bool) -> i64 {
    const MIN_GAP: i64 = 60;
    if editing_from {
        edited.clamp(other - MAX_RANGE_SECS, other - MIN_GAP)
    } else {
        edited.clamp(other + MIN_GAP, other + MAX_RANGE_SECS)
    }
}

fn tint(base: egui::Color32, accent: egui::Color32, percent: u16) -> egui::Color32 {
    let percent = percent.min(100);
    let mix = |from: u8, to: u8| -> u8 {
        ((u16::from(from) * (100 - percent) + u16::from(to) * percent) / 100) as u8
    };
    egui::Color32::from_rgb(
        mix(base.r(), accent.r()),
        mix(base.g(), accent.g()),
        mix(base.b(), accent.b()),
    )
}

fn endpoint_card(ui: &mut egui::Ui, title: &str, stamp: &str, selected: bool, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 86.0), egui::Sense::click());
    let accent = ui_accent(ui);
    let stroke = if selected {
        egui::Stroke::new(1.5, accent)
    } else if response.hovered() {
        egui::Stroke::new(1.0, accent)
    } else {
        egui::Stroke::new(1.0, LINE)
    };
    ui.painter().rect_filled(rect, 8.0, TILE);
    ui.painter()
        .rect_stroke(rect, 8.0, stroke, egui::StrokeKind::Inside);
    let text = rect.shrink(14.0);
    ui.painter().text(
        text.left_top(),
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::proportional(12.0),
        if selected { accent } else { MUTED },
    );
    ui.painter().text(
        egui::pos2(text.left(), text.top() + 22.0),
        egui::Align2::LEFT_TOP,
        format_day(stamp.get(..10).unwrap_or("")),
        egui::FontId::proportional(15.0),
        TEXT,
    );
    ui.painter().text(
        egui::pos2(text.left(), text.top() + 46.0),
        egui::Align2::LEFT_TOP,
        stamp.get(11..16).unwrap_or(""),
        egui::FontId::proportional(14.0),
        if selected { TEXT } else { MUTED },
    );
    response.clicked()
}

fn calendar_cell(rect: egui::Rect, cell_w: f32, row_h: f32, week: usize, col: usize) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(
            rect.left() + col as f32 * cell_w,
            rect.top() + 28.0 + week as f32 * row_h,
        ),
        egui::vec2(cell_w, row_h),
    )
}

fn time_box(ui: &mut egui::Ui, value: &mut String, id: &str) -> egui::Response {
    ui.add(
        egui::TextEdit::singleline(value)
            .id(egui::Id::new(id))
            .desired_width(48.0)
            .font(egui::FontId::proportional(15.0))
            .horizontal_align(egui::Align::Center)
            .vertical_align(egui::Align::Center)
            .margin(egui::Margin::symmetric(4, 8))
            .min_size(egui::vec2(48.0, 36.0))
            .char_limit(2),
    )
}

fn shift_month(year: &mut i32, month: &mut u32, delta: i32) {
    let mut index = *month as i32 - 1 + delta;
    let mut next_year = *year;
    while index < 0 {
        index += 12;
        next_year -= 1;
    }
    while index > 11 {
        index -= 12;
        next_year += 1;
    }
    *year = next_year;
    *month = index as u32 + 1;
}

fn month_title(month: u32) -> &'static str {
    [
        "Январь",
        "Февраль",
        "Март",
        "Апрель",
        "Май",
        "Июнь",
        "Июль",
        "Август",
        "Сентябрь",
        "Октябрь",
        "Ноябрь",
        "Декабрь",
    ]
    .get(month.wrapping_sub(1) as usize)
    .copied()
    .unwrap_or("")
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let start = days_from_civil(year, month, 1);
    let next = if month == 12 {
        days_from_civil(year + 1, 1, 1)
    } else {
        days_from_civil(year, month + 1, 1)
    };
    (next - start).max(0) as u32
}

fn weekday_monday(year: i32, month: u32, day: u32) -> i64 {
    (days_from_civil(year, month, day) + 3).rem_euclid(7)
}

fn scale_at(scale: egui::Rect, start: i64, span: i64, at: i64) -> f32 {
    let t = (at - start) as f32 / span.max(1) as f32;
    scale.left() + scale.width() * t.clamp(0.0, 1.0)
}

fn at_from_x(scale: egui::Rect, start: i64, span: i64, x: f32) -> i64 {
    let t = ((x - scale.left()) / scale.width().max(1.0)).clamp(0.0, 1.0);
    start + (t * span as f32).round() as i64
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = (year - era * 400) as u64;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * u64::from(month_prime) + 2) / 5 + u64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era as i64 - 719468
}

fn civil_from_days(mut z: i64) -> (i32, u32, u32) {
    z += 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let day_of_era = (z - era * 146097) as u64;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 { month_prime + 3 } else { month_prime - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u32, day as u32)
}

fn paint_sidebar_joints(ctx: &egui::Context, bottom: f32) {
    let screen = ctx.screen_rect();
    let content = egui::Rect::from_min_max(
        egui::pos2(screen.left(), screen.top() + TOP_BAR),
        egui::pos2(screen.right() - SIDEBAR, screen.bottom() - bottom),
    );
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("sidebar-joints"),
    ));
    paint_joint(&painter, content.right_top(), egui::vec2(-1.0, 1.0));
    paint_joint(&painter, content.right_bottom(), egui::vec2(-1.0, -1.0));
}

fn paint_joint(painter: &egui::Painter, corner: egui::Pos2, into: egui::Vec2) {
    let center = corner + into * CELL_RADIUS;
    painter.rect_filled(egui::Rect::from_two_pos(corner, center), 0.0, PANEL);
    painter.circle_filled(center, CELL_RADIUS, CANVAS);
}

fn paint_picture(
    ui: &egui::Ui,
    rect: egui::Rect,
    radius: egui::CornerRadius,
    texture: &egui::TextureHandle,
) {
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    ui.painter().with_clip_rect(rect).add(
        egui::epaint::RectShape::filled(rect, radius, egui::Color32::WHITE)
            .with_texture(texture.id(), uv),
    );
}

#[derive(PartialEq, Eq)]
enum TabHit {
    Open,
    Close,
}

fn page_label(page: Page) -> &'static str {
    match page {
        Page::Home => "Главная",
        Page::Live => "Просмотр",
        Page::Device => "Устройства",
        Page::Archive => "Архив",
        Page::Journal => "Журнал",
        Page::Settings => "Программа",
        Page::Config => "Конфигуратор",
        Page::Later(title, _) => title,
    }
}

fn tab(ui: &mut egui::Ui, icon: &str, text: &str, on: bool, closable: bool) -> Option<TabHit> {
    let color = if on { TEXT } else { MUTED };
    let icon_galley = ui.painter().layout_no_wrap(
        icon.to_owned(),
        egui::FontId::proportional(16.0),
        color,
    );
    let text_galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(14.0),
        color,
    );
    let pad = 14.0;
    let gap = 8.0;
    let close_slot = if closable { gap + 16.0 } else { 0.0 };
    let width = pad + icon_galley.size().x + gap + text_galley.size().x + close_slot + pad;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::click());
    let pointer = ui.input(|input| input.pointer.hover_pos());
    let close_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - pad - 8.0, rect.center().y),
        egui::vec2(18.0, 18.0),
    );
    let over_close = closable && pointer.is_some_and(|pos| close_rect.contains(pos));
    let fill = if on {
        accent_soft(ui_accent(ui))
    } else if response.hovered() && !over_close {
        TILE
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    if on {
        ui.painter().rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(1.0, ui_accent(ui)),
            egui::StrokeKind::Inside,
        );
    }
    let x = rect.left() + pad;
    let icon_size = icon_galley.size();
    let text_size = text_galley.size();
    ui.painter().galley(
        egui::pos2(x, rect.center().y - icon_size.y * 0.5),
        icon_galley,
        color,
    );
    ui.painter().galley(
        egui::pos2(x + icon_size.x + gap, rect.center().y - text_size.y * 0.5),
        text_galley,
        color,
    );
    if closable {
        if over_close {
            ui.painter().circle_filled(close_rect.center(), 9.0, HOVER);
        }
        let mark = ui.painter().layout_no_wrap(
            ph::X.to_owned(),
            egui::FontId::proportional(12.0),
            if over_close { TEXT } else { MUTED },
        );
        ui.painter().galley(
            close_rect.center() - mark.size() * 0.5,
            mark,
            if over_close { TEXT } else { MUTED },
        );
    }
    if !response.clicked() {
        return None;
    }
    let pos = response.interact_pointer_pos().unwrap_or(rect.center());
    if closable && close_rect.contains(pos) {
        Some(TabHit::Close)
    } else {
        Some(TabHit::Open)
    }
}

fn home_tile(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(196.0, 56.0), egui::Sense::click());
    let accent = ui_accent(ui);
    let hovered = response.hovered();
    ui.painter().rect_filled(rect, 8.0, if hovered { HOVER } else { TILE });
    ui.painter().rect_stroke(
        rect,
        8.0,
        egui::Stroke::new(1.0, if hovered { accent } else { LINE }),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.left_center() + egui::vec2(16.0, 0.0),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(18.0),
        accent,
    );
    ui.painter().text(
        rect.left_center() + egui::vec2(44.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(15.0),
        TEXT,
    );
    response
}


#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    host: String,
    port: String,
    user: String,
    password: String,
    devices: Vec<SavedDevice>,
    next_id: u64,
    day: String,
    #[serde(default)]
    archive_from: String,
    #[serde(default)]
    archive_to: String,
    archive_device: u64,
    archive_channel: u16,
    layout: u32,
    stream: String,
    hwaccel: bool,
    #[serde(default)]
    download_mp4: bool,
    #[serde(default)]
    door_call: bool,
    #[serde(default)]
    door_camera: Option<Source>,
    #[serde(default = "default_accent")]
    accent: [u8; 3],
    aspect: String,
    page: String,
    cells: Vec<Option<Source>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: "10.18.10.44".into(),
            port: "34567".into(),
            user: "admin".into(),
            password: String::new(),
            devices: Vec::new(),
            next_id: 1,
            day: "2026-10-06".into(),
            archive_from: String::new(),
            archive_to: String::new(),
            archive_device: 0,
            archive_channel: 0,
            layout: 16,
            stream: "extra".into(),
            hwaccel: false,
            download_mp4: false,
            door_call: false,
            door_camera: None,
            accent: default_accent(),
            aspect: "auto".into(),
            page: "home".into(),
            cells: Vec::new(),
        }
    }
}

impl Settings {
    fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(settings_path()) else {
            return Self::default();
        };
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Self::default();
        };
        migrate_settings(&mut value);
        let mut settings: Settings = serde_json::from_value(value).unwrap_or_default();
        if !matches!(settings.layout, 1 | 4 | 9 | 16 | 25 | 36 | 64) {
            settings.layout = 16;
        }
        if settings.devices.is_empty() && !settings.host.trim().is_empty() {
            settings.devices.push(SavedDevice {
                id: 1,
                name: settings.host.trim().to_string(),
                host: settings.host.trim().to_string(),
                port: {
                    let port = settings.port.trim();
                    if port.is_empty() {
                        "34567".into()
                    } else {
                        port.to_string()
                    }
                },
                user: {
                    let user = settings.user.trim();
                    if user.is_empty() {
                        "admin".into()
                    } else {
                        user.to_string()
                    }
                },
                password: settings.password.clone(),
                kind: Vendor::Xm,
            });
            settings.next_id = 2;
        }
        let max_id = settings.devices.iter().map(|device| device.id).max().unwrap_or(0);
        if settings.next_id <= max_id {
            settings.next_id = max_id + 1;
        }
        settings
    }
}

fn migrate_settings(value: &mut serde_json::Value) {
    let Some(cells) = value
        .as_object_mut()
        .and_then(|obj| obj.get_mut("cells"))
        .and_then(|item| item.as_array_mut())
    else {
        return;
    };
    for cell in cells {
        if let Some(channel) = cell.as_u64() {
            *cell = serde_json::json!({
                "device_id": 1,
                "channel": channel,
            });
        }
    }
}

fn watch_doors(
    gen: u64,
    generation: Arc<AtomicU64>,
    devices: Arc<Mutex<Vec<LiveDevice>>>,
    report: Arc<Mutex<DoorReport>>,
) {
    let mut tracks: Vec<(u64, DoorPhase, u8, String)> = Vec::new();
    while generation.load(Ordering::Acquire) == gen {
        let jobs: Vec<(u64, String, String, u16, String, String)> = {
            let devices = devices.lock().unwrap();
            devices
                .iter()
                .filter(|device| device.kind == Vendor::Hikvision && device.door && device.connected)
                .map(|device| {
                    (
                        device.id,
                        device.display_name(),
                        device.host.clone(),
                        device.port,
                        device.user.clone(),
                        device.password.clone(),
                    )
                })
                .collect()
        };
        let ids: Vec<u64> = jobs.iter().map(|job| job.0).collect();
        tracks.retain(|track| ids.contains(&track.0));
        for (id, name, host, port, user, password) in jobs {
            if generation.load(Ordering::Acquire) != gen {
                return;
            }
            let phase = match hik_call_status(&host, port, &user, &password) {
                Ok(DoorCall::Idle) => DoorPhase::Idle,
                Ok(DoorCall::Ring) => DoorPhase::Ring,
                Ok(DoorCall::Talk) => DoorPhase::Talk,
                Err(_) => {
                    if let Some(track) = tracks.iter_mut().find(|track| track.0 == id) {
                        track.2 = track.2.saturating_add(1);
                        if track.2 >= 3 {
                            track.1 = DoorPhase::Idle;
                        }
                        track.3 = name;
                    }
                    continue;
                }
            };
            if let Some(track) = tracks.iter_mut().find(|track| track.0 == id) {
                track.1 = phase;
                track.2 = 0;
                track.3 = name;
            } else {
                tracks.push((id, phase, 0, name));
            }
        }
        let active = tracks
            .iter()
            .find(|track| track.1 == DoorPhase::Talk)
            .or_else(|| tracks.iter().find(|track| track.1 == DoorPhase::Ring));
        let next = if let Some(track) = active {
            DoorReport {
                phase: track.1,
                device_id: track.0,
                name: track.3.clone(),
            }
        } else {
            DoorReport::default()
        };
        *report.lock().unwrap() = next;
        for _ in 0..8 {
            if generation.load(Ordering::Acquire) != gen {
                return;
            }
            thread::sleep(Duration::from_millis(125));
        }
    }
}

fn device_channel_count(device: &LiveDevice) -> u16 {
    if device.channels > 0 {
        device.channels
    } else {
        device.titles.len() as u16
    }
}

fn channel_link<'a>(device: &'a LiveDevice, channel: u16) -> &'a str {
    device
        .links
        .get(channel as usize)
        .map(String::as_str)
        .unwrap_or("")
}

fn link_is_live(status: &str) -> bool {
    status == "Connected"
}

fn channel_available(device: &LiveDevice, channel: u16) -> bool {
    let status = channel_link(device, channel);
    status.is_empty() || link_is_live(status)
}

fn live_channel_count(device: &LiveDevice) -> usize {
    (0..device_channel_count(device))
        .filter(|channel| link_is_live(channel_link(device, *channel)))
        .count()
}

fn link_color(status: &str, placed: bool) -> egui::Color32 {
    if !status.is_empty() && status != "Connected" {
        return egui::Color32::from_rgb(196, 86, 78);
    }
    if !placed || status != "Connected" {
        return MUTED;
    }
    egui::Color32::from_rgb(86, 176, 112)
}

fn link_hint(status: &str) -> &'static str {
    match status {
        "Connected" => "Камера на связи",
        "Offline" => "Нет сигнала",
        "NoConfig" => "Камера не добавлена",
        "NoLogin" | "LoginFailed" => "Камера не входит",
        "" => "Состояние ещё не получено",
        _ => "Камера не работает",
    }
}

fn list_hint(status: &str, placed: bool) -> String {
    let health = link_hint(status);
    if placed {
        health.to_string()
    } else if status.is_empty() || status == "Connected" {
        "Не выведена в ячейку".into()
    } else {
        format!("{health}\nНе выведена в ячейку")
    }
}

fn channel_title(device: Option<&LiveDevice>, channel: u16) -> String {
    device
        .and_then(|item| item.titles.get(channel as usize))
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| format!("Канал {}", channel + 1))
}

fn settings_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("xm-settings.json")))
        .unwrap_or_else(|| PathBuf::from("xm-settings.json"))
}

fn page_name(page: Page) -> String {
    match page {
        Page::Home => "home",
        Page::Live => "live",
        Page::Device => "device",
        Page::Archive => "archive",
        Page::Journal => "journal",
        Page::Settings => "settings",
        Page::Config => "config",
        Page::Later(_, _) => "home",
    }
    .into()
}

fn page_from_name(name: &str) -> Page {
    match name {
        "live" => Page::Live,
        "device" => Page::Device,
        "archive" => Page::Archive,
        "journal" => Page::Journal,
        "settings" => Page::Settings,
        "config" => Page::Config,
        _ => Page::Home,
    }
}

fn stream_name(stream: StreamKind) -> &'static str {
    match stream {
        StreamKind::Main => "main",
        StreamKind::Extra => "extra",
    }
}

fn stream_from_name(name: &str) -> StreamKind {
    match name {
        "main" | "Main" => StreamKind::Main,
        _ => StreamKind::Extra,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Aspect {
    Auto,
    Original,
    R4x3,
    R16x9,
}

fn aspect_name(aspect: Aspect) -> &'static str {
    match aspect {
        Aspect::Auto => "Авто",
        Aspect::Original => "Оригинал",
        Aspect::R4x3 => "4:3",
        Aspect::R16x9 => "16:9",
    }
}

fn aspect_from_name(name: &str) -> Aspect {
    match name {
        "original" | "Оригинал" => Aspect::Original,
        "4:3" => Aspect::R4x3,
        "16:9" => Aspect::R16x9,
        _ => Aspect::Auto,
    }
}

fn aspect_name_saved(aspect: Aspect) -> &'static str {
    match aspect {
        Aspect::Auto => "auto",
        Aspect::Original => "original",
        Aspect::R4x3 => "4:3",
        Aspect::R16x9 => "16:9",
    }
}

fn swapped_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if index == to {
        from
    } else {
        index
    }
}

fn grid_shape(layout: u32) -> (usize, usize) {
    let side = match layout {
        1 => 1,
        4 => 2,
        9 => 3,
        16 => 4,
        25 => 5,
        36 => 6,
        64 => 8,
        _ => 4,
    };
    (side, side)
}

fn place_video(bounds: egui::Rect, width: f32, height: f32, aspect: Aspect) -> egui::Rect {
    match aspect {
        Aspect::Auto => bounds,
        Aspect::Original => fit_rect(bounds, width, height),
        Aspect::R4x3 => fit_rect(bounds, 4.0, 3.0),
        Aspect::R16x9 => fit_rect(bounds, 16.0, 9.0),
    }
}

fn fit_rect(bounds: egui::Rect, width: f32, height: f32) -> egui::Rect {
    if width <= 1.0 || height <= 1.0 {
        return bounds;
    }
    let scale = (bounds.width() / width).min(bounds.height() / height);
    egui::Rect::from_center_size(bounds.center(), egui::vec2(width * scale, height * scale))
}

fn refresh_slot(
    ctx: &egui::Context,
    slot: &StreamSlot,
    seen: &mut u64,
    texture: &mut Option<egui::TextureHandle>,
    name: &str,
) -> bool {
    let snapshot = {
        let picture = slot.picture.lock().unwrap();
        if picture.seq == 0 {
            *seen = 0;
            *texture = None;
            return false;
        }
        if picture.seq == *seen || picture.rgba.is_empty() {
            return false;
        }
        (
            picture.seq,
            picture.width,
            picture.height,
            Arc::clone(&picture.rgba),
        )
    };
    let (seq, width, height, rgba) = snapshot;
    if width == 0 || height == 0 || rgba.len() != width * height * 4 {
        return false;
    }
    let image = egui::ColorImage::from_rgba_premultiplied([width, height], &rgba);
    let options = egui::TextureOptions::LINEAR;
    if let Some(texture) = texture.as_mut() {
        texture.set(image, options);
    } else {
        *texture = Some(ctx.load_texture(name, image, options));
    }
    *seen = seq;
    true
}

enum DoorGesture {
    None,
    Open,
    Close,
}

fn mix_rgb(from: egui::Color32, to: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    egui::Color32::from_rgb(mix(from.r(), to.r()), mix(from.g(), to.g()), mix(from.b(), to.b()))
}

fn paint_tracked(
    painter: &egui::Painter,
    center: egui::Pos2,
    text: &str,
    size: f32,
    color: egui::Color32,
    tracking: f32,
) {
    let font = egui::FontId::proportional(size);
    let mut widths = Vec::new();
    let mut total = 0.0;
    for ch in text.chars() {
        let width = painter
            .layout_no_wrap(ch.to_string(), font.clone(), color)
            .size()
            .x;
        widths.push(width);
        total += width + tracking;
    }
    if !widths.is_empty() {
        total -= tracking;
    }
    let mut x = center.x - total * 0.5;
    for (ch, width) in text.chars().zip(widths) {
        painter.text(
            egui::pos2(x, center.y),
            egui::Align2::LEFT_CENTER,
            ch.to_string(),
            font.clone(),
            color,
        );
        x += width + tracking;
    }
}

fn paint_slide_mark(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.6, color);
    let left = center + egui::vec2(-3.2, 0.0);
    painter.line_segment([left + egui::vec2(0.0, -5.0), center + egui::vec2(2.2, 0.0)], stroke);
    painter.line_segment([left + egui::vec2(0.0, 5.0), center + egui::vec2(2.2, 0.0)], stroke);
}

fn vendor_name(kind: Vendor) -> &'static str {
    match kind {
        Vendor::Xm => "Xiongmai",
        Vendor::Hikvision => "Hikvision",
    }
}

fn device_model(device: &LiveDevice) -> String {
    let model = device.hardware.trim();
    if model.is_empty() {
        "—".into()
    } else {
        model.to_string()
    }
}

fn device_status_text(device: &LiveDevice) -> String {
    if device.connected {
        "Онлайн".into()
    } else if !device.error.is_empty() {
        device.error.clone()
    } else {
        "Подключение…".into()
    }
}

fn device_columns(rect: egui::Rect) -> [egui::Rect; 5] {
    let weights = [0.24_f32, 0.20, 0.22, 0.14, 0.20];
    let mut columns = [rect; 5];
    let mut x = rect.left();
    for (index, weight) in weights.iter().enumerate() {
        let width = rect.width() * weight;
        columns[index] = egui::Rect::from_min_size(
            egui::pos2(x, rect.top()),
            egui::vec2(width, rect.height()),
        );
        x += width;
    }
    columns
}

fn paint_row_text(ui: &egui::Ui, rect: egui::Rect, text: &str, color: egui::Color32) {
    let inner = rect.shrink2(egui::vec2(10.0, 0.0));
    if inner.width() < 1.0 {
        return;
    }
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(14.0),
        color,
    );
    ui.painter().with_clip_rect(inner).galley(
        egui::pos2(inner.left(), inner.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
}

fn device_detail_rows(ui: &mut egui::Ui, device: &LiveDevice) {
    let live = live_channel_count(device);
    let rows = [
        (
            "Пользователь",
            if device.user.trim().is_empty() {
                "—".into()
            } else {
                device.user.clone()
            },
        ),
        ("Серийный номер", blank_dash(&device.serial)),
        ("Прошивка", blank_dash(&device.software)),
        (
            "Каналов",
            if device.connected {
                device.channels.to_string()
            } else {
                "ещё не получено".into()
            },
        ),
        (
            "На связи",
            if device.connected {
                live.to_string()
            } else {
                "—".into()
            },
        ),
        (
            "Домофон",
            if device.door {
                "замок доступен".into()
            } else {
                "нет".into()
            },
        ),
    ];
    for (label, value) in rows {
        ui.horizontal(|ui| {
            ui.add_sized(
                [150.0, 22.0],
                egui::Label::new(egui::RichText::new(label).color(MUTED)),
            );
            ui.label(value);
        });
    }
}

fn blank_dash(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        "—".into()
    } else {
        text.to_string()
    }
}

fn probe_device(
    status: Arc<Mutex<String>>,
    devices: Arc<Mutex<Vec<LiveDevice>>>,
    id: u64,
    kind: Vendor,
    name: String,
    host: String,
    port: u16,
    user: String,
    password: String,
) {
    if kind == Vendor::Hikvision {
        probe_hikvision(status, devices, id, name, host, port, user, password);
        return;
    }
    let mut session = match Session::connect(&host, port, &user, &password) {
        Ok(session) => session,
        Err(err) => {
            mark_device(&devices, id, err.to_string());
            *status.lock().unwrap() = format!("{name}: {err}");
            return;
        }
    };
    let info = match session.device_info() {
        Ok(info) => info,
        Err(err) => {
            mark_device(&devices, id, err.to_string());
            *status.lock().unwrap() = format!("{name}: {err}");
            return;
        }
    };
    let links = session.channel_links().unwrap_or_default();
    let mut channels = info.video_channels();
    if channels == 0 {
        channels = info.titles.len() as u16;
    }
    if channels == 0 {
        channels = 8;
    }
    let mut list = devices.lock().unwrap();
    let Some(device) = list.iter_mut().find(|device| device.id == id) else {
        return;
    };
    device.serial = info.serial;
    device.software = info.software;
    device.hardware = info.hardware;
    device.channels = channels;
    device.titles = info.titles;
    device.links = links;
    device.connected = true;
    device.error.clear();
    drop(list);
    *status.lock().unwrap() = format!("{name}: {channels} каналов");
}

fn probe_hikvision(
    status: Arc<Mutex<String>>,
    devices: Arc<Mutex<Vec<LiveDevice>>>,
    id: u64,
    name: String,
    host: String,
    port: u16,
    user: String,
    password: String,
) {
    let info = match xm_nvr::hik_describe(&host, port, &user, &password) {
        Ok(info) => info,
        Err(err) => {
            mark_device(&devices, id, err.to_string());
            *status.lock().unwrap() = format!("{name}: {err}");
            return;
        }
    };
    let channels = info.channels.len() as u16;
    let door = info.door;
    let mut list = devices.lock().unwrap();
    let Some(device) = list.iter_mut().find(|device| device.id == id) else {
        return;
    };
    device.serial = info.serial;
    device.software = info.firmware;
    device.hardware = info.model;
    device.channels = channels;
    device.titles = info.channels;
    device.links = vec!["Connected".to_string(); channels as usize];
    device.door = door;
    device.connected = true;
    device.error.clear();
    if device.name.trim().is_empty() && !info.name.is_empty() {
        device.name = info.name;
    }
    drop(list);
    *status.lock().unwrap() = if door {
        format!("{name}: домофон, {channels} кан.")
    } else {
        format!("{name}: {channels} каналов")
    };
}

fn run_hik_live(
    slot: Arc<StreamSlot>,
    generation: u64,
    status: Arc<Mutex<String>>,
    host: String,
    user: String,
    password: String,
    channel: u16,
    stream: StreamKind,
    hwaccel: bool,
) {
    if !slot.still(generation) {
        return;
    }
    let mut rtsp = match xm_nvr::HikStream::open(
        &host,
        &user,
        &password,
        channel + 1,
        stream == StreamKind::Main,
    ) {
        Ok(rtsp) => rtsp,
        Err(err) => {
            if slot.still(generation) {
                let text = err.to_string();
                *status.lock().unwrap() = if text.contains(":554:") {
                    format!("{text}. Для видео нужен открытый порт 554")
                } else {
                    text
                };
            }
            return;
        }
    };
    if let Ok(socket) = rtsp.clone_socket() {
        slot.arm(generation, socket);
    }
    let mut hw = hwaccel;
    let mut codec = rtsp.codec();
    let mut decoder = None;
    let mut primed = false;
    let mut resume_key = false;
    let mut fed = 0u32;
    let mut shown = false;
    while slot.still(generation) {
        let nal = match rtsp.read_nal() {
            Ok(nal) => nal,
            Err(_) => {
                if slot.still(generation) {
                    *status.lock().unwrap() = "поток Hikvision прервался".into();
                }
                break;
            }
        };
        if nal.codec != codec {
            codec = nal.codec;
            decoder = None;
            primed = false;
        }
        if !slot.awake.load(Ordering::Acquire) {
            resume_key = true;
            continue;
        }
        if (resume_key || !primed) && !nal.keyframe {
            continue;
        }
        if decoder.is_none() {
            match open_decoder(codec, &slot, &mut hw) {
                Ok(opened) => decoder = Some(opened),
                Err(err) => {
                    if slot.still(generation) {
                        *status.lock().unwrap() = err;
                    }
                    break;
                }
            }
        }
        if decoder.as_mut().unwrap().push(&nal.data).is_err() {
            if hw {
                hw = false;
                decoder = None;
                primed = false;
                continue;
            }
            if slot.still(generation) {
                *status.lock().unwrap() = "декодер остановился".into();
            }
            break;
        }
        fed += 1;
        if nal.keyframe {
            primed = true;
            resume_key = false;
        }
        if !shown && slot.picture.lock().unwrap().seq > 0 {
            shown = true;
        }
        if hw && !shown && fed > 80 {
            hw = false;
            decoder = None;
            primed = false;
            fed = 0;
        }
    }
}

fn mark_device(devices: &Mutex<Vec<LiveDevice>>, id: u64, message: String) {
    if let Some(device) = devices.lock().unwrap().iter_mut().find(|device| device.id == id) {
        device.connected = false;
        device.error = message;
    }
}

enum MediaJob {
    Live { channel: u16, stream: StreamKind },
    Play(PlaybackSpec),
}

fn run_media(
    slot: Arc<StreamSlot>,
    generation: u64,
    status: Arc<Mutex<String>>,
    announce: bool,
    host: String,
    port: u16,
    user: String,
    password: String,
    job: MediaJob,
    label: String,
    hwaccel: bool,
) {
    if !slot.still(generation) {
        return;
    }
    let mut hw = hwaccel;
    // Процесс декодера поднимается параллельно с входом на регистратор.
    let mut decoder = Decoder::spawn(Codec::H264, Arc::clone(&slot), hw).ok();
    let mut chosen = Some(Codec::H264);
    let mut session = match Session::connect(&host, port, &user, &password) {
        Ok(session) => session,
        Err(err) => {
            drop(decoder);
            if slot.still(generation) {
                *status.lock().unwrap() = err.to_string();
            }
            return;
        }
    };
    if let Ok(socket) = session.clone_socket() {
        slot.arm(generation, socket);
    }
    if !slot.still(generation) {
        return;
    }
    let started = match &job {
        MediaJob::Live { channel, stream } => session.start_live(*channel, *stream),
        MediaJob::Play(spec) => session.start_playback(spec),
    };
    if let Err(err) = started {
        if slot.still(generation) {
            *status.lock().unwrap() = err.to_string();
        }
        return;
    }
    let mut pushed = 0u32;
    let mut swapped = false;
    let mut shown = false;
    let mut primed = false;
    let mut resume_key = false;
    let mut asked_key = false;
    let mut applied_speed = 1u8;
    let mut forgive = 0u8;
    let live = match job {
        MediaJob::Live { channel, stream } => Some((channel, stream)),
        MediaJob::Play(_) => None,
    };
    while slot.still(generation) {
        let mut sent_rate = false;
        let mut sent_locate = false;
        if let MediaJob::Play(spec) = &job {
            if let Some(stamp) = slot.seek_to.lock().unwrap().take() {
                let mut located = spec.clone();
                located.begin = stamp;
                let _ = session.locate_playback(&located);
                forgive = forgive.saturating_add(1);
                sent_locate = true;
                primed = false;
                applied_speed = 1;
                pushed = 0;
                if let Some(decoder) = decoder.as_mut() {
                    decoder.kill();
                }
                decoder = None;
            }
            let want = normalize_speed(slot.play_speed.load(Ordering::Relaxed));
            if want != applied_speed {
                let _ = session.set_playback_rate(spec, want);
                applied_speed = want;
                sent_rate = true;
                forgive = forgive.saturating_add(1);
            }
        }
        match session.next_piece() {
            Ok(LivePiece::Audio(audio)) => {
                forward_audio(&slot, &audio);
                continue;
            }
            Ok(LivePiece::Video(frame)) => {
                if live.is_some() && !slot.awake.load(Ordering::Acquire) {
                    resume_key = true;
                    asked_key = false;
                    continue;
                }
                if resume_key && !frame.keyframe {
                    if !asked_key {
                        if let Some((channel, stream)) = live {
                            let _ = session.force_iframe(channel, stream);
                        }
                        asked_key = true;
                    }
                    continue;
                }
                resume_key = false;
                asked_key = false;
                if frame.codec == Codec::Jpeg || frame.data.is_empty() {
                    continue;
                }
                if !primed && !frame.keyframe {
                    continue;
                }
                if frame.keyframe && !swapped {
                    let sniffed = sniff_codec(&frame.data).unwrap_or(frame.codec);
                    if chosen != Some(sniffed) {
                        if let Some(decoder) = decoder.as_mut() {
                            decoder.kill();
                        }
                        decoder = None;
                        chosen = Some(sniffed);
                    }
                }
                if decoder.is_none() {
                    if !frame.keyframe {
                        continue;
                    }
                    let codec = chosen.unwrap_or(frame.codec);
                    match open_decoder(codec, &slot, &mut hw) {
                        Ok(next) => {
                            decoder = Some(next);
                            chosen = Some(codec);
                            pushed = 0;
                        }
                        Err(err) => {
                            *status.lock().unwrap() = format!("декодер: {err}");
                            return;
                        }
                    }
                }
                let push_failed = {
                    let decoder = decoder.as_mut().unwrap();
                    decoder.push(&frame.data).is_err()
                };
                if push_failed {
                    let detail = decoder
                        .as_ref()
                        .map(|item| item.error_text())
                        .unwrap_or_default();
                    if let Some(decoder) = decoder.as_mut() {
                        decoder.kill();
                    }
                    decoder = None;
                    if hw {
                        hw = false;
                        *status.lock().unwrap() =
                            "Видеокарта не взяла поток, декодирую на процессоре".into();
                        continue;
                    }
                    if !swapped {
                        swapped = true;
                        chosen = Some(alternate(chosen.unwrap_or(Codec::H264)));
                        if announce {
                            *status.lock().unwrap() = "Пробую другой кодек…".into();
                        }
                        continue;
                    }
                    *status.lock().unwrap() = if detail.is_empty() {
                        "декодер остановился".into()
                    } else {
                        format!("декодер: {detail}")
                    };
                    break;
                }
                pushed += 1;
                if frame.keyframe {
                    primed = true;
                }
                if let Some(time) = frame.time {
                    if slot.still(generation) {
                        *slot.clock.lock().unwrap() = Some(time);
                    }
                }
                let (seq, width, height) = {
                    let picture = slot.picture.lock().unwrap();
                    (picture.seq, picture.width, picture.height)
                };
                if announce && seq > 0 && !shown {
                    shown = true;
                    let codec_name = match chosen.unwrap_or(Codec::H264) {
                        Codec::H265 => "H.265",
                        Codec::Mpeg4 => "MPEG4",
                        Codec::Jpeg => "JPEG",
                        Codec::H264 => "H.264",
                    };
                    *status.lock().unwrap() = format!("{label} · {codec_name} {width}×{height}");
                }
                if pushed >= 80 && seq == 0 {
                    let detail = decoder
                        .as_ref()
                        .map(|item| item.error_text())
                        .unwrap_or_default();
                    if let Some(decoder) = decoder.as_mut() {
                        decoder.kill();
                    }
                    decoder = None;
                    if hw {
                        hw = false;
                        *status.lock().unwrap() =
                            "Видеокарта не взяла поток, декодирую на процессоре".into();
                        pushed = 0;
                        continue;
                    }
                    if !swapped {
                        swapped = true;
                        chosen = Some(alternate(chosen.unwrap_or(Codec::H264)));
                        if announce {
                            *status.lock().unwrap() = "Картинки нет, пробую другой кодек…".into();
                        }
                        pushed = 0;
                        continue;
                    }
                    *status.lock().unwrap() = if detail.is_empty() {
                        "поток идёт, картинки нет".into()
                    } else {
                        format!("поток идёт, картинки нет. {detail}")
                    };
                    break;
                }
            }
            Err(_) if !slot.still(generation) => break,
            Err(xm_nvr::Error::Device { .. }) if forgive > 0 => {
                forgive -= 1;
                if slot.still(generation) {
                    if sent_locate {
                        *status.lock().unwrap() = "Регистратор не перемотал на это место".into();
                    } else if sent_rate {
                        *status.lock().unwrap() = format!(
                            "Регистратор не принял скорость {}",
                            speed_label(applied_speed)
                        );
                    }
                }
                continue;
            }
            Err(err) => {
                if slot.still(generation) {
                    *status.lock().unwrap() = err.to_string();
                }
                break;
            }
        }
    }
    drop(decoder);
}

fn snapshot_dir() -> PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("snapshots")))
        .unwrap_or_else(|| PathBuf::from("snapshots"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn picture_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn save_jpg(path: &Path, width: usize, height: usize, rgba: &[u8]) -> std::io::Result<()> {
    let expected = width.saturating_mul(height).saturating_mul(4);
    if width == 0
        || height == 0
        || width > u32::MAX as usize
        || height > u32::MAX as usize
        || rgba.len() < expected
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "кадр повреждён",
        ));
    }
    let pixels = width * height;
    let mut rgb = Vec::with_capacity(pixels * 3);
    for pixel in rgba[..expected].chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }
    let file = File::create(path)?;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 100);
    encoder
        .encode(
            &rgb,
            width as u32,
            height as u32,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))
}

struct Decoder {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    errors: Arc<Mutex<String>>,
}

impl Decoder {
    fn spawn(codec: Codec, slot: Arc<StreamSlot>, hwaccel: bool) -> Result<Self, String> {
        let format = match codec {
            Codec::H265 => "hevc",
            Codec::Mpeg4 => "mpeg4",
            Codec::H264 | Codec::Jpeg => "h264",
        };
        let hw = hwaccel && matches!(codec, Codec::H264 | Codec::H265);
        let mut args = vec![
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-fpsprobesize",
            "0",
            "-probesize",
            "8192",
            "-analyzeduration",
            "0",
            "-threads",
            "1",
        ];
        if hw {
            args.extend([
                "-hwaccel",
                if cfg!(windows) { "d3d11va" } else { "auto" },
                "-hwaccel_flags",
                "allow_profile_mismatch",
            ]);
        }
        args.extend([
            "-f",
            format,
            "-i",
            "pipe:0",
            "-an",
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "rgba",
            "-f",
            "image2pipe",
            "-c:v",
            "pam",
            "pipe:1",
        ]);
        let mut command = Command::new(ffmpeg_path());
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().map_err(|err| {
            format!("не удалось запустить ffmpeg ({}): {err}", ffmpeg_path())
        })?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ffmpeg не отдал вывод".to_string())?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| "ffmpeg не отдал журнал".to_string())?;
        let errors = Arc::new(Mutex::new(String::new()));
        let error_slot = Arc::clone(&errors);
        thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            *error_slot.lock().unwrap() = text.chars().take(400).collect();
        });
        thread::spawn(move || {
            let mut input = BufReader::new(stdout);
            let mut seq = 0u64;
            while let Ok((width, height, rgba)) = read_pam_frame(&mut input) {
                seq += 1;
                let mut picture = slot.picture.lock().unwrap();
                picture.rgba = Arc::new(rgba);
                picture.width = width;
                picture.height = height;
                picture.seq = seq;
            }
        });
        Ok(Self {
            child,
            stdin,
            errors,
        })
    }

    fn push(&mut self, data: &[u8]) -> std::io::Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "декодер закрыт",
            ));
        };
        stdin.write_all(data)?;
        stdin.flush()?;
        Ok(())
    }

    fn error_text(&self) -> String {
        self.errors.lock().unwrap().clone()
    }

    fn kill(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.kill();
        let _ = self.child.wait();
    }
}

fn alternate(codec: Codec) -> Codec {
    match codec {
        Codec::H265 => Codec::H264,
        _ => Codec::H265,
    }
}

fn read_pam_frame(input: &mut impl BufRead) -> std::io::Result<(usize, usize, Vec<u8>)> {
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "поток кадров закончился",
            ));
        }
        if line.starts_with("P7") {
            break;
        }
    }
    let mut width = 0usize;
    let mut height = 0usize;
    let mut depth = 0usize;
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "заголовок кадра оборвался",
            ));
        }
        let trimmed = line.trim();
        if trimmed == "ENDHDR" {
            break;
        }
        let mut parts = trimmed.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("WIDTH"), Some(value)) => width = value.parse().unwrap_or(0),
            (Some("HEIGHT"), Some(value)) => height = value.parse().unwrap_or(0),
            (Some("DEPTH"), Some(value)) => depth = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    if width == 0 || height == 0 || width > 4096 || height > 4096 || (depth != 3 && depth != 4) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("кадр {width}x{height} depth {depth}"),
        ));
    }
    let mut raw = vec![0u8; width * height * depth];
    input.read_exact(&mut raw)?;
    if depth == 4 {
        return Ok((width, height, raw));
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in raw.chunks_exact(3) {
        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    Ok((width, height, rgba))
}

fn open_decoder(
    codec: Codec,
    slot: &Arc<StreamSlot>,
    hw: &mut bool,
) -> Result<Decoder, String> {
    let want = *hw && matches!(codec, Codec::H264 | Codec::H265);
    match Decoder::spawn(codec, Arc::clone(slot), want) {
        Ok(decoder) => Ok(decoder),
        Err(_) if want => {
            *hw = false;
            Decoder::spawn(codec, Arc::clone(slot), false)
        }
        Err(err) => Err(err),
    }
}

fn ffmpeg_path() -> String {
    let bundled = r"C:\ffmpeg\ffmpeg.exe";
    if Path::new(bundled).exists() {
        bundled.to_string()
    } else {
        "ffmpeg".to_string()
    }
}

enum PtzHit {
    Move(&'static str),
    Center,
}

fn zoom_column(pad: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(pad.right() - 46.0, pad.top() + 14.0),
        egui::vec2(34.0, 84.0),
    )
}

fn zoom_key(column: egui::Rect, top: bool) -> egui::Rect {
    let height = (column.height() - 8.0) * 0.5;
    if top {
        egui::Rect::from_min_size(column.min, egui::vec2(column.width(), height))
    } else {
        egui::Rect::from_min_size(
            egui::pos2(column.left(), column.top() + height + 8.0),
            egui::vec2(column.width(), height),
        )
    }
}

fn speed_track(pad: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(pad.left() + 6.0, pad.bottom() - 38.0),
        egui::pos2(pad.right() - 6.0, pad.bottom() - 8.0),
    )
}

fn ptz_hit(
    center: egui::Pos2,
    inner: f32,
    outer: f32,
    zoom: egui::Rect,
    speed: egui::Rect,
    pos: egui::Pos2,
) -> Option<PtzHit> {
    if speed.contains(pos) {
        return None;
    }
    if zoom_key(zoom, true).contains(pos) {
        return Some(PtzHit::Move("ZoomTile"));
    }
    if zoom_key(zoom, false).contains(pos) {
        return Some(PtzHit::Move("ZoomWide"));
    }
    let delta = pos - center;
    let distance = delta.length();
    if distance <= inner {
        return Some(PtzHit::Center);
    }
    if distance > outer + 8.0 {
        return None;
    }
    let angle = delta.y.atan2(delta.x);
    let dirs = [
        (0.0, "DirectionRight"),
        (std::f32::consts::FRAC_PI_2, "DirectionDown"),
        (std::f32::consts::PI, "DirectionLeft"),
        (-std::f32::consts::FRAC_PI_2, "DirectionUp"),
    ];
    let mut best = dirs[0].1;
    let mut best_gap = f32::MAX;
    for (mid, command) in dirs {
        let mut gap = (angle - mid).abs();
        if gap > std::f32::consts::PI {
            gap = std::f32::consts::TAU - gap;
        }
        if gap < best_gap {
            best_gap = gap;
            best = command;
        }
    }
    Some(PtzHit::Move(best))
}

fn ring_sector(center: egui::Pos2, inner: f32, outer: f32, mid: f32, span: f32) -> Vec<egui::Pos2> {
    let steps = 12;
    let start = mid - span * 0.5;
    let mut points = Vec::with_capacity((steps + 1) * 2);
    for index in 0..=steps {
        let angle = start + span * (index as f32) / (steps as f32);
        points.push(center + egui::vec2(angle.cos(), angle.sin()) * outer);
    }
    for index in (0..=steps).rev() {
        let angle = start + span * (index as f32) / (steps as f32);
        points.push(center + egui::vec2(angle.cos(), angle.sin()) * inner);
    }
    points
}

fn paint_chevron(painter: &egui::Painter, origin: egui::Pos2, direction: egui::Vec2, size: f32, color: egui::Color32) {
    let forward = direction.normalized();
    let side = egui::vec2(-forward.y, forward.x);
    let tip = origin + forward * size;
    let left = origin - forward * size * 0.45 + side * size * 0.75;
    let right = origin - forward * size * 0.45 - side * size * 0.75;
    let stroke = egui::Stroke::new(1.7, color);
    painter.line_segment([left, tip], stroke);
    painter.line_segment([right, tip], stroke);
}

fn paint_zoom_key(painter: &egui::Painter, rect: egui::Rect, hot: bool, active: bool, plus: bool) {
    let fill = if active {
        egui::Color32::from_rgb(28, 78, 86)
    } else if hot {
        egui::Color32::from_rgb(58, 66, 80)
    } else {
        egui::Color32::from_rgb(44, 50, 62)
    };
    painter.rect_filled(rect, 10.0, fill);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if plus {
            ph::MAGNIFYING_GLASS_PLUS
        } else {
            ph::MAGNIFYING_GLASS_MINUS
        },
        egui::FontId::proportional(16.0),
        egui::Color32::WHITE,
    );
}

struct PtzJob {
    host: String,
    port: u16,
    user: String,
    password: String,
    channel: u16,
    command: String,
    step: u8,
    halt: bool,
}

fn spawn_ptz_worker(status: Arc<Mutex<String>>) -> mpsc::Sender<PtzJob> {
    let (tx, rx) = mpsc::channel::<PtzJob>();
    thread::spawn(move || {
        let mut current: Option<(String, u16, String, Session)> = None;
        while let Ok(job) = rx.recv() {
            run_ptz_job(&mut current, &rx, job, &status);
        }
    });
    tx
}

fn run_ptz_job(
    current: &mut Option<(String, u16, String, Session)>,
    rx: &mpsc::Receiver<PtzJob>,
    mut job: PtzJob,
    status: &Mutex<String>,
) {
    loop {
        if !ensure_ptz_session(current, &job, status) {
            return;
        }
        if job.halt {
            let result = {
                let Some((_, _, _, session)) = current.as_mut() else {
                    return;
                };
                session.ptz(job.channel, &job.command, job.step, true)
            };
            if let Err(err) = result {
                *status.lock().unwrap() = format!("PTZ: {err}");
                *current = None;
            }
            return;
        }
        let started = {
            let Some((_, _, _, session)) = current.as_mut() else {
                return;
            };
            session.ptz(job.channel, &job.command, job.step, false)
        };
        if let Err(err) = started {
            *status.lock().unwrap() = format!("PTZ: {err}");
            *current = None;
            return;
        }
        let Ok(next) = rx.recv() else {
            if let Some((_, _, _, session)) = current.as_mut() {
                let _ = session.ptz(job.channel, &job.command, job.step, true);
            }
            return;
        };
        if let Some((_, _, _, session)) = current.as_mut() {
            let _ = session.ptz(job.channel, &job.command, job.step, true);
        }
        if next.halt {
            return;
        }
        job = next;
    }
}

fn ensure_ptz_session(
    current: &mut Option<(String, u16, String, Session)>,
    job: &PtzJob,
    status: &Mutex<String>,
) -> bool {
    let same = current.as_ref().is_some_and(|(host, port, user, _)| {
        host == &job.host && *port == job.port && user == &job.user
    });
    if same {
        return true;
    }
    match Session::connect(&job.host, job.port, &job.user, &job.password) {
        Ok(session) => {
            *current = Some((job.host.clone(), job.port, job.user.clone(), session));
            true
        }
        Err(err) => {
            *status.lock().unwrap() = format!("PTZ: {err}");
            *current = None;
            false
        }
    }
}

fn forward_audio(slot: &StreamSlot, audio: &AudioFrame) {
    let sender = slot.audio_out.lock().unwrap().clone();
    let Some(sender) = sender else {
        return;
    };
    let Some(chunk) = decode_xm_audio(audio) else {
        return;
    };
    let _ = sender.try_send(chunk);
}

fn decode_xm_audio(audio: &AudioFrame) -> Option<PcmChunk> {
    let rate = match audio.rate {
        1 => 4000,
        2 => 8000,
        3 => 11_025,
        4 => 16_000,
        5 => 20_000,
        6 => 22_050,
        7 => 32_000,
        8 => 44_100,
        9 => 48_000,
        _ => 8000,
    };
    let samples = match audio.codec {
        0x0E => audio.data.iter().copied().map(alaw_decode).collect(),
        0x0A => audio.data.iter().copied().map(ulaw_decode).collect(),
        _ => return None,
    };
    Some(PcmChunk { rate, samples })
}

fn alaw_decode(value: u8) -> i16 {
    let value = value ^ 0x55;
    let sign = value & 0x80;
    let exponent = (value & 0x70) >> 4;
    let mantissa = value & 0x0F;
    let mut sample = ((mantissa as i32) << 4) + 8;
    if exponent != 0 {
        sample += 0x100;
    }
    if exponent > 1 {
        sample <<= exponent - 1;
    }
    if sign == 0 {
        sample = -sample;
    }
    sample as i16
}

fn ulaw_decode(value: u8) -> i16 {
    let value = !value;
    let sign = value & 0x80;
    let exponent = (value & 0x70) >> 4;
    let mantissa = value & 0x0F;
    let mut sample = (((mantissa as i32) << 3) + 0x84) << exponent;
    sample -= 0x84;
    if sign != 0 {
        sample = -sample;
    }
    sample as i16
}

#[cfg(not(windows))]
fn play_audio(rx: mpsc::Receiver<PcmChunk>, status: Arc<Mutex<String>>) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let queue = Arc::new(Mutex::new(std::collections::VecDeque::<f32>::new()));
    let opened = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| "устройство вывода не найдено".to_string())
        .and_then(|device| {
            let config = device
                .default_output_config()
                .map_err(|err| format!("устройство вывода не открылось ({err})"))?;
            let format = config.sample_format();
            let config: cpal::StreamConfig = config.into();
            let rate = config.sample_rate;
            let stream = match format {
                cpal::SampleFormat::F32 => {
                    open_output::<f32>(&device, &config, queue.clone(), status.clone())
                }
                cpal::SampleFormat::I16 => {
                    open_output::<i16>(&device, &config, queue.clone(), status.clone())
                }
                cpal::SampleFormat::U16 => {
                    open_output::<u16>(&device, &config, queue.clone(), status.clone())
                }
                cpal::SampleFormat::I32 => {
                    open_output::<i32>(&device, &config, queue.clone(), status.clone())
                }
                other => Err(format!("формат вывода {other} не поддерживается")),
            }?;
            stream
                .play()
                .map_err(|err| format!("устройство вывода не запустилось ({err})"))?;
            Ok((stream, rate))
        });
    let (_stream, out_rate) = match opened {
        Ok(opened) => opened,
        Err(err) => {
            *status.lock().unwrap() = format!("Звук: {err}");
            return;
        }
    };
    let limit = out_rate as usize * 2 / 5;
    let mut phase = 0.0f64;
    while let Ok(chunk) = rx.recv() {
        if chunk.samples.is_empty() || chunk.rate == 0 {
            continue;
        }
        let step = chunk.rate as f64 / out_rate as f64;
        let last = chunk.samples.len() - 1;
        let mut queue = queue.lock().unwrap();
        while phase < chunk.samples.len() as f64 {
            let index = phase as usize;
            let frac = (phase - index as f64) as f32;
            let a = chunk.samples[index] as f32 / 32768.0;
            let b = chunk.samples[(index + 1).min(last)] as f32 / 32768.0;
            queue.push_back(a + (b - a) * frac);
            phase += step;
        }
        phase -= chunk.samples.len() as f64;
        if queue.len() > limit {
            let extra = queue.len() - limit;
            queue.drain(..extra);
        }
    }
}

#[cfg(not(windows))]
fn open_output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<Mutex<std::collections::VecDeque<f32>>>,
    status: Arc<Mutex<String>>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    use cpal::traits::DeviceTrait;

    let channels = config.channels.max(1) as usize;
    device
        .build_output_stream(
            config.clone(),
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let mut queue = queue.lock().unwrap();
                for frame in data.chunks_mut(channels) {
                    let value = T::from_sample(queue.pop_front().unwrap_or(0.0));
                    frame.fill(value);
                }
            },
            move |err: cpal::Error| {
                if !matches!(err.kind(), cpal::ErrorKind::Xrun) {
                    *status.lock().unwrap() = format!("Звук: {err}");
                }
            },
            None,
        )
        .map_err(|err| format!("устройство вывода не открылось ({err})"))
}

#[cfg(windows)]
fn play_audio(rx: mpsc::Receiver<PcmChunk>, status: Arc<Mutex<String>>) {
    let mut device: Option<WaveOut> = None;
    let mut failed = false;
    while let Ok(chunk) = rx.recv() {
        if failed {
            continue;
        }
        if device.as_ref().map(|item| item.rate) != Some(chunk.rate) {
            if let Some(previous) = device.take() {
                previous.close();
            }
            match WaveOut::open(chunk.rate) {
                Ok(opened) => device = Some(opened),
                Err(err) => {
                    *status.lock().unwrap() = format!("Звук: {err}");
                    failed = true;
                    continue;
                }
            }
        }
        if let Some(device) = device.as_mut() {
            device.write(&chunk.samples);
        }
    }
    if let Some(device) = device {
        device.close();
    }
}

#[cfg(windows)]
struct WaveOut {
    handle: *mut std::ffi::c_void,
    rate: u32,
    slots: [WaveSlot; 4],
}

#[cfg(windows)]
struct WaveSlot {
    bytes: Vec<u8>,
    header: WaveHdr,
    prepared: bool,
}

#[cfg(windows)]
#[repr(C)]
struct WaveFormatEx {
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    size: u16,
}

#[cfg(windows)]
#[repr(C)]
struct WaveHdr {
    data: *mut u8,
    buffer_length: u32,
    bytes_recorded: u32,
    user: usize,
    flags: u32,
    loops: u32,
    next: *mut WaveHdr,
    reserved: usize,
}

#[cfg(windows)]
#[link(name = "winmm")]
extern "system" {
    fn waveOutOpen(
        handle: *mut *mut std::ffi::c_void,
        device: u32,
        format: *const WaveFormatEx,
        callback: usize,
        instance: usize,
        flags: u32,
    ) -> u32;
    fn waveOutPrepareHeader(
        handle: *mut std::ffi::c_void,
        header: *mut WaveHdr,
        size: u32,
    ) -> u32;
    fn waveOutUnprepareHeader(
        handle: *mut std::ffi::c_void,
        header: *mut WaveHdr,
        size: u32,
    ) -> u32;
    fn waveOutWrite(handle: *mut std::ffi::c_void, header: *mut WaveHdr, size: u32) -> u32;
    fn waveOutReset(handle: *mut std::ffi::c_void) -> u32;
    fn waveOutClose(handle: *mut std::ffi::c_void) -> u32;
}

#[cfg(windows)]
impl WaveOut {
    fn open(rate: u32) -> Result<Self, String> {
        let format = WaveFormatEx {
            format_tag: 1,
            channels: 1,
            samples_per_sec: rate,
            avg_bytes_per_sec: rate * 2,
            block_align: 2,
            bits_per_sample: 16,
            size: 0,
        };
        let mut handle = std::ptr::null_mut();
        let code = unsafe {
            waveOutOpen(
                &mut handle,
                u32::MAX,
                &format,
                0,
                0,
                0,
            )
        };
        if code != 0 || handle.is_null() {
            return Err(format!("устройство вывода не открылось ({code})"));
        }
        Ok(Self {
            handle,
            rate,
            slots: std::array::from_fn(|_| WaveSlot {
                bytes: Vec::new(),
                header: WaveHdr {
                    data: std::ptr::null_mut(),
                    buffer_length: 0,
                    bytes_recorded: 0,
                    user: 0,
                    flags: 0,
                    loops: 0,
                    next: std::ptr::null_mut(),
                    reserved: 0,
                },
                prepared: false,
            }),
        })
    }

    fn write(&mut self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        let header_size = std::mem::size_of::<WaveHdr>() as u32;
        for _ in 0..40 {
            if let Some(index) = self.free_slot() {
                self.submit(index, samples, header_size);
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn free_slot(&mut self) -> Option<usize> {
        let header_size = std::mem::size_of::<WaveHdr>() as u32;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if !slot.prepared {
                return Some(index);
            }
            if slot.header.flags & 1 != 0 {
                unsafe {
                    waveOutUnprepareHeader(self.handle, &mut slot.header, header_size);
                }
                slot.prepared = false;
                slot.header.flags = 0;
                return Some(index);
            }
        }
        None
    }

    fn submit(&mut self, index: usize, samples: &[i16], header_size: u32) {
        let slot = &mut self.slots[index];
        slot.bytes.clear();
        slot.bytes
            .extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        slot.header.data = slot.bytes.as_mut_ptr();
        slot.header.buffer_length = slot.bytes.len() as u32;
        slot.header.flags = 0;
        let code = unsafe { waveOutPrepareHeader(self.handle, &mut slot.header, header_size) };
        if code != 0 {
            return;
        }
        slot.prepared = true;
        let code = unsafe { waveOutWrite(self.handle, &mut slot.header, header_size) };
        if code != 0 {
            unsafe {
                waveOutUnprepareHeader(self.handle, &mut slot.header, header_size);
            }
            slot.prepared = false;
        }
    }

    fn close(mut self) {
        let header_size = std::mem::size_of::<WaveHdr>() as u32;
        unsafe {
            waveOutReset(self.handle);
        }
        for slot in &mut self.slots {
            if slot.prepared {
                unsafe {
                    waveOutUnprepareHeader(self.handle, &mut slot.header, header_size);
                }
                slot.prepared = false;
            }
        }
        unsafe {
            waveOutClose(self.handle);
        }
    }
}
