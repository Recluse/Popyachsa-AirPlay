//! Settings window — `popyachsa-airplay.exe --settings` spawns this in its own
//! process, runs an eframe event loop, lets the user edit Config, writes it
//! back to disk and exits. The main tray's config-watcher then picks up the
//! file change and restarts the engine with the new flags.

use eframe::egui::{self, Color32, FontId, Margin, RichText, Rounding, Stroke, Vec2};

use crate::config::{Config, APP_NAME};

const ACCENT_BLUE: Color32 = Color32::from_rgb(0x0A, 0x84, 0xFF);
const BG_DARK:     Color32 = Color32::from_rgb(0x18, 0x1A, 0x20);
const BG_PANEL:    Color32 = Color32::from_rgb(0x22, 0x25, 0x2D);
const BG_FIELD:    Color32 = Color32::from_rgb(0x2C, 0x30, 0x39);
const TEXT_PRIM:   Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF7);
const TEXT_DIM:    Color32 = Color32::from_rgb(0x9A, 0x9F, 0xAA);
const RED_DANGER:  Color32 = Color32::from_rgb(0xFF, 0x45, 0x3A);

const APP_ICON_PNG: &[u8] = include_bytes!("../icons/app.ico");

/// Try to ensure only one settings window is open at a time. Returns true if
/// we hold the lock and should proceed; returns false if another instance is
/// already running (in which case we tried to raise its window).
#[cfg(windows)]
fn acquire_single_instance(window_title: &str) -> bool {
    use windows::core::{PCWSTR, w};
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, FindWindowW, SetForegroundWindow, ShowWindow,
        SW_RESTORE,
    };
    // CreateMutexW returns a handle even if the mutex already existed; the
    // ERROR_ALREADY_EXISTS code tells us which.  We intentionally leak the
    // handle so the mutex stays alive for the lifetime of this process.
    // (Mutex name is language-independent; the window title may be localized.)
    let h = unsafe { CreateMutexW(None, false, w!("PopyachsaAirPlay.Settings.SingleInstance")) };
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already {
        // Try to raise the existing settings window by its (localized) title.
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
    // Keep handle alive — drop would close the mutex.
    std::mem::forget(h);
    true
}

// Non-Windows: no named-mutex single-instance yet (TODO: flock on
// $XDG_RUNTIME_DIR). eframe just opens a new Settings window if one is already up.
#[cfg(not(windows))]
fn acquire_single_instance(_window_title: &str) -> bool { true }

pub fn run(initial: Config) -> Result<(), eframe::Error> {
    // Localized window title (suffix follows the configured UI language).
    let t = crate::i18n::s(crate::i18n::Lang::from_config(&initial.language));
    let win_title = format!("{APP_NAME} — {}", t.settings.trim_end_matches('…'));

    // Bail out cleanly if a settings window is already open; raise it to the
    // foreground instead.
    if !acquire_single_instance(&win_title) {
        return Ok(());
    }

    // Load the .ico for the window title-bar icon.
    let icon_data = image::load_from_memory(APP_ICON_PNG)
        .ok()
        .map(|i| {
            let rgba = i.to_rgba8();
            let (w, h) = rgba.dimensions();
            egui::IconData { rgba: rgba.into_raw(), width: w, height: h }
        });

    // Center the window on the primary monitor (Windows). Elsewhere we let the
    // window manager place it (eframe/winit default).
    let win_w = 520.0_f32;
    let win_h = 640.0_f32;
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

    let mut native_options = eframe::NativeOptions::default();
    native_options.viewport = egui::ViewportBuilder::default()
        .with_title(&win_title)
        .with_inner_size([win_w, win_h])
        .with_min_inner_size([460.0, 560.0])
        .with_resizable(true);
    if let Some(p) = pos {
        native_options.viewport = native_options.viewport.clone().with_position(p);
    }
    if let Some(icon) = icon_data {
        native_options.viewport = native_options.viewport.with_icon(icon);
    }

    eframe::run_native(
        &win_title,
        native_options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
            crate::fonts::install(&cc.egui_ctx);
            Ok(Box::new(SettingsApp::new(initial)))
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

    // Slightly bigger fonts than egui default for an Apple-y feel.
    let mut style = (*ctx.style()).clone();
    use egui::TextStyle::*;
    style.text_styles.insert(Body,     FontId::proportional(14.0));
    style.text_styles.insert(Button,   FontId::proportional(14.0));
    style.text_styles.insert(Heading,  FontId::proportional(20.0));
    style.text_styles.insert(Monospace, FontId::monospace(13.0));
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 6.0);
    style.spacing.window_margin = Margin::same(18.0);
    ctx.set_style(style);
}

struct SettingsApp {
    original: Config,
    edited:   Config,
    save_ok_flash:  f32, // small post-save confirmation alpha
    copy_hint_flash: f32, // "path copied to clipboard" toast
}

impl SettingsApp {
    fn new(initial: Config) -> Self {
        Self { original: initial.clone(), edited: initial,
               save_ok_flash: 0.0, copy_hint_flash: 0.0 }
    }

    fn dirty(&self) -> bool { self.original != self.edited }

    fn save(&mut self) -> anyhow::Result<()> {
        self.edited.save()?;
        self.original = self.edited.clone();
        self.save_ok_flash = 1.0;
        Ok(())
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Localized strings for the currently-selected language (live preview).
        let t = crate::i18n::s(crate::i18n::Lang::from_config(&self.edited.language));
        // Fade out the green save-confirmation banner.
        if self.save_ok_flash > 0.0 {
            self.save_ok_flash = (self.save_ok_flash - ctx.input(|i| i.unstable_dt) * 0.5).max(0.0);
            ctx.request_repaint();
        }
        if self.copy_hint_flash > 0.0 {
            self.copy_hint_flash = (self.copy_hint_flash - ctx.input(|i| i.unstable_dt) * 0.7).max(0.0);
            ctx.request_repaint();
        }

        // Bottom action bar (Cancel / Save) — fixed at bottom regardless of scroll.
        egui::TopBottomPanel::bottom("bottom_bar")
            .frame(egui::Frame::default()
                .fill(BG_PANEL)
                .inner_margin(Margin::symmetric(18.0, 12.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Compact "Config file" hint: hover for full path, click to copy.
                    let cfg_path = crate::config::config_path();
                    let cfg_str  = cfg_path.display().to_string();
                    let label = RichText::new("📄 Config file")
                        .color(ACCENT_BLUE).size(12.0).underline();
                    let resp = ui.add(egui::Label::new(label)
                                       .sense(egui::Sense::click()));
                    let resp = resp.on_hover_text(&cfg_str);
                    if resp.clicked() {
                        ctx.copy_text(cfg_str.clone());
                        self.save_ok_flash = 0.0; // re-use flash to display below
                        self.copy_hint_flash = 1.0;
                    }

                    // Push Cancel/Save to the right edge.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let save_color = if self.dirty() { ACCENT_BLUE } else { BG_FIELD };
                        let save = egui::Button::new(
                            RichText::new(t.save).size(14.0).color(Color32::WHITE).strong())
                            .fill(save_color);
                        let save_resp = ui.add_sized([100.0, 30.0], save);
                        if save_resp.clicked() && self.dirty() {
                            if let Err(e) = self.save() {
                                eprintln!("[settings] save: {e}");
                            }
                        }
                        ui.add_space(8.0);
                        let cancel = egui::Button::new(RichText::new(t.cancel).size(14.0))
                            .fill(BG_FIELD);
                        if ui.add_sized([90.0, 30.0], cancel).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });

                if self.save_ok_flash > 0.0 {
                    let alpha = (self.save_ok_flash * 255.0) as u8;
                    let green = Color32::from_rgba_premultiplied(0x34, 0xC7, 0x59, alpha);
                    ui.colored_label(green, t.saved_flash);
                }
                if self.copy_hint_flash > 0.0 {
                    let alpha = (self.copy_hint_flash * 255.0) as u8;
                    let blue = Color32::from_rgba_premultiplied(0x0A, 0x84, 0xFF, alpha);
                    ui.colored_label(blue, "📋 Path copied to clipboard");
                }
            });

        // Main scrollable area
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(BG_DARK).inner_margin(Margin::same(0.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                    ui.add_space(20.0);
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.heading(RichText::new(APP_NAME).color(TEXT_PRIM));
                    });
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.label(RichText::new(t.settings.trim_end_matches('…'))
                                 .color(TEXT_DIM));
                    });
                    ui.add_space(18.0);

                    self.section(ui, t.sec_device, |ui, edited| {
                        labelled_row(ui, t.lbl_language, t.help_language, |ui| {
                            let cur = if edited.language.is_empty() || edited.language.eq_ignore_ascii_case("auto") {
                                "Auto".to_string()
                            } else {
                                crate::i18n::Lang::from_config(&edited.language).native_name().to_string()
                            };
                            egui::ComboBox::from_id_salt("language").selected_text(cur).show_ui(ui, |ui| {
                                ui.selectable_value(&mut edited.language, "auto".to_string(), "Auto");
                                for l in crate::i18n::Lang::all() {
                                    ui.selectable_value(&mut edited.language, l.code().to_string(), l.native_name());
                                }
                            });
                        });
                        labelled_row(ui, t.lbl_device_name, t.help_device_name, |ui| {
                            ui.add(egui::TextEdit::singleline(&mut edited.device_name)
                                   .desired_width(260.0));
                        });
                        // Monitor selector — only shown when >1 display present.
                        let mons = crate::monitors::list();
                        if mons.len() > 1 {
                            labelled_row(ui, t.lbl_display,
                                t.help_display,
                                |ui| {
                                let cur_text = match edited.preferred_monitor {
                                    None => "Primary".to_string(),
                                    Some(i) => mons.iter()
                                        .find(|m| m.index == i)
                                        .map(|m| format!("Display {} ({}x{})",
                                                         i + 1, m.width(), m.height()))
                                        .unwrap_or_else(|| format!("Display {} (missing)", i + 1)),
                                };
                                egui::ComboBox::from_id_salt("monitor")
                                    .selected_text(cur_text)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut edited.preferred_monitor,
                                                            None, "Primary");
                                        for m in &mons {
                                            let label = format!(
                                                "Display {}{} ({}x{})",
                                                m.index + 1,
                                                if m.primary { " — primary" } else { "" },
                                                m.width(), m.height());
                                            ui.selectable_value(&mut edited.preferred_monitor,
                                                                Some(m.index), label);
                                        }
                                    });
                            });
                        }
                    });

                    self.section(ui, t.sec_autostart, |ui, edited| {
                        // "Start with Windows" (HKCU Run) / "Start at login"
                        // (macOS login item or Linux XDG autostart).
                        #[cfg(windows)]
                        let (start_lbl, start_help) = (t.lbl_start_with_windows, t.help_start_with_windows);
                        #[cfg(target_os = "macos")]
                        let (start_lbl, start_help): (&str, &str) =
                            ("Start at login", "Launch automatically when you log in.");
                        #[cfg(all(not(windows), not(target_os = "macos")))]
                        let (start_lbl, start_help): (&str, &str) = (
                            "Start at login",
                            "Add an XDG autostart entry (~/.config/autostart) so the app starts at login.",
                        );
                        checkbox_row(ui, &mut edited.autostart_with_windows, start_lbl, start_help);
                        checkbox_row(ui, &mut edited.autostart_on_app_launch,
                                     t.lbl_autostart_launch,
                                     t.help_autostart_launch);
                    });

                    self.section(ui, t.sec_video, |ui, edited| {
                        checkbox_row(ui, &mut edited.fullscreen,
                                     t.lbl_fullscreen,
                                     t.help_fullscreen);
                        // Window chrome (always-on-top / borderless) is Win32-only
                        // for now; the X11 host window gets _NET_WM_STATE later.
                        #[cfg(windows)]
                        checkbox_row(ui, &mut edited.always_on_top,
                                     t.lbl_always_on_top,
                                     t.help_always_on_top);
                        #[cfg(windows)]
                        checkbox_row(ui, &mut edited.borderless,
                                     t.lbl_borderless,
                                     t.help_borderless);
                        // h265 — Windows only (Linux v1 forces h264: this UxPlay
                        // fork asserts on h265 reconnect).
                        #[cfg(windows)]
                        checkbox_row(ui, &mut edited.enable_h265,
                                     t.lbl_h265,
                                     t.help_h265);
                        labelled_row(ui, t.lbl_decoder,
                            t.help_decoder, |ui| {
                            // (label, config value) — per-OS hardware decoders.
                            #[cfg(windows)]
                            let opts: &[(&str, &str)] = &[
                                ("Direct3D 11", "d3d11"),
                                ("Direct3D 12", "d3d12"),
                                ("NVIDIA (NVDEC)", "nvidia"),
                            ];
                            #[cfg(target_os = "macos")]
                            let opts: &[(&str, &str)] = &[
                                ("VideoToolbox (auto)", "videotoolbox"),
                                ("Software (avdec)", "software"),
                            ];
                            #[cfg(all(not(windows), not(target_os = "macos")))]
                            let opts: &[(&str, &str)] = &[
                                ("Auto (recommended)", "auto"),
                                ("Software", "software"),
                                ("VA-API (Intel/AMD)", "vaapi"),
                                ("NVIDIA (NVDEC)", "nvidia"),
                            ];
                            let cur = opts.iter().find(|(_, v)| *v == edited.video_decoder)
                                .map(|(l, _)| *l).unwrap_or(opts[0].0);
                            egui::ComboBox::from_id_salt("video_decoder")
                                .selected_text(cur)
                                .show_ui(ui, |ui| {
                                    for &(label, value) in opts {
                                        ui.selectable_value(&mut edited.video_decoder,
                                                            value.to_string(), label);
                                    }
                                });
                        });
                        labelled_row(ui, t.lbl_fps,
                            t.help_fps, |ui| {
                            // Common monitor refresh rates covering the realistic ceiling.
                            // Anything > 240 has been observed to make uxplay refuse the stream.
                            let opts: &[u32] = &[30, 60, 75, 90, 100, 120, 144, 165, 180, 200, 240];
                            egui::ComboBox::from_id_salt("fps")
                                .selected_text(format!("{} fps", edited.target_fps))
                                .show_ui(ui, |ui| {
                                    for &v in opts {
                                        ui.selectable_value(&mut edited.target_fps, v,
                                                            format!("{v} fps"));
                                    }
                                });
                        });
                    });

                    self.section(ui, t.sec_audio, |ui, edited| {
                        labelled_row(ui, t.lbl_audio_renderer,
                            t.help_audio, |ui| {
                            // (label, GStreamer sink value) — per-OS audio sinks.
                            #[cfg(windows)]
                            let opts: &[(&str, &str)] = &[
                                ("WASAPI", "wasapisink"),
                                ("WASAPI (exclusive)", "wasapi2sink"),
                                ("System default", "autoaudiosink"),
                                ("Off", ""),
                            ];
                            #[cfg(target_os = "macos")]
                            let opts: &[(&str, &str)] = &[
                                ("System default", "autoaudiosink"),
                                ("Core Audio", "osxaudiosink"),
                                ("Off", ""),
                            ];
                            #[cfg(all(not(windows), not(target_os = "macos")))]
                            let opts: &[(&str, &str)] = &[
                                ("System default", "autoaudiosink"),
                                ("PulseAudio", "pulsesink"),
                                ("PipeWire", "pipewiresink"),
                                ("ALSA", "alsasink"),
                                ("Off", ""),
                            ];
                            let cur = opts.iter().find(|(_, v)| *v == edited.audio_sink)
                                .map(|(l, _)| *l).unwrap_or(opts[0].0);
                            egui::ComboBox::from_id_salt("audio_sink")
                                .selected_text(cur)
                                .show_ui(ui, |ui| {
                                    for &(label, value) in opts {
                                        ui.selectable_value(&mut edited.audio_sink,
                                                            value.to_string(), label);
                                    }
                                });
                        });
                    });

                    self.section(ui, t.sec_advanced, |ui, edited| {
                        checkbox_row(ui, &mut edited.check_updates_on_launch,
                                     t.lbl_check_updates,
                                     t.help_check_updates);
                        checkbox_row(ui, &mut edited.debug_logging,
                                     t.lbl_debug,
                                     t.help_debug);
                        labelled_row(ui, t.lbl_custom_flags,
                            t.help_custom_flags, |ui| {
                            ui.add(egui::TextEdit::singleline(&mut edited.custom_flags)
                                   .desired_width(260.0).hint_text("--option value"));
                        });
                    });

                    if self.dirty() {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.colored_label(RED_DANGER, "Unsaved changes");
                        });
                    }
                    ui.add_space(20.0);
                });
            });
    }
}

impl SettingsApp {
    fn section(&mut self, ui: &mut egui::Ui, title: &str,
               add: impl FnOnce(&mut egui::Ui, &mut Config)) {
        // Section title above the panel.
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.label(RichText::new(title.to_uppercase())
                     .color(TEXT_DIM).size(11.0).strong());
        });
        ui.add_space(4.0);
        // Panel — outer margin gives the left+right indent, inner margin gives
        // the inside padding. set_min_width inside guarantees all sections come
        // out the same width regardless of how much content each one has.
        let full = ui.available_width();
        egui::Frame::default()
            .fill(BG_PANEL)
            .inner_margin(Margin::symmetric(18.0, 14.0))
            .outer_margin(Margin::symmetric(20.0, 0.0))
            .rounding(Rounding::same(12.0))
            .show(ui, |ui| {
                ui.set_min_width(full - 40.0 - 36.0); // - outer - 2*inner
                add(ui, &mut self.edited);
            });
        ui.add_space(16.0);
    }
}

/// Two-line row: a label on the left + a right-aligned control + a dim help
/// line spanning the full width underneath.
fn labelled_row(ui: &mut egui::Ui, label: &str, help: &str,
                add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(TEXT_PRIM).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            add(ui);
        });
    });
    if !help.is_empty() {
        ui.label(RichText::new(help).color(TEXT_DIM).size(11.0));
    }
    ui.add_space(8.0);
}

fn checkbox_row(ui: &mut egui::Ui, value: &mut bool, label: &str, help: &str) {
    ui.vertical(|ui| {
        ui.checkbox(value, RichText::new(label).color(TEXT_PRIM));
        if !help.is_empty() {
            ui.label(RichText::new(help).color(TEXT_DIM).size(11.0));
        }
    });
    ui.add_space(8.0);
}
