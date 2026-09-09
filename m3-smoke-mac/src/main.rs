//! M3 prototype-GATE smoke (macOS) — the "moment of truth" for the macOS track.
//!
//! Mirrors `airplay-lib-smoke` (Windows): creates a `tao` window WE own, extracts
//! its `NSView*`, hands it to `uxplay-core.dylib` via `airplay-lib`, and runs the
//! tao event loop **on the main thread**. If an iPhone mirror renders INTO this
//! window, the load-bearing macOS architecture is validated:
//!   * `tao` owns the `NSApplication` run loop (NO `gst_macos_main`),
//!   * the engine runs on its own worker thread,
//!   * the sink renders into OUR `NSView` via `GstVideoOverlay`, with the bind +
//!     `setWantsLayer:` marshalled onto the main queue (video_renderer.c).
//!
//! This isolates the render question from the full tray-app integration (R3),
//! exactly as the macOS render gate prescribes.
//!
//! Run:
//!   ./m3-smoke-mac /path/to/uxplay-core.dylib
//! Then mirror from an iPhone; the AirPlay picker shows the device name below.
//! Close the window to stop — clean teardown relies on the L1 `gmainloop` fix.

use std::ffi::c_void;

use airplay_lib::AirPlay;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;

fn main() -> anyhow::Result<()> {
    let dll = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "uxplay-core.dylib".to_string());

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Popyachsa AirPlay — M3 macOS smoke")
        .with_inner_size(LogicalSize::new(1280.0, 720.0))
        .build(&event_loop)?;

    // Engine handle, created once the event loop is up (see StartCause::Init).
    let mut ap: Option<AirPlay> = None;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // The loop is now running on the main thread: the NSApplication run
            // loop is up and draining the main dispatch queue. Only NOW is it safe
            // to start the engine, so the worker thread's dispatch_sync(main_queue)
            // overlay bind in video_renderer.c executes instead of deadlocking.
            Event::NewEvents(StartCause::Init) => {
                let nsview: *mut c_void = match window.window_handle() {
                    Ok(h) => match h.as_raw() {
                        RawWindowHandle::AppKit(v) => v.ns_view.as_ptr(),
                        other => {
                            eprintln!("[m3] expected AppKit handle, got {other:?}");
                            *control_flow = ControlFlow::Exit;
                            return;
                        }
                    },
                    Err(e) => {
                        eprintln!("[m3] window_handle() failed: {e}");
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                };
                println!("[m3] tao window NSView = {nsview:?} — starting engine");

                let start = || -> anyhow::Result<AirPlay> {
                    let mut a = AirPlay::load(&dll)?;
                    a.set_device_name("Popyachsa AirPlay M3 smoke")?;
                    a.set_window(nsview)?;
                    // macOS sink/decoder defaults: glimagesink into
                    // our NSView, VideoToolbox decode, no vsync drops, no-close.
                    a.set_options(
                        "-vs glimagesink -vd vtdec -fps 60 -vsync no -nh -nohold -nc -FPSdata",
                    )?;
                    a.start()?;
                    Ok(a)
                };
                match start() {
                    Ok(a) => {
                        ap = Some(a);
                        println!("[m3] engine started — mirror from iPhone; close window to stop");
                    }
                    Err(e) => {
                        eprintln!("[m3] engine start FAILED: {e:?}");
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                println!("[m3] window closed — stopping engine (clean teardown)");
                if let Some(mut a) = ap.take() {
                    a.stop();
                }
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
