#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod decode;
mod library;
mod loopback;
mod setup;

use audio::{Clip, Engine};
use config::{Config, PadConfig};
use eframe::egui;
use egui::{
    Align, Align2, Color32, CornerRadius, FontFamily, FontId, Key, Layout, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Vec2,
};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BG: Color32 = Color32::from_rgb(15, 17, 22);
const CARD: Color32 = Color32::from_rgb(26, 30, 38);
const CARD_HOVER: Color32 = Color32::from_rgb(35, 40, 50);
const TEXT: Color32 = Color32::from_rgb(232, 236, 244);
const MUTED: Color32 = Color32::from_rgb(128, 138, 155);
const DIM: Color32 = Color32::from_rgb(92, 100, 114);
const ACCENT: Color32 = Color32::from_rgb(99, 211, 168);
const ACCENT_DIM: Color32 = Color32::from_rgb(27, 56, 48);
const LINE: Color32 = Color32::from_rgb(38, 43, 54);

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Minimize,
    Close,
}

enum Action {
    Play(usize),
    Rename(usize),
    Loop(usize, bool),
    Volume(usize, f32),
    Remove(usize),
}

struct Pad {
    cfg: PadConfig,
    clip: Option<Arc<Clip>>,
    missing: bool,
}

enum Task {
    Pack(Result<Vec<PathBuf>, String>),
    LocalScan(Vec<library::LocalSound>),
    Cable(setup::Event),
}
struct App {
    cfg: Config,
    engine: Engine,
    pads: Vec<Pad>,
    playing: HashSet<u64>,
    devices: Vec<String>,
    renaming: Option<(usize, String, bool)>,
    toast: Option<(String, Instant)>,
    hotkey_manager: Option<GlobalHotKeyManager>,
    hotkey_rx: std::sync::mpsc::Receiver<GlobalHotKeyEvent>,
    hotkey_bindings: Vec<(u32, u64)>,
    registered: Vec<HotKey>,
    next_id: u64,
    last_size: Option<[f32; 2]>,
    dirty: bool,
    dirty_at: Instant,
    help_open: bool,
    corners_applied: bool,
    logo: Option<egui::TextureHandle>,
    library_open: bool,
    search: String,
    local_sounds: Vec<library::LocalSound>,
    preview_cache: HashMap<String, Arc<Clip>>,
    task_tx: Sender<Task>,
    task_rx: Receiver<Task>,
    busy: Option<String>,
    cable_prompt: bool,
    installing_cable: bool,
    cable_note: Option<String>,
    capture: Option<loopback::Capture>,
    capture_devices: Vec<String>,
}

fn main() -> eframe::Result {
    if std::env::args().any(|arg| arg == "--check") {
        run_check();
        return Ok(());
    }

    let cfg = config::load();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Soundboard")
        .with_decorations(false)
        .with_resizable(true)
        .with_inner_size(cfg.window.unwrap_or([760.0, 500.0]))
        .with_min_inner_size([560.0, 380.0]);

    if let Some((width, height, rgba)) = load_logo_rgba() {
        viewport = viewport.with_icon(egui::IconData {
            rgba,
            width: width as u32,
            height: height as u32,
        });
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Soundboard",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cfg, cc)))),
    )
}

impl App {
    fn new(cfg: Config, cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        apply_style(&cc.egui_ctx);

        let (tx, hotkey_rx) = std::sync::mpsc::channel();
        let repaint_ctx = cc.egui_ctx.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            let _ = tx.send(event);
            repaint_ctx.request_repaint();
        }));

        let (task_tx, task_rx) = std::sync::mpsc::channel();
        let logo = load_logo_rgba().map(|(width, height, rgba)| {
            cc.egui_ctx.load_texture(
                "app-logo",
                egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba),
                egui::TextureOptions::LINEAR,
            )
        });

        let mut app = Self {
            cfg,
            engine: Engine::new(),
            pads: Vec::new(),
            playing: HashSet::new(),
            devices: Engine::list_devices(),
            renaming: None,
            toast: None,
            hotkey_manager: GlobalHotKeyManager::new().ok(),
            hotkey_rx,
            hotkey_bindings: Vec::new(),
            registered: Vec::new(),
            next_id: 1,
            last_size: None,
            dirty: false,
            dirty_at: Instant::now(),
            help_open: false,
            corners_applied: false,
            logo,
            library_open: false,
            search: String::new(),
            local_sounds: Vec::new(),
            preview_cache: HashMap::new(),
            task_tx,
            task_rx,
            busy: None,
            cable_prompt: false,
            installing_cable: false,
            cable_note: None,
            capture: None,
            capture_devices: loopback::list_devices(),
        };

        app.engine.set_master(app.cfg.master);
        app.engine.start(app.cfg.device.as_deref());
        app.load_pads();
        app.refresh_hotkeys();
        if app.cfg.system_audio {
            let device = app.cfg.capture_device.clone();
            app.start_capture(device);
        }
        if !app.cfg.library_folders.is_empty() {
            app.rescan_library(&cc.egui_ctx);
        }
        app
    }

    fn load_pads(&mut self) {
        let dir = config::sounds_dir();
        let mut missing = 0;

        for pad in self.cfg.pads.clone() {
            let exists = dir.join(&pad.file).exists();
            if !exists {
                missing += 1;
            }
            self.next_id = self.next_id.max(pad.id + 1);
            self.pads.push(Pad {
                cfg: pad,
                clip: None,
                missing: !exists,
            });
        }

        if missing > 0 {
            self.show_toast(format!("{missing} sound file(s) missing"));
        }
    }

    fn ensure_clip(&mut self, index: usize) -> Option<Arc<Clip>> {
        let pad = self.pads.get(index)?;
        if let Some(clip) = &pad.clip {
            return Some(clip.clone());
        }
        let path = config::sounds_dir().join(&pad.cfg.file);

        match decode::decode(&path) {
            Ok(clip) => {
                let clip = Arc::new(clip);
                if let Some(pad) = self.pads.get_mut(index) {
                    pad.clip = Some(clip.clone());
                    pad.missing = false;
                }
                Some(clip)
            }
            Err(error) => {
                let name = self.pads[index].cfg.name.clone();
                if let Some(pad) = self.pads.get_mut(index) {
                    pad.missing = true;
                }
                self.show_toast(format!("{name}: {error}"));
                None
            }
        }
    }
    fn persist(&self) {
        let mut cfg = self.cfg.clone();
        cfg.pads = self.pads.iter().map(|p| p.cfg.clone()).collect();
        cfg.window = self.last_size.or(cfg.window);
        config::save(&cfg);
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.dirty_at = Instant::now();
    }

    fn show_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    fn play_index(&mut self, index: usize) {
        let Some(pad) = self.pads.get(index) else {
            return;
        };
        let id = pad.cfg.id;
        let volume = pad.cfg.volume;
        let looping = pad.cfg.looping;
        let Some(clip) = self.ensure_clip(index) else {
            return;
        };

        if looping && self.playing.contains(&id) {
            self.engine.stop_pad(id);
            self.playing.remove(&id);
        } else {
            self.engine.play(id, clip, volume, looping);
        }
    }

    fn play_id(&mut self, id: u64) {
        if let Some(index) = self.pads.iter().position(|p| p.cfg.id == id) {
            self.play_index(index);
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Play(index) => self.play_index(index),
            Action::Rename(index) => {
                let name = self.pads[index].cfg.name.clone();
                self.renaming = Some((index, name, true));
            }
            Action::Loop(index, looping) => {
                self.pads[index].cfg.looping = looping;
                self.persist();
            }
            Action::Volume(index, volume) => {
                self.pads[index].cfg.volume = volume.clamp(0.0, 1.5);
                self.mark_dirty();
            }
            Action::Remove(index) => {
                let id = self.pads[index].cfg.id;
                self.engine.stop_pad(id);
                self.pads.remove(index);
                self.persist();
                self.refresh_hotkeys();
            }
        }
    }

    fn add_paths(&mut self, paths: &[PathBuf]) {
        let mut added = 0;
        for path in paths {
            if self.add_one(path, None) {
                added += 1;
            }
        }
        if added > 0 {
            self.persist();
            self.refresh_hotkeys();
            self.show_toast(format!(
                "Added {added} sound{}",
                if added == 1 { "" } else { "s" }
            ));
        }
    }

    fn add_one(&mut self, path: &Path, display_name: Option<&str>) -> bool {
        if !decode::is_audio(path) {
            self.show_toast(format!("Not an audio file: {}", file_name(path)));
            return false;
        }

        let clip = match decode::decode(path) {
            Ok(clip) => clip,
            Err(error) => {
                self.show_toast(format!("{}: {error}", file_name(path)));
                return false;
            }
        };

        let stored = match config::import_file(path) {
            Ok(name) => name,
            Err(error) => {
                self.show_toast(error);
                return false;
            }
        };

        let name = display_name.map(str::to_string).unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("sound")
                .to_string()
        });

        let id = self.next_id;
        self.next_id += 1;
        self.pads.push(Pad {
            cfg: PadConfig {
                id,
                name,
                file: stored,
                volume: 1.0,
                looping: false,
            },
            clip: Some(Arc::new(clip)),
            missing: false,
        });
        true
    }

    fn pick_files(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("Audio", decode::EXTENSIONS)
            .pick_files()
        {
            self.add_paths(&files);
        }
    }

    fn poll_tasks(&mut self) {
        while let Ok(task) = self.task_rx.try_recv() {
            match task {
                Task::LocalScan(sounds) => {
                    self.local_sounds = sounds;
                    self.busy = None;
                }
                Task::Pack(Ok(paths)) => {
                    self.busy = None;
                    self.add_pack_files(&paths);
                }
                Task::Pack(Err(error)) => {
                    self.busy = None;
                    self.show_toast(error);
                }
                Task::Cable(event) => match event {
                    setup::Event::Status(note) => self.cable_note = Some(note),
                    setup::Event::Done(Ok(())) => {
                        self.installing_cable = false;
                        self.cable_note = Some("Virtual cable installed".to_string());
                        self.devices = Engine::list_devices();
                        self.capture_devices = loopback::list_devices();
                        if let Some(cable) = setup::cable_input(&self.devices) {
                            self.cfg.device = Some(cable.clone());
                            self.engine.start(Some(&cable));
                            self.persist();
                        }
                    }
                    setup::Event::Done(Err(error)) => {
                        self.installing_cable = false;
                        self.cable_note = Some(error.clone());
                        self.show_toast(error);
                    }
                },
            }
        }
    }

    fn rescan_library(&mut self, ctx: &egui::Context) {
        if self.cfg.library_folders.is_empty() {
            return;
        }
        self.busy = Some("Scanning folders...".to_string());
        let folders = self.cfg.library_folders.clone();
        let tx = self.task_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let sounds = library::scan_folders(&folders);
            let _ = tx.send(Task::LocalScan(sounds));
            ctx.request_repaint();
        });
    }

    fn add_library_folder(&mut self, ctx: &egui::Context) {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            let folder = folder.to_string_lossy().to_string();
            if !self.cfg.library_folders.contains(&folder) {
                self.cfg.library_folders.push(folder);
                self.persist();
            }
            self.rescan_library(ctx);
        }
    }

    fn preview_local(&mut self, sound: &library::LocalSound) {
        let key = sound.path.to_string_lossy().to_string();
        if let Some(clip) = self.preview_cache.get(&key) {
            self.engine.play(u64::MAX, clip.clone(), 1.0, false);
            return;
        }
        match decode::decode(&sound.path) {
            Ok(clip) => {
                let clip = Arc::new(clip);
                self.preview_cache.insert(key, clip.clone());
                self.engine.play(u64::MAX, clip, 1.0, false);
            }
            Err(error) => self.show_toast(format!("{}: {error}", sound.name)),
        }
    }

    fn import_pack(&mut self, ctx: &egui::Context) {
        let Some(zip) = rfd::FileDialog::new()
            .add_filter("Sound pack", &["zip"])
            .pick_file()
        else {
            return;
        };
        self.busy = Some("Extracting pack...".to_string());
        let tx = self.task_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let temp =
                std::env::temp_dir().join(format!("soundboard-pack-{}", std::process::id()));
            let result = library::extract_pack(&zip, &temp);
            let _ = tx.send(Task::Pack(result));
            ctx.request_repaint();
        });
    }

    fn add_pack_files(&mut self, paths: &[PathBuf]) {
        let mut added = 0;
        for path in paths {
            let Ok(stored) = config::import_file(path) else {
                continue;
            };
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("sound")
                .to_string();
            let id = self.next_id;
            self.next_id += 1;
            self.pads.push(Pad {
                cfg: PadConfig {
                    id,
                    name,
                    file: stored,
                    volume: 1.0,
                    looping: false,
                },
                clip: None,
                missing: false,
            });
            added += 1;
        }

        if added > 0 {
            self.persist();
            self.refresh_hotkeys();
        }
        self.show_toast(format!(
            "Imported {added} sound{}",
            if added == 1 { "" } else { "s" }
        ));

        if let Some(first) = paths.first() {
            if let Some(dir) = first.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
    fn set_output(&mut self, device: Option<String>) {
        self.cfg.device = device;
        self.engine.start(self.cfg.device.as_deref());
        if let Some(capture) = &self.capture {
            if self.engine.device.as_deref() == Some(capture.device.as_str()) {
                self.stop_capture();
                self.show_toast("PC audio stopped: it would capture its own output");
            }
        }
        self.persist();
    }

    fn enable_pc_audio(&mut self) {
        if setup::cable_input(&self.devices).is_none() {
            self.cable_prompt = true;
            self.show_toast("Install the virtual mic first, then turn on PC audio");
            return;
        }
        if let Some(cable) = setup::cable_input(&self.devices) {
            if self.cfg.device.as_deref() != Some(cable.as_str()) {
                self.set_output(Some(cable));
            }
        }
        let device = self.cfg.capture_device.clone();
        self.start_capture(device);
    }
    fn start_capture(&mut self, device: Option<String>) {
        if let Some(current) = &self.capture {
            if device.as_deref() == Some(current.device.as_str()) {
                return;
            }
        }
        self.stop_capture();

        if let (Some(dev), Some(out)) = (device.as_deref(), self.engine.device.as_deref()) {
            if dev == out {
                self.show_toast("Pick a different source: that would loop the board into itself");
                return;
            }
        }

        let sink = self.engine.capture_sink();
        self.engine.set_capture_active(true);
        match loopback::start(device.clone(), sink) {
            Ok(capture) => {
                if self.engine.device.as_deref() == Some(capture.device.as_str()) {
                    drop(capture);
                    self.engine.set_capture_active(false);
                    self.cfg.system_audio = false;
                    self.show_toast("That device is already the board output - pick another source");
                    return;
                }
                self.capture = Some(capture);
                self.cfg.system_audio = true;
                self.cfg.capture_device = device;
                self.persist();
                self.show_toast("PC audio is going into the mic");
            }
            Err(error) => {
                self.engine.set_capture_active(false);
                self.cfg.system_audio = false;
                self.show_toast(error);
            }
        }
    }

    fn stop_capture(&mut self) {
        if self.capture.is_some() {
            self.capture = None;
            self.engine.set_capture_active(false);
            self.cfg.system_audio = false;
            self.persist();
        }
    }
    fn install_cable(&mut self) {
        if self.installing_cable {
            return;
        }
        self.installing_cable = true;
        self.cable_note = Some("Starting...".to_string());
        let tx = self.task_tx.clone();
        let (inner_tx, inner_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(event) = inner_rx.recv() {
                if tx.send(Task::Cable(event)).is_err() {
                    break;
                }
            }
        });
        setup::install(inner_tx);
    }
    fn refresh_hotkeys(&mut self) {
        let Some(manager) = self.hotkey_manager.take() else {
            return;
        };
        for hotkey in self.registered.drain(..) {
            let _ = manager.unregister(hotkey);
        }
        self.hotkey_bindings.clear();

        const CODES: [Code; 10] = [
            Code::Digit1,
            Code::Digit2,
            Code::Digit3,
            Code::Digit4,
            Code::Digit5,
            Code::Digit6,
            Code::Digit7,
            Code::Digit8,
            Code::Digit9,
            Code::Digit0,
        ];

        for (index, pad) in self.pads.iter().take(10).enumerate() {
            let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), CODES[index]);
            if manager.register(hotkey).is_ok() {
                self.hotkey_bindings.push((hotkey.id(), pad.cfg.id));
                self.registered.push(hotkey);
            }
        }

        self.hotkey_manager = Some(manager);
    }

    fn poll_hotkeys(&mut self) {
        while let Ok(event) = self.hotkey_rx.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if let Some((_, pad_id)) = self.hotkey_bindings.iter().find(|(id, _)| *id == event.id) {
                let pad_id = *pad_id;
                self.play_id(pad_id);
            }
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_paths(&dropped);
        }
    }

    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click_and_drag());
        if response.double_clicked() {
            let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        } else if response.drag_started() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        let mark = Pos2::new(rect.left() + 20.0, rect.center().y);
        let mut text_left = mark.x + 11.0;
        if let Some(texture) = &self.logo {
            let logo_rect = Rect::from_center_size(mark, Vec2::splat(20.0));
            ui.painter().image(
                texture.id(),
                logo_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            text_left = logo_rect.right() + 9.0;
        } else {
            ui.painter().circle_filled(mark, 3.5, ACCENT);
        }
        ui.painter().text(
            Pos2::new(text_left, mark.y),
            Align2::LEFT_CENTER,
            "Soundboard",
            FontId::proportional(13.5),
            TEXT,
        );

        let close = Rect::from_center_size(
            Pos2::new(rect.right() - 22.0, rect.center().y),
            Vec2::splat(26.0),
        );
        let minimize = close.translate(Vec2::new(-32.0, 0.0));
        self.window_button(ui, minimize, Icon::Minimize);
        self.window_button(ui, close, Icon::Close);

        ui.painter().text(
            Pos2::new(close.left() - 14.0, rect.center().y),
            Align2::RIGHT_CENTER,
            "ctrl+alt+1-0",
            FontId::proportional(10.5),
            DIM,
        );
    }

    fn window_button(&self, ui: &mut egui::Ui, rect: Rect, icon: Icon) {
        let response = ui.interact(
            rect,
            ui.scope_id().with(("window_button", icon as u8)),
            Sense::click(),
        );
        let hovered = response.hovered();
        let background = if hovered {
            if icon == Icon::Close {
                Color32::from_rgb(198, 62, 62)
            } else {
                Color32::from_rgb(40, 45, 56)
            }
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), background);

        let stroke = Stroke::new(
            1.3,
            if hovered {
                Color32::from_rgb(250, 250, 252)
            } else {
                MUTED
            },
        );
        let center = rect.center();
        let r = 4.0;
        match icon {
            Icon::Minimize => {
                ui.painter().line_segment(
                    [
                        Pos2::new(center.x - r, center.y),
                        Pos2::new(center.x + r, center.y),
                    ],
                    stroke,
                );
            }
            Icon::Close => {
                ui.painter().line_segment(
                    [
                        Pos2::new(center.x - r, center.y - r),
                        Pos2::new(center.x + r, center.y + r),
                    ],
                    stroke,
                );
                ui.painter().line_segment(
                    [
                        Pos2::new(center.x - r, center.y + r),
                        Pos2::new(center.x + r, center.y - r),
                    ],
                    stroke,
                );
            }
        }

        if response.clicked() {
            match icon {
                Icon::Minimize => ui
                    .ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                Icon::Close => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(RichText::new("Output").size(11.0).color(MUTED));

            let combo_width = (ui.available_width() * 0.45).clamp(130.0, 260.0);
            let selected = self
                .cfg
                .device
                .clone()
                .unwrap_or_else(|| "System default".to_string());

            egui::ComboBox::from_id_salt("device_combo")
                .width(combo_width)
                .selected_text(RichText::new(selected).size(12.0))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.cfg.device.is_none(), "System default")
                        .clicked()
                    {
                        self.set_output(None);
                    }
                    for name in self.devices.clone() {
                        let active = self.cfg.device.as_deref() == Some(name.as_str());
                        if ui.selectable_label(active, RichText::new(&name).size(12.0)).clicked() {
                            self.set_output(Some(name.clone()));
                        }
                    }
                });

            if ui
                .add(egui::Button::new(RichText::new("Refresh").size(11.0)).corner_radius(CornerRadius::same(7)))
                .on_hover_text("Rescan audio devices")
                .clicked()
            {
                self.devices = Engine::list_devices();
                self.capture_devices = loopback::list_devices();
                self.show_toast("Device list refreshed");
            }

            ui.add_space(6.0);
            ui.label(RichText::new("Vol").size(11.0).color(MUTED));
            let slider = ui.add_sized(
                [100.0, 18.0],
                egui::Slider::new(&mut self.cfg.master, 0.0..=1.0).show_value(false),
            );
            if slider.changed() {
                self.engine.set_master(self.cfg.master);
            }
            if slider.drag_stopped() {
                self.persist();
            }

            if let Some(cable) = setup::cable_input(&self.devices) {
                if self.cfg.device.as_deref() != Some(cable.as_str()) {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Use cable").size(11.0))
                                .corner_radius(CornerRadius::same(7)),
                        )
                        .on_hover_text("Send the board into VB-Audio Cable")
                        .clicked()
                    {
                        self.set_output(Some(cable.clone()));
                        self.show_toast("Output set to CABLE Input");
                    }
                }
            }
        });

        ui.horizontal(|ui| {
            ui.add_space(2.0);
            if !setup::has_cable(&self.devices) {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Set up virtual mic")
                                .size(11.0)
                                .color(ACCENT),
                        )
                        .corner_radius(CornerRadius::same(7)),
                    )
                    .on_hover_text("Install VB-Audio Cable so other apps hear the board")
                    .clicked()
                {
                    self.cable_prompt = true;
                }
            }

            let capturing = self.capture.is_some();
            let mut toggle_capture = false;
            let mut new_source: Option<Option<String>> = None;
            let current_source = self
                .capture
                .as_ref()
                .map(|capture| capture.device.clone())
                .unwrap_or_default();

            if ui
                .add(
                    egui::Button::new(
                        RichText::new(if capturing {
                            "PC audio: on"
                        } else {
                            "PC audio: off"
                        })
                        .size(11.0)
                        .color(if capturing { ACCENT } else { MUTED }),
                    )
                    .corner_radius(CornerRadius::same(7)),
                )
                .on_hover_text("Send everything playing on this PC into the microphone")
                .clicked()
            {
                toggle_capture = true;
            }

            if capturing {
                let devices = self.capture_devices.clone();
                egui::ComboBox::from_id_salt("capture_source")
                    .width(150.0)
                    .selected_text(RichText::new(ellipsize(&current_source, 18)).size(11.0))
                    .show_ui(ui, |ui| {
                        for name in devices {
                            if ui
                                .selectable_label(
                                    current_source == name,
                                    RichText::new(&name).size(11.0),
                                )
                                .clicked()
                            {
                                new_source = Some(Some(name.clone()));
                            }
                        }
                    });
            }

            if toggle_capture {
                if capturing {
                    self.stop_capture();
                } else {
                    self.enable_pc_audio();
                }
            }
            if let Some(source) = new_source {
                self.start_capture(source);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(RichText::new("Stop").size(12.0)).corner_radius(CornerRadius::same(7)))
                    .clicked()
                {
                    self.engine.stop_all();
                    self.playing.clear();
                }
                if ui
                    .add(egui::Button::new(RichText::new("?").size(12.0)).corner_radius(CornerRadius::same(7)))
                    .on_hover_text("How to play into your microphone")
                    .clicked()
                {
                    self.help_open = true;
                }
                let add = ui.add(
                    egui::Button::new(
                        RichText::new("+ Add sounds")
                            .size(12.0)
                            .strong()
                            .color(Color32::from_rgb(8, 18, 14)),
                    )
                    .fill(ACCENT)
                    .corner_radius(CornerRadius::same(8)),
                );
                if add.clicked() {
                    self.pick_files();
                }
                let library_button = ui.add(
                    egui::Button::new(RichText::new("Library").size(12.0))
                        .corner_radius(CornerRadius::same(7)),
                );
                if library_button.clicked() {
                    self.library_open = !self.library_open;
                }
            });
        });

        ui.add_space(6.0);

        if let Some(error) = self.engine.error.lock().ok().and_then(|e| e.clone()) {
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.label(RichText::new(error).size(11.0).color(Color32::from_rgb(232, 120, 120)));
            });
        }
    }
    fn grid(&mut self, ui: &mut egui::Ui) {
        let gap = 10.0;
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(16, 14))
            .show(ui, |ui| {
                if self.pads.is_empty() {
                    let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
                    ui.painter().text(
                        rect.center() + Vec2::new(0.0, -12.0),
                        Align2::CENTER_CENTER,
                        "Drop audio files here",
                        FontId::proportional(16.0),
                        MUTED,
                    );
                    ui.painter().text(
                        rect.center() + Vec2::new(0.0, 14.0),
                        Align2::CENTER_CENTER,
                        "mp3 / wav / ogg / flac / m4a  -  or press + Add sounds",
                        FontId::proportional(11.5),
                        DIM,
                    );
                    return;
                }

                let available = ui.available_width();
                let columns =
                    (((available + gap) / (150.0 + gap)).floor() as usize).clamp(1, 8);
                let cell_width =
                    ((available - gap * (columns as f32 - 1.0)) / columns as f32).max(60.0);
                let cell_height = (cell_width * 0.78).clamp(64.0, 148.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(gap, gap);
                        let mut actions = Vec::new();
                        let total = self.pads.len();
                        for row_start in (0..total).step_by(columns) {
                            ui.horizontal(|ui| {
                                let end = (row_start + columns).min(total);
                                for index in row_start..end {
                                    if let Some(action) =
                                        self.pad_widget(ui, index, Vec2::new(cell_width, cell_height))
                                    {
                                        actions.push(action);
                                    }
                                }
                            });
                        }
                        for action in actions {
                            self.apply(action);
                        }
                    });
            });
    }

    fn pad_widget(&self, ui: &mut egui::Ui, index: usize, size: Vec2) -> Option<Action> {
        let pad = &self.pads[index];
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let radius = CornerRadius::same(14);
        let playing = self.playing.contains(&pad.cfg.id);
        let hovered = response.hovered();
        let missing = pad.missing;

        let painter = ui.painter();
        let background = if playing {
            ACCENT_DIM
        } else if hovered {
            CARD_HOVER
        } else {
            CARD
        };
        painter.rect_filled(rect, radius, background);

        if playing {
            painter.rect_stroke(rect, radius, Stroke::new(1.2, ACCENT), StrokeKind::Inside);
            let bar = Rect::from_min_size(
                Pos2::new(rect.left() + 10.0, rect.top() + 12.0),
                Vec2::new(3.0, 14.0),
            );
            painter.rect_filled(bar, CornerRadius::same(2), ACCENT);
        } else if hovered {
            painter.rect_stroke(rect, radius, Stroke::new(1.0, Color32::from_rgb(58, 66, 82)), StrokeKind::Inside);
        }

        let label_color = if missing {
            Color32::from_rgb(112, 120, 134)
        } else if playing {
            Color32::WHITE
        } else {
            TEXT
        };
        let max_chars = ((size.x - 22.0) / 7.4).max(4.0) as usize;
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            ellipsize(&pad.cfg.name, max_chars),
            FontId::proportional(13.0),
            label_color,
        );

        let small = FontId::proportional(10.0);
        if missing {
            painter.text(
                Pos2::new(rect.center().x, rect.center().y + 18.0),
                Align2::CENTER_CENTER,
                "file missing",
                small,
                Color32::from_rgb(168, 108, 108),
            );
        } else {
            if pad.cfg.looping {
                painter.text(
                    Pos2::new(rect.right() - 10.0, rect.top() + 8.0),
                    Align2::RIGHT_TOP,
                    "loop",
                    FontId::proportional(9.5),
                    ACCENT,
                );
            }
            if (pad.cfg.volume - 1.0).abs() > 0.01 {
                painter.text(
                    Pos2::new(rect.right() - 10.0, rect.bottom() - 8.0),
                    Align2::RIGHT_BOTTOM,
                    format!("{}%", (pad.cfg.volume * 100.0).round() as i32),
                    small.clone(),
                    MUTED,
                );
            }
            if let Some(clip) = &pad.clip {
                painter.text(
                    Pos2::new(rect.left() + 10.0, rect.bottom() - 8.0),
                    Align2::LEFT_BOTTOM,
                    fmt_duration(clip.duration),
                    small.clone(),
                    DIM,
                );
            }
        }

        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        let mut action = None;

        if response.clicked() {
            action = Some(Action::Play(index));
        }

        response.context_menu(|ui| {
            ui.set_min_width(190.0);
            ui.label(RichText::new(&pad.cfg.name).size(11.5).color(MUTED));
            ui.separator();
            if ui.button("Rename").clicked() {
                action = Some(Action::Rename(index));
                ui.close();
            }

            let mut looping = pad.cfg.looping;
            if ui.checkbox(&mut looping, "Loop").changed() {
                action = Some(Action::Loop(index, looping));
            }

            let mut volume = pad.cfg.volume;
            ui.horizontal(|ui| {
                ui.label("Volume");
                if ui
                    .add(egui::Slider::new(&mut volume, 0.0..=1.5).show_value(false))
                    .changed()
                {
                    action = Some(Action::Volume(index, volume));
                }
            });

            ui.separator();
            if ui
                .button(RichText::new("Remove").color(Color32::from_rgb(232, 120, 120)))
                .clicked()
            {
                action = Some(Action::Remove(index));
                ui.close();
            }
        });

        action
    }

    fn rename_window(&mut self, ctx: &egui::Context) {
        let Some((index, text, focus)) = self.renaming.as_mut() else {
            return;
        };

        let mut open = true;
        let mut commit = false;
        let mut cancel = false;

        egui::Window::new("Rename pad")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(250.0);
                let edit = ui.add(
                    egui::TextEdit::singleline(text)
                        .desired_width(f32::INFINITY)
                        .hint_text("Pad name"),
                );
                if *focus {
                    edit.request_focus();
                    *focus = false;
                }
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    commit = true;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        commit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        let index = *index;
        let name = text.trim().to_string();

        if commit && !name.is_empty() {
            self.pads[index].cfg.name = name;
            self.persist();
            self.renaming = None;
        } else if cancel || !open || commit {
            self.renaming = None;
        }
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        if !self.help_open {
            return;
        }
        let mut open = self.help_open;
        egui::Window::new("Play into your microphone")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(400.0);
                ui.label(RichText::new("One-time setup").strong().color(ACCENT));
                ui.add_space(4.0);
                let steps = [
                    "Press \"Set up virtual mic\" in the bottom bar; it downloads and installs VB-Audio Cable for you.",
                    "Pick \"CABLE Input\" as the Output device above.",
                    "In Discord / OBS / your game, set the microphone to \"CABLE Output\".",
                ];
                for (index, step) in steps.iter().enumerate() {
                    ui.label(RichText::new(format!("{}. {step}", index + 1)).size(12.0));
                }
                ui.add_space(10.0);
                ui.label(RichText::new("Heads up").strong().color(MUTED));
                ui.label(
                    RichText::new(
                        "While the cable is your mic, people only hear the board. To talk at the same time, mix your real mic and the cable together with VoiceMeeter.",
                    )
                    .size(11.5)
                    .color(MUTED),
                );
                ui.add_space(10.0);
                ui.label(
                    RichText::new(
                        "Ctrl+Alt+1 ... Ctrl+Alt+0 play the first ten pads. Right-click a pad for volume, loop and rename.",
                    )
                    .size(11.5)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "PC audio: on also sends everything playing on the chosen device into the mic. That includes Discord's own output, so keep Discord on a different device (or use VoiceMeeter) to avoid echo.",
                    )
                    .size(11.0)
                    .color(DIM),
                );
            });
        self.help_open = open;
    }

    fn library_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Sounds").strong().size(13.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(RichText::new("x").size(11.0)).corner_radius(CornerRadius::same(6)))
                    .clicked()
                {
                    self.library_open = false;
                }
            });
        });

        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search sounds")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.0);

        self.files_tab(ui);

        if let Some(busy) = self.busy.clone() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(busy).size(11.0).color(MUTED));
            });
        }
    }
    fn files_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(RichText::new("+ Folder").size(11.5)).corner_radius(CornerRadius::same(7)))
                .clicked()
            {
                self.add_library_folder(ui.ctx());
            }
            if ui
                .add(egui::Button::new(RichText::new("+ Pack").size(11.5)).corner_radius(CornerRadius::same(7)))
                .on_hover_text("Import a .zip sound pack")
                .clicked()
            {
                self.import_pack(ui.ctx());
            }
            if ui
                .add(egui::Button::new(RichText::new("Rescan").size(11.5)).corner_radius(CornerRadius::same(7)))
                .clicked()
            {
                self.rescan_library(ui.ctx());
            }
            ui.label(
                RichText::new(format!("{} files", self.local_sounds.len()))
                    .size(11.0)
                    .color(DIM),
            );
        });

        if self.cfg.library_folders.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Point at a folder of sounds and they show up here. Files stay where they are until you add one to the board.",
                )
                .size(11.5)
                .color(MUTED),
            );
            return;
        }

        let needle = self.search.trim().to_lowercase();
        let indices: Vec<usize> = self
            .local_sounds
            .iter()
            .enumerate()
            .filter(|(_, sound)| needle.is_empty() || sound.name.to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .take(300)
            .collect();
        let sounds: Vec<library::LocalSound> = indices
            .into_iter()
            .map(|index| self.local_sounds[index].clone())
            .collect();

        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for sound in sounds {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(ellipsize(&sound.name, 28)).size(12.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .small_button("+")
                                .on_hover_text("Add to board")
                                .clicked()
                            {
                                if self.add_one(&sound.path, Some(&sound.name)) {
                                    self.persist();
                                    self.refresh_hotkeys();
                                    self.show_toast(format!("Added \"{}\"", sound.name));
                                }
                            }
                            if ui.small_button("Play").clicked() {
                                self.preview_local(&sound);
                            }
                        });
                    });
                    ui.separator();
                }
            });
    }

    fn cable_window(&mut self, ctx: &egui::Context) {
        if !self.cable_prompt {
            return;
        }
        let mut open = self.cable_prompt;
        egui::Window::new("Virtual microphone")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label(RichText::new("Install VB-Audio Cable").strong().color(ACCENT));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Installs a signed virtual audio cable so other apps can hear the board as a microphone. Windows will ask for admin approval.",
                    )
                    .size(11.5)
                    .color(MUTED),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if self.installing_cable {
                        ui.spinner();
                        ui.label(RichText::new("Installing...").size(11.5).color(MUTED));
                    } else if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Download and install")
                                    .strong()
                                    .color(Color32::from_rgb(8, 18, 14)),
                            )
                            .fill(ACCENT)
                            .corner_radius(CornerRadius::same(8)),
                        )
                        .clicked()
                    {
                        self.install_cable();
                    }
                    if ui.button("Open website").clicked() {
                        setup::open_home_page();
                    }
                });
                if let Some(note) = self.cable_note.clone() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(note).size(11.0).color(MUTED));
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "After it finishes: set Output to CABLE Input here, and set your mic to CABLE Output in Discord/OBS.",
                    )
                    .size(11.0)
                    .color(DIM),
                );
            });
        self.cable_prompt = open;
    }
    fn toast_ui(&mut self, ctx: &egui::Context) {
        let Some((message, shown_at)) = self.toast.clone() else {
            return;
        };
        let age = shown_at.elapsed().as_secs_f32();
        if age > 4.0 {
            self.toast = None;
            return;
        }
        let fade = if age > 3.2 { 1.0 - (age - 3.2) / 0.8 } else { 1.0 };
        let alpha = (fade * 255.0).clamp(0.0, 255.0) as u8;

        egui::Area::new(egui::Id::unique("soundboard_toast"))
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -62.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(30, 34, 42, alpha))
                    .corner_radius(CornerRadius::same(9))
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&message)
                                .size(12.0)
                                .color(Color32::from_rgba_unmultiplied(238, 241, 246, alpha)),
                        );
                    });
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if !self.corners_applied {
            apply_rounded_corners(frame);
            self.corners_applied = true;
        }

        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            self.last_size = Some([rect.width(), rect.height()]);
        }

        self.playing = self.engine.active();
        if let Some(error) = self.engine.take_error() {
            self.show_toast(error);
        }
        self.poll_hotkeys();
        self.poll_tasks();
        self.handle_drops(&ctx);

        if self.dirty && self.dirty_at.elapsed() > Duration::from_millis(600) {
            self.persist();
            self.dirty = false;
        }

        egui::Panel::top("title_bar").show(ui, |ui| self.title_bar(ui));
        egui::Panel::bottom("controls").show(ui, |ui| self.controls(ui));
        if self.library_open {
            egui::Panel::right("library_panel")
                .default_size(340.0)
                .min_size(300.0)
                .resizable(false)
                .show(ui, |ui| self.library_panel(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.grid(ui));

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let rect = ui.max_rect();
            ui.painter()
                .rect_filled(rect, 0, Color32::from_rgba_unmultiplied(10, 12, 16, 205));
            ui.painter().text(
                rect.center() + Vec2::new(0.0, -10.0),
                Align2::CENTER_CENTER,
                "Drop to add",
                FontId::proportional(20.0),
                ACCENT,
            );
            ui.painter().text(
                rect.center() + Vec2::new(0.0, 18.0),
                Align2::CENTER_CENTER,
                "mp3 / wav / ogg / flac / m4a",
                FontId::proportional(12.0),
                MUTED,
            );
        }

        self.rename_window(&ctx);
        self.help_window(&ctx);
        self.cable_window(&ctx);
        self.toast_ui(&ctx);

        ui.painter().rect_stroke(
            ui.max_rect(),
            CornerRadius::same(8),
            Stroke::new(1.0, Color32::from_rgb(33, 37, 46)),
            StrokeKind::Inside,
        );

        if !self.playing.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(80));
        } else {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.persist();
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\SegoeUIVF.ttf",
        r"C:\Windows\Fonts\segoeuib.ttf",
    ];

    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let key = format!("sys-{}", fonts.font_data.len());
            fonts
                .font_data
                .insert(key.clone(), Arc::new(egui::FontData::from_owned(bytes)));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, key);
            break;
        }
    }

    ctx.set_fonts(fonts);
}

fn apply_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let mut visuals = egui::Visuals::dark();

        visuals.panel_fill = BG;
        visuals.window_fill = CARD;
        visuals.extreme_bg_color = Color32::from_rgb(10, 12, 16);
        visuals.faint_bg_color = Color32::from_rgb(22, 25, 32);
        visuals.override_text_color = Some(TEXT);
        visuals.window_stroke = Stroke::new(1.0, LINE);
        visuals.window_corner_radius = CornerRadius::same(12);
        visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(99, 211, 168, 70);
        visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        visuals.hyperlink_color = ACCENT;

        visuals.widgets.noninteractive.bg_fill = BG;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);

        visuals.widgets.inactive.bg_fill = CARD;
        visuals.widgets.inactive.weak_bg_fill = CARD;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);

        visuals.widgets.hovered.bg_fill = CARD_HOVER;
        visuals.widgets.hovered.weak_bg_fill = CARD_HOVER;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(62, 70, 88));
        visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);

        visuals.widgets.active.bg_fill = ACCENT_DIM;
        visuals.widgets.active.weak_bg_fill = ACCENT_DIM;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        visuals.widgets.active.corner_radius = CornerRadius::same(8);
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

        style.visuals = visuals;
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.spacing.slider_width = 110.0;
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(12.5));
        style
            .text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(11.0));
    });
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_string()
    } else if max_chars <= 3 {
        "...".to_string()
    } else {
        let mut out: String = text.chars().take(max_chars - 3).collect();
        out.push_str("...");
        out
    }
}

fn fmt_duration(seconds: f32) -> String {
    let total = seconds.round().max(0.0) as u32;
    format!("{}:{:02}", total / 60, total % 60)
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string()
}

fn run_check() {
    let cfg = config::load();
    let mut report = String::new();
    report.push_str(&format!("soundboard {}\n", env!("CARGO_PKG_VERSION")));

    let devices = Engine::list_devices();
    report.push_str(&format!("output devices ({}):\n", devices.len()));
    for device in &devices {
        report.push_str(&format!("  - {device}\n"));
    }
    report.push_str(&format!(
        "virtual cable: {}\n",
        if setup::has_cable(&devices) {
            "found"
        } else {
            "not installed"
        }
    ));

    let mut engine = Engine::new();
    engine.set_master(0.6);
    engine.start(cfg.device.as_deref());
    report.push_str(&format!("opened: {:?}\n", engine.device));
    if let Some(error) = engine.take_error() {
        report.push_str(&format!("ERROR: {error}\n"));
    }

    let dir = config::sounds_dir();
    let mut played = 0;
    for pad in &cfg.pads {
        match decode::decode(&dir.join(&pad.file)) {
            Ok(clip) => {
                report.push_str(&format!(
                    "decoded \"{}\": {:.2}s, {} Hz, {} ch\n",
                    pad.name, clip.duration, clip.rate, clip.channels
                ));
                engine.play(pad.id, Arc::new(clip), 0.5, false);
                std::thread::sleep(Duration::from_millis(350));
                played += 1;
                if let Some(error) = engine.take_error() {
                    report.push_str(&format!("ERROR: {error}\n"));
                }
            }
            Err(error) => report.push_str(&format!("failed \"{}\": {error}\n", pad.name)),
        }
    }

    std::thread::sleep(Duration::from_millis(400));
    report.push_str(&format!("played {played} clip(s)\n"));
    match engine.take_error() {
        Some(error) => report.push_str(&format!("ERROR: {error}\n")),
        None => report.push_str("no stream errors\n"),
    }

    let loopback_devices = loopback::list_devices();
    report.push_str(&format!(
        "loopback sources ({}): {:?}\n",
        loopback_devices.len(),
        loopback_devices
    ));
    let sink = engine.capture_sink();
    match loopback::start(None, sink.clone()) {
        Ok(capture) => {
            std::thread::sleep(Duration::from_millis(900));
            report.push_str(&format!(
                "loopback: {} ({} frames captured)\n",
                capture.device,
                sink.frames()
            ));
            drop(capture);
        }
        Err(error) => report.push_str(&format!("loopback ERROR: {error}\n")),
    }
    let path = config::base_dir().join("soundboard-check.txt");
    let _ = std::fs::write(&path, report);
}
#[cfg(windows)]
fn apply_rounded_corners(frame: &eframe::Frame) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attribute: u32,
            value: *const core::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: i32 = 2;

    let Some(window) = frame.winit_window() else {
        return;
    };
    let Ok(handle) = window.window_handle() else {
        return;
    };
    if let RawWindowHandle::Win32(win32) = handle.as_raw() {
        let preference = DWMWCP_ROUND;
        unsafe {
            DwmSetWindowAttribute(
                win32.hwnd.get() as isize,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &preference as *const i32 as *const core::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
}

#[cfg(not(windows))]
fn apply_rounded_corners(_frame: &eframe::Frame) {}
fn load_logo_rgba() -> Option<(usize, usize, Vec<u8>)> {
    let candidates = [
        config::base_dir().join("assets").join("logo.png"),
        config::base_dir().join("logo.png"),
    ];
    for path in candidates {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(image) = image::load_from_memory(&bytes) else {
            continue;
        };
        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        return Some((width as usize, height as usize, rgba.into_raw()));
    }
    None
}