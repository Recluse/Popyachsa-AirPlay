//! About window — same Apple-y aesthetic as the Settings dialog.
//! Spawned as `popyachsa-airplay.exe --about` so eframe owns its own loop.

use eframe::egui::{self, Color32, FontId, Margin, RichText, Rounding, Stroke, Vec2};

use crate::config::APP_NAME;

const ACCENT_BLUE: Color32 = Color32::from_rgb(0x0A, 0x84, 0xFF);
const BG_DARK:     Color32 = Color32::from_rgb(0x18, 0x1A, 0x20);
const BG_PANEL:    Color32 = Color32::from_rgb(0x22, 0x25, 0x2D);
const BG_FIELD:    Color32 = Color32::from_rgb(0x2C, 0x30, 0x39);
const TEXT_PRIM:   Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF7);
const TEXT_DIM:    Color32 = Color32::from_rgb(0x9A, 0x9F, 0xAA);

const APP_ICON_PNG: &[u8] = include_bytes!("../icons/app.ico");
const PPC_LOGO_PNG: &[u8] = include_bytes!("../icons/popyachsacraft-logo.png");

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns true if we hold the single-instance lock and should proceed.
#[cfg(windows)]
fn acquire_single_instance(window_title: &str) -> bool {
    use windows::core::{PCWSTR, w};
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };
    let h = unsafe { CreateMutexW(None, false, w!("PopyachsaAirPlay.About.SingleInstance")) };
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already {
        let title: Vec<u16> = window_title.encode_utf16()
            .chain(std::iter::once(0)).collect();
        unsafe {
            if let Ok(hwnd) = FindWindowW(None, PCWSTR(title.as_ptr())) {
                if hwnd != HWND::default() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                    let _ = BringWindowToTop(hwnd);
                    let _ = SetForegroundWindow(hwnd);
                }
            }
        }
        return false;
    }
    std::mem::forget(h);
    true
}

#[cfg(not(windows))]
fn acquire_single_instance(_window_title: &str) -> bool { true }

pub fn run() -> Result<(), eframe::Error> {
    let lang = crate::i18n::Lang::from_config(&crate::config::Config::load().language);
    let t = crate::i18n::s(lang);
    let win_title = format!("{APP_NAME} — {}", t.about);

    if !acquire_single_instance(&win_title) { return Ok(()); }

    let icon_data = image::load_from_memory(APP_ICON_PNG).ok().map(|i| {
        let rgba = i.to_rgba8();
        let (w, h) = rgba.dimensions();
        egui::IconData { rgba: rgba.into_raw(), width: w, height: h }
    });

    let win_w = 540.0_f32;
    let win_h = 720.0_f32;
    #[cfg(windows)]
    let pos: Option<egui::Pos2> = unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        let sw = GetSystemMetrics(SM_CXSCREEN) as f32;
        let sh = GetSystemMetrics(SM_CYSCREEN) as f32;
        Some(egui::Pos2::new(((sw - win_w) * 0.5).max(0.0),
                             ((sh - win_h) * 0.5).max(0.0)))
    };
    #[cfg(not(windows))]
    let pos: Option<egui::Pos2> = None;

    let mut opts = eframe::NativeOptions::default();
    opts.viewport = egui::ViewportBuilder::default()
        .with_title(&win_title)
        .with_inner_size([win_w, win_h])
        .with_min_inner_size([480.0, 600.0])
        .with_resizable(true);
    if let Some(p) = pos {
        opts.viewport = opts.viewport.clone().with_position(p);
    }
    if let Some(icon) = icon_data {
        opts.viewport = opts.viewport.with_icon(icon);
    }

    eframe::run_native(
        &win_title,
        opts,
        Box::new(move |cc| {
            apply_theme(&cc.egui_ctx);
            crate::fonts::install(&cc.egui_ctx);
            Ok(Box::new(AboutApp { logo: None, lang }))
        }),
    )
}

fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT_PRIM);
    visuals.window_fill = BG_DARK;
    visuals.panel_fill = BG_DARK;
    visuals.extreme_bg_color = BG_FIELD;
    visuals.faint_bg_color = BG_PANEL;
    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.inactive.bg_fill = BG_FIELD;
    visuals.widgets.hovered.bg_fill = BG_FIELD.gamma_multiply(1.2);
    visuals.widgets.active.bg_fill = ACCENT_BLUE.gamma_multiply(0.6);
    visuals.selection.bg_fill = ACCENT_BLUE.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT_BLUE);
    visuals.hyperlink_color = ACCENT_BLUE;
    visuals.window_rounding = Rounding::same(12.0);
    visuals.menu_rounding = Rounding::same(10.0);
    visuals.widgets.noninteractive.rounding = Rounding::same(8.0);
    visuals.widgets.inactive.rounding = Rounding::same(8.0);
    visuals.widgets.hovered.rounding = Rounding::same(8.0);
    visuals.widgets.active.rounding = Rounding::same(8.0);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    use egui::TextStyle::*;
    style.text_styles.insert(Body,      FontId::proportional(14.0));
    style.text_styles.insert(Button,    FontId::proportional(14.0));
    style.text_styles.insert(Heading,   FontId::proportional(22.0));
    style.text_styles.insert(Monospace, FontId::monospace(13.0));
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 6.0);
    style.spacing.window_margin = Margin::same(18.0);
    ctx.set_style(style);
}

struct AboutApp {
    logo: Option<egui::TextureHandle>,
    lang: crate::i18n::Lang,
}

/// Lazily decode + upload the PopyachsaCraft logo as an egui texture.
fn ppc_logo(ctx: &egui::Context, cache: &mut Option<egui::TextureHandle>) -> Option<egui::TextureHandle> {
    if cache.is_none() {
        if let Ok(img) = image::load_from_memory(PPC_LOGO_PNG) {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
            *cache = Some(ctx.load_texture("ppc-logo", color, egui::TextureOptions::LINEAR));
        }
    }
    cache.clone()
}

impl eframe::App for AboutApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let t = crate::i18n::s(self.lang);
        egui::TopBottomPanel::bottom("bottom_bar")
            .frame(egui::Frame::default().fill(BG_PANEL)
                   .inner_margin(Margin::symmetric(18.0, 12.0)))
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let btn = egui::Button::new(
                        RichText::new(t.close).size(14.0).color(Color32::WHITE).strong())
                        .fill(ACCENT_BLUE);
                    if ui.add_sized([100.0, 30.0], btn).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(BG_DARK).inner_margin(Margin::same(0.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    ui.add_space(28.0);
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        if let Some(tex) = ppc_logo(ctx, &mut self.logo) {
                            ui.add(egui::Image::new(
                                egui::load::SizedTexture::new(tex.id(), egui::vec2(56.0, 56.0))));
                            ui.add_space(12.0);
                        }
                        ui.heading(RichText::new(APP_NAME).color(TEXT_PRIM).strong());
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.label(RichText::new(format!("Version {VERSION}"))
                                 .color(TEXT_DIM).size(13.0));
                    });
                    ui.add_space(18.0);

                    section(ui, t.about_what, |ui| {
                        for p in t.about_body.split("\n\n") {
                            para(ui, p);
                        }
                    });

                    section(ui, t.about_builton, |ui| {
                        link_row(ui, "UxPlay",
                                 "Open-source AirPlay mirroring + audio server",
                                 "https://github.com/FDH2/UxPlay");
                        link_row(ui, "RPiPlay",
                                 "UxPlay's predecessor, original RTSP/FairPlay reverse-engineering",
                                 "https://github.com/FD-/RPiPlay");
                        link_row(ui, "openairplay / airplay-spec",
                                 "Community-maintained unofficial AirPlay protocol specification",
                                 "https://github.com/openairplay/airplay-spec");
                        link_row(ui, "mjansson / mdns",
                                 "Single-header C mDNS responder used by our dnssd.dll shim",
                                 "https://github.com/mjansson/mdns");
                        link_row(ui, "GStreamer",
                                 "Multimedia pipeline framework (d3d11videosink, nvh264dec, …)",
                                 "https://gstreamer.freedesktop.org/");
                        link_row(ui, "tauri-apps / tray-icon",
                                 "Cross-platform native tray icon used here on Windows",
                                 "https://github.com/tauri-apps/tray-icon");
                        link_row(ui, "tauri-apps / tao",
                                 "Tao event loop driving the tray menu",
                                 "https://github.com/tauri-apps/tao");
                        link_row(ui, "emilk / egui (eframe)",
                                 "Immediate-mode GUI for the Settings / About windows",
                                 "https://github.com/emilk/egui");
                        link_row(ui, "microsoft / windows-rs",
                                 "Rust bindings for Win32 (host window + WndProc, registry, DPI, input)",
                                 "https://github.com/microsoft/windows-rs");
                    });

                    section(ui, t.about_contact, |ui| {
                        link_row(ui, "Website",
                                 "airplay.popyachsa.com",
                                 "https://airplay.popyachsa.com");
                        link_row(ui, "Blog",
                                 "recluse.lol",
                                 "https://recluse.lol");
                        link_row(ui, "GitHub",
                                 "@Recluse on github.com",
                                 "https://github.com/Recluse");
                        link_row(ui, "Email",
                                 "me@recluse.lol",
                                 "mailto:me@recluse.lol");
                        link_row(ui, "Telegram",
                                 "@recluseru",
                                 "https://t.me/recluseru");
                    });

                    section(ui, t.about_license, |ui| {
                        para(ui, t.about_license_body);
                    });

                    ui.add_space(20.0);
                });
            });
    }
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        ui.label(RichText::new(title.to_uppercase())
                 .color(TEXT_DIM).size(11.0).strong());
    });
    ui.add_space(4.0);
    let full = ui.available_width();
    egui::Frame::default()
        .fill(BG_PANEL)
        .inner_margin(Margin::symmetric(18.0, 14.0))
        .outer_margin(Margin::symmetric(20.0, 0.0))
        .rounding(Rounding::same(12.0))
        .show(ui, |ui| {
            ui.set_min_width(full - 40.0 - 36.0);
            add(ui);
        });
    ui.add_space(16.0);
}

/// A paragraph of normal body text — wrapped, dimmed.
fn para(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(TEXT_PRIM).size(13.0));
    ui.add_space(6.0);
}

/// Two-column row with a clickable hyperlink as the first line and a dim
/// help line underneath.
fn link_row(ui: &mut egui::Ui, label: &str, help: &str, url: &str) {
    ui.hyperlink_to(RichText::new(label).color(ACCENT_BLUE).size(14.0).strong(), url);
    if !help.is_empty() {
        ui.label(RichText::new(help).color(TEXT_DIM).size(11.0));
    }
    ui.add_space(6.0);
}
