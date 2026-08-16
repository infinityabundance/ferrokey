//! # ferrokey — the on-screen keyboard
//!
//! Unprivileged UI. Owns no input devices: every key event goes to
//! `ferrokeyd` over the authenticated Unix socket, which owns `/dev/uinput`.
//!
//! Slint renders and hit-tests; `ferrokey-surface` owns the window semantics
//! (layer-shell `keyboard_interactivity = none` on Wayland, ICCCM
//! `WM_HINTS.input = False` on X11); `ferrokey-core` owns the key semantics.
//!
//! # Terminal workspace mode (`ferrokey --terminal`)
//!
//! The same keyboard drives an embedded PTY terminal directly — no
//! `ferrokeyd`, no `/dev/uinput`, no compositor focus dependency
//! (Phase 3 addendum #2, §1–§5). Input routing goes through one decision
//! point ([`input::InputRouter`]): SYSTEM keys flow to the daemon, TERMINAL
//! keys flow to the PTY. The pane below the OSK is rendered by
//! `ferrokey-terminal` into an opaque RGBA frame displayed as a Slint image;
//! all pane interaction (tap/drag/long-press) is handled by the Rust
//! terminal-input bridge (`term_input`), never by Slint TouchAreas.

mod config;
mod daemon;
mod input;
mod pointer;
mod term_input;
mod text;
mod tray;
mod views;

use anyhow::Context;
use config::UiConfig;
use daemon::{DaemonLink, LinkState};
use ferrokey_core::{
    KeyAction, KeySymbol, KeyboardDriver, ModifierSet, RepeatSettings, StateSettings, VirtualKey,
};
use ferrokey_layouts::builtin;
use ferrokey_surface::slint_adapter::FerrokeyPlatform;
use ferrokey_surface::{
    detect, fallback::NullSurface, PointerButton, Surface, SurfaceBackend, SurfaceEvent,
};
use ferrokey_terminal::{
    Terminal as TerminalEngine, TerminalConfig, TerminalKeyEncoder, TerminalKeySink,
};
use input::{Destination, InputRouter};
use slint::{Image, ModelRc};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

slint::include_modules!();

/// Minimum/maximum uniform keyboard scale (the window can resize the whole
/// OSK; 1.0 = the view's natural size).
const MIN_VIEW_SCALE: f32 = 0.35;
const MAX_VIEW_SCALE: f32 = 3.0;

/// The single monotonic epoch for the keyboard engine's time input: captured
/// once at process start, so every `Moment` across modules shares one clock
/// (mixed epochs would corrupt tap/latch timing).
static STARTED: OnceLock<Instant> = OnceLock::new();

/// Interactive-window gesture: drag the top edge to move, drag the
/// bottom-right corner to resize. The OSK is positioned like a normal window
/// while keeping its no-focus contract (`keyboard_interactivity = none` on
/// Wayland, `WM_HINTS.input = False` on X11) — the surface API's move/resize
/// only ever touches position/size. Presses inside a grip zone are swallowed
/// by this layer and never reach the pointer bridge, so a grab can neither
/// fire a key nor leave a ghost hold.
enum WindowGesture {
    Idle,
    Move {
        /// The window's top-left at grab time (and updated after every
        /// move), so the drag is self-contained: the target is always
        /// `window position + pointer delta`, never a re-read of surface
        /// state that might have diverged (e.g. after a hide/show cycle).
        win_x: i32,
        win_y: i32,
        grab_x: f64,
        grab_y: f64,
    },
    Resize {
        start_scale: f32,
        grab_x: f64,
        grab_y: f64,
        /// The terminal pane height (physical px) captured at grab time;
        /// 0 outside terminal mode. The window height on resize is the
        /// scaled keyboard PLUS this constant pane (§25: the pane is not
        /// part of the keyboard scale).
        pane_phys: u32,
    },
}

/// The top edge band (logical px of the title strip) that drags the window;
/// the gesture converts it to physical px via the platform scale.
const MOVE_BAND: f64 = 22.0;
/// The bottom-right corner (physical px) that resizes the window.
const RESIZE_CORNER: f64 = 28.0;

/// The current engine time (`ferrokey-core`'s deterministic `Moment`).
pub(crate) fn now_moment() -> ferrokey_core::Moment {
    ferrokey_core::Moment::from_elapsed(*STARTED.get_or_init(Instant::now))
}

fn main() -> anyhow::Result<()> {
    init_logging();
    // `--diagnose` / `diagnose`: print the terminal-workspace diagnostics and
    // exit without touching the display (§86).
    if std::env::args().any(|a| a == "--diagnose" || a == "diagnose") {
        let config = load_ui_config()?;
        return diagnose(&config);
    }
    let config = load_ui_config()?;
    run(&config)
}

/// The terminal diagnostics report (§86). Never reveals typed content.
fn diagnose(config: &UiConfig) -> anyhow::Result<()> {
    let terminal = TerminalEngine::new(TerminalConfig {
        scrollback_lines: config.terminal.scrollback_lines,
        font_size_px: config.terminal.font_size_px,
        shell: config.terminal.shell.clone(),
        home: None,
        env: vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
        ],
        max_paste_bytes: ferrokey_terminal::limits::MAX_PASTE_BYTES,
        confirm_multiline_paste: config.terminal.confirm_multiline_paste,
    })
    .context("terminal engine init")?;
    println!("ferrokey diagnostics");
    println!("  destination: {}", config.destination);
    println!("  terminal mode: {}", config.terminal.enabled);
    for (key, value) in terminal.diagnostics() {
        println!("  {key}: {value}");
    }
    Ok(())
}

/// A tiny stderr logger (the UI deliberately has no heavy logging deps).
/// Courts inspect `ferrokey.log` for backend selection evidence, so the
/// backend/detail lines must actually be emitted.
struct StderrLogger(log::LevelFilter);

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.0
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "{} [{:5}] {}",
                record.level(),
                record.level(),
                record.args()
            );
        }
    }
    fn flush(&self) {}
}

fn init_logging() {
    let level = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "info".into())
        .to_ascii_lowercase();
    let level = match level.as_str() {
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    };
    let _ = log::set_boxed_logger(Box::new(StderrLogger(level)));
    log::set_max_level(level);
}

/// Parse the command line. Flags accumulate over the default config (the
/// historical `--layout`/`--view` shortcuts are preserved).
fn load_ui_config() -> anyhow::Result<UiConfig> {
    let mut explicit: Option<std::path::PathBuf> = None;
    let mut layout: Option<String> = None;
    let mut view: Option<String> = None;
    let mut terminal = false;
    let mut terminal_height: Option<u32> = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                explicit = Some(std::path::PathBuf::from(
                    args.get(i).context("--config requires a path")?,
                ));
            }
            "--layout" => {
                i += 1;
                layout = Some(args.get(i).context("--layout requires an id")?.clone());
            }
            "--view" => {
                i += 1;
                view = Some(args.get(i).context("--view requires an id")?.clone());
            }
            "--terminal" => {
                terminal = true;
            }
            "--terminal-height" => {
                i += 1;
                let raw = args.get(i).context("--terminal-height requires px")?;
                let px: u32 = raw
                    .parse()
                    .context("--terminal-height must be a pixel count")?;
                if !(200..=4000).contains(&px) {
                    anyhow::bail!("--terminal-height must be between 200 and 4000 px");
                }
                terminal_height = Some(px);
            }
            "--help" | "-h" => {
                println!(
                    "ferrokey — on-screen keyboard with focus preservation\n\n\
                     USAGE:\n  \
                     ferrokey [--config <path>] [--layout <id>] [--view <compact|full>]\n  \
                     \x20           [--terminal] [--terminal-height <px>]\n\n\
                     --terminal         start in embedded terminal workspace mode\n\
                     \x20                (no ferrokeyd, no /dev/uinput; direct OSK → PTY)\n\
                     --terminal-height  terminal pane height in physical px (default 420)"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
        i += 1;
    }
    let mut config = if let Some(path) = explicit {
        UiConfig::load(&path)?
    } else {
        let mut found = None;
        for candidate in UiConfig::default_paths() {
            if candidate.exists() {
                found = Some(candidate);
                break;
            }
        }
        match found {
            Some(path) => UiConfig::load(&path)?,
            None => UiConfig::default(),
        }
    };
    if let Some(l) = layout {
        config.layout = l;
    }
    if let Some(v) = view {
        config.view = v;
    }
    if terminal {
        config.terminal.enabled = true;
        config.destination = "terminal".into();
    }
    if let Some(px) = terminal_height {
        config.terminal.pane_height = px;
    }
    Ok(config)
}

fn run(config: &UiConfig) -> anyhow::Result<()> {
    // ── Keyboard view (visual arrangement over the one key engine) ──────────
    let view = views::view(&config.view).with_context(|| {
        format!(
            "unknown view {:?} (views: {})",
            config.view,
            views::VIEW_IDS.join(", ")
        )
    })?;
    let keyboard_w = view.width;
    let keyboard_h = view.height;
    log::info!(
        "view: {} ({}x{}, base width {})",
        view.name,
        view.width,
        view.height,
        view.base_width
    );
    views::check_geometry(view);

    // ── Terminal workspace mode ───────────────────────────────────────────
    let terminal_cfg = &config.terminal;
    // The window is the fixed 22px title strip PLUS the keyboard, sized at
    // the configured initial scale (1.0 = the view's natural size; the
    // shipped default is 0.75). The runtime transform re-derives the exact
    // scale from the window size.
    let title_h = views::TITLE_H as u32;
    let scale = config.scale.clamp(MIN_VIEW_SCALE, MAX_VIEW_SCALE);
    let win_width = (keyboard_w as f32 * scale).round() as u32;
    let win_height = (keyboard_h as f32 * scale).round() as u32;
    let (win_width, win_height) = if terminal_cfg.enabled {
        let pane_h = terminal_cfg.pane_height.max(200);
        log::info!(
            "terminal workspace mode: pane {}px, font {}px, scrollback {}, scale {scale:.2}",
            pane_h,
            terminal_cfg.font_size_px,
            terminal_cfg.scrollback_lines
        );
        (win_width, win_height + pane_h + title_h)
    } else {
        log::info!("initial keyboard scale {scale:.2} (window {win_width}x{win_height})");
        (win_width, win_height + title_h)
    };

    // ── Surface detection (capability-driven, never compositor-name based) ──
    let detection = detect::detect();
    log::info!(
        "surface backend: {} ({})",
        detection.backend.name(),
        detection.detail
    );

    let surface: Box<dyn Surface> = match detection.backend {
        SurfaceBackend::WaylandLayerShell => {
            let surface = ferrokey_surface::wayland::WaylandSurface::connect()
                .context("wayland layer-shell connect failed")?;
            Box::new(surface)
        }
        SurfaceBackend::X11NoInput => {
            let options = ferrokey_surface::x11::X11Options {
                display: detection
                    .x11_display
                    .clone()
                    .or_else(|| config.x11_display.clone()),
                override_redirect: true,
                ..Default::default()
            };
            let surface = ferrokey_surface::x11::X11Surface::create(options)
                .context("x11 surface create failed")?;
            Box::new(surface)
        }
        SurfaceBackend::WaylandDegraded | SurfaceBackend::None => Box::new(NullSurface::new()),
    };

    // ── Platform + Slint window ────────────────────────────────────────────
    let platform = FerrokeyPlatform::new(surface, win_width, win_height);
    slint::platform::set_platform(Box::new(ferrokey_surface::slint_adapter::PlatformHandle(
        platform.clone(),
    )))
    .map_err(|e| anyhow::anyhow!("set_platform: {e}"))?;

    let ui = MainWindow::new().map_err(|e| anyhow::anyhow!("ui init: {e}"))?;
    platform.set_size(win_width, win_height)?;
    ui.set_degraded(config.force_degraded_banner || !detection.backend.preserves_focus());
    ui.set_status_line(detection.detail.clone().into());
    ui.set_text_mode(config.text_mode);
    if config.text_mode {
        log::info!("text mode enabled");
    }
    if terminal_cfg.enabled {
        ui.set_terminal_mode(true);
        let scale = platform.scale().max(0.25);
        ui.set_terminal_height(terminal_cfg.pane_height.max(200) as f32 / scale);
    }
    ui.set_destination(config.destination.clone().into());

    // ── Layout ─────────────────────────────────────────────────────────────
    let layout = if looks_like_xkb_spec(&config.layout) {
        match ferrokey_layouts::xkb::load_system_layout(&config.layout) {
            Some(layout) => {
                log::info!("layout: system xkb {:?} ({})", config.layout, layout.id);
                Arc::new(layout)
            }
            None => anyhow::bail!(
                "layout {:?}: XKB variant spec could not be loaded (is xkb data installed?)",
                config.layout
            ),
        }
    } else if ferrokey_layouts::BUILTIN_IDS.contains(&config.layout.as_str()) {
        let layout = builtin(&config.layout).with_context(|| {
            format!(
                "unknown layout {:?} (builtins: {})",
                config.layout,
                ferrokey_layouts::BUILTIN_IDS.join(", ")
            )
        })?;
        Arc::new(layout)
    } else {
        match ferrokey_layouts::xkb::load_system_layout(&config.layout) {
            Some(layout) => {
                log::info!("layout: system xkb {:?} ({})", config.layout, layout.id);
                Arc::new(layout)
            }
            None => anyhow::bail!(
                "unknown layout {:?} (builtins: {})",
                config.layout,
                ferrokey_layouts::BUILTIN_IDS.join(", ")
            ),
        }
    };
    set_keyboard_view(&ui, &layout, view);

    // ── Core driver over the input router ─────────────────────────────────
    // The daemon link (system destination). In terminal-only mode there is
    // no link at all (§4–§5): the terminal keeps working with no daemon.
    let link = Rc::new(RefCell::new(DaemonLink::new(config.socket_path.clone())));
    let system_sink: Option<Rc<RefCell<DaemonLink>>> = if terminal_cfg.enabled {
        log::info!("terminal-only mode: ferrokeyd connection disabled");
        None
    } else {
        Some(link.clone())
    };

    // The terminal engine (terminal destination).
    let terminal: Option<Rc<RefCell<TerminalEngine>>> = if terminal_cfg.enabled {
        let engine = TerminalEngine::new(TerminalConfig {
            scrollback_lines: terminal_cfg.scrollback_lines,
            font_size_px: terminal_cfg.font_size_px,
            shell: terminal_cfg.shell.clone(),
            home: None,
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
            max_paste_bytes: ferrokey_terminal::limits::MAX_PASTE_BYTES,
            confirm_multiline_paste: terminal_cfg.confirm_multiline_paste,
        })
        .context("terminal engine init")?;
        Some(Rc::new(RefCell::new(engine)))
    } else {
        None
    };

    let terminal_sink: Option<Box<dyn ferrokey_core::KeySink>> = terminal.as_ref().map(|term| {
        let encoder = TerminalKeyEncoder::new(layout.clone());
        let sink = ferrokey_terminal::PtySink::new();
        let buffer = sink.buffer();
        // The terminal drains this buffer during its poll cycle.
        term.borrow_mut().set_input_source(buffer);
        let modes = term.borrow().modes_cell();
        Box::new(TerminalKeySink::new(encoder, modes, Box::new(sink)))
            as Box<dyn ferrokey_core::KeySink>
    });
    // The copy/paste overlay buttons use an unprivileged clipboard backend.
    if let Some(term) = &terminal {
        term.borrow_mut()
            .set_clipboard(Box::new(ferrokey_terminal::ExternalClipboard::detect()));
    }

    let initial_destination = if config.destination == "terminal" {
        Destination::Terminal
    } else {
        Destination::System
    };
    let router = Rc::new(RefCell::new(InputRouter::new(
        system_sink.clone(),
        terminal_sink,
        initial_destination,
    )));

    let sink: Box<dyn ferrokey_core::KeySink> = Box::new(input::RouterSink(router.clone()));
    let driver = Rc::new(RefCell::new(KeyboardDriver::new(
        StateSettings {
            latch_enabled: config.sticky.latch_enabled,
            tap_timeout: Duration::from_millis(config.sticky.tap_timeout_ms),
            ..Default::default()
        },
        RepeatSettings {
            enabled: config.repeat.enabled,
            delay: Duration::from_millis(config.repeat.delay_ms),
            cadence: Duration::from_millis(config.repeat.cadence_ms),
        },
        layout.clone(),
        sink,
    )));
    // Text-mode composer: dead keys + compose sequences over the layout.
    let composer = Rc::new(RefCell::new(text::TextComposer::new(layout.clone())));
    log::info!(
        "driver: repeat enabled={} delay={}ms cadence={}ms latch={} tap={}ms",
        config.repeat.enabled,
        config.repeat.delay_ms,
        config.repeat.cadence_ms,
        config.sticky.latch_enabled,
        config.sticky.tap_timeout_ms
    );

    // ── Wire the UI callbacks ──────────────────────────────────────────────
    let ui_handle = ui.as_weak();
    let driver_cb = driver.clone();
    let link_cb = link.clone();
    let layout_cb = layout.clone();
    let composer_cb = composer.clone();
    let router_cb = router.clone();
    ui.on_key_pressed(move |name| {
        if let Some(ui) = ui_handle.upgrade() {
            let _ = handle_key(
                &driver_cb,
                &link_cb,
                &layout_cb,
                &composer_cb,
                &router_cb,
                &ui,
                KeyAction::Down,
                &name,
            );
        }
    });
    let ui_handle = ui.as_weak();
    let driver_cb = driver.clone();
    let link_cb = link.clone();
    let layout_cb = layout.clone();
    let composer_cb = composer.clone();
    let router_cb = router.clone();
    ui.on_key_released(move |name| {
        if let Some(ui) = ui_handle.upgrade() {
            let _ = handle_key(
                &driver_cb,
                &link_cb,
                &layout_cb,
                &composer_cb,
                &router_cb,
                &ui,
                KeyAction::Up,
                &name,
            );
        }
    });

    // Destination switch (the badge is tappable in terminal mode, §112).
    let ui_handle = ui.as_weak();
    let driver_cb = driver.clone();
    let router_cb = router.clone();
    ui.on_destination_switch(move || {
        if let Some(ui) = ui_handle.upgrade() {
            let next = match router_cb.borrow().active() {
                Destination::System => Destination::Terminal,
                Destination::Terminal => Destination::System,
            };
            // Release held state on the old destination BEFORE switching
            // (§62–§63): no down/up may cross the boundary.
            let _ = driver_cb.borrow_mut().emergency_release();
            router_cb.borrow_mut().set_active(next);
            ui.set_destination(next.label().into());
            log::info!("input destination: {}", next.label());
        }
    });

    // ── Event loop ─────────────────────────────────────────────────────────
    platform.set_visible(true)?;
    log::info!("ferrokey running ({win_width}x{win_height})");

    // Start the terminal session (spawn the shell on a fresh PTY) and size
    // the pane to the configured height (§6–§8).
    if let Some(term) = &terminal {
        let mut t = term.borrow_mut();
        let pane_h = terminal_cfg.pane_height.max(200);
        let _ = t.resize(keyboard_w, pane_h);
        let mut shell = ferrokey_terminal::ShellConfig::default();
        shell.shell.clone_from(&terminal_cfg.shell);
        shell.env = vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
        ];
        match t.start_session(&shell) {
            Ok(()) => log::info!("terminal session started"),
            Err(e) => log::error!("terminal session start failed: {e}"),
        }
        drop(t);
    }

    let adaptive = if config.adaptive.enabled {
        // Adaptive key geometry (WS4): the OSK learns touch placement and
        // adapts the effective hit targets. Built from the view's visual
        // geometry; the initial hit regions equal the visual rects, so the
        // OSK behaves identically until it has learned.
        let (rects, neighbors, _names) = views::adaptive_geometry_basis(view);
        let ag = ferrokey_core::geometry::AdaptiveGeometry::new(
            ferrokey_core::geometry::AdaptiveConfig {
                enabled: true,
                frozen: config.adaptive.frozen,
                min_samples: config.adaptive.min_samples,
                optimize_every: config.adaptive.optimize_every,
                ..Default::default()
            },
            ferrokey_core::geometry::GeometryConstraints::default(),
            &rects,
            &neighbors,
        );
        Some((ag, config.adaptive.evidence_confidence))
    } else {
        None
    };
    let mut bridge = pointer::PointerBridge::new(view, platform.scale(), adaptive);
    let mut terminal_input = term_input::TerminalInput::default();

    // Apply the window→view transform (uniform keyboard scale + centering
    // below the title strip) once at startup; Resized events re-apply it.
    // In terminal mode the window height includes the fixed pane, so the
    // scale is derived from the keyboard's own height budget only.
    let initial_pane_phys = if terminal_cfg.enabled {
        (terminal_cfg.pane_height.max(200) as f32 * platform.scale().max(0.25)).round() as u32
    } else {
        0
    };
    let s0 = apply_view_transform(&ui, &platform, &mut bridge, view, initial_pane_phys);
    let mut keyboard_h_phys =
        ((views::TITLE_H + view.height as f32 * s0) * platform.scale().max(0.25)) as u32;

    // WS5: shell-aware terminal rows. The initial identity is KNOWN from the
    // child Ferrokey itself spawned (§5.2); later transitions are learned
    // from the process tree (§5.3), throttled in the UI loop. Rows are
    // presentation-only (§5.10).
    let shell_row_id: std::cell::RefCell<&'static str> = std::cell::RefCell::new("generic");
    if let Some(term) = &terminal {
        let spawned = terminal_cfg
            .shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".into());
        let ctx = ferrokey_terminal::shell::ShellContext::from_spawned_shell(&spawned);
        let id = ctx.row_id();
        if ctx.is_confident() {
            let row = ferrokey_terminal::shell::shell_row(id);
            bridge.set_shell_row(Some(row));
            set_terminal_shortcut_row(&ui, view, Some(row));
            *shell_row_id.borrow_mut() = id;
            log::info!("terminal shell context: {id} (spawned child)");
        }
        let _ = term; // the terminal itself keeps running; the context probe
                      // below inspects its child's process tree
    }

    // System tray presence (no-op without a session DBus, e.g. the courts).
    let tray = tray::Tray::start();

    let mut last_ping = Instant::now();
    let mut last_status_update = Instant::now();
    let mut last_shell_probe = Instant::now();
    let mut gesture = WindowGesture::Idle;
    // The current uniform keyboard scale (tracked for the resize gesture).
    let mut current_view_scale = s0;
    // OSK visibility (the tray can show/hide it).
    let mut visible = true;
    let mut last_lit: (bool, bool, bool, bool, bool, bool, bool, bool) =
        (false, false, false, false, false, false, false, false);
    loop {
        // Tray commands (show/hide, quit) are polled every frame.
        if let Some(t) = &tray {
            match t.command() {
                tray::TrayCommand::Toggle => {
                    visible = !visible;
                    if let Err(e) = platform.set_visible(visible) {
                        log::warn!("tray toggle failed: {e}");
                    }
                    log::info!("OSK visibility -> {visible}");
                }
                tray::TrayCommand::None => {}
            }
        }
        bridge.set_scale(platform.scale());

        // 1. Surface events: dispatched into Slint (visuals) AND returned so
        //    the pointer bridge / terminal-input bridge can drive semantics.
        let events = match platform.process_events(Some(Duration::from_millis(10))) {
            Ok(events) => events,
            Err(e) => {
                log::error!("surface event error: {e}");
                break;
            }
        };
        let now = Instant::now();
        // The window size is the frame's gesture/pane geometry baseline
        // (used for the resize-corner hit-tests in the pane router below).
        let win_size = platform.size();
        let (gw, gh) = (win_size.width, win_size.height);
        for event in events {
            // A window resize (user gesture or compositor) re-derives the
            // keyboard scale + offset, and the terminal pane boundary. In
            // terminal mode the pane is CONSTANT across keyboard resizes (the
            // gesture keeps it), so while a resize gesture is active its
            // captured pane is authoritative for the scale derivation —
            // deriving it from `height − old keyboard height` would inflate
            // the pane by the keyboard's own growth and clamp the scale at
            // the previous size (the window would only ever grow wider).
            if let SurfaceEvent::Resized { height, .. } = event {
                let pane_phys = match &gesture {
                    WindowGesture::Resize { pane_phys, .. } => *pane_phys,
                    _ if terminal_cfg.enabled => height.saturating_sub(keyboard_h_phys),
                    _ => 0,
                };
                let s = apply_view_transform(&ui, &platform, &mut bridge, view, pane_phys);
                current_view_scale = s;
                keyboard_h_phys =
                    ((views::TITLE_H + view.height as f32 * s) * platform.scale().max(0.25)) as u32;
            }
            // Terminal-pane events never reach the OSK bridge (§25), except
            // while a window gesture is active (drag/resize moves inside the
            // pane band must reach the gesture layer) and except the
            // bottom-right resize corner itself.
            if let Some(term) = &terminal {
                if matches!(gesture, WindowGesture::Idle)
                    && event_in_terminal_pane(event, keyboard_h_phys, gw, gh)
                {
                    route_terminal_event(&mut terminal_input, term, event, now, keyboard_h_phys);
                    continue;
                }
                if let SurfaceEvent::Resized { width, height } = event {
                    let pane_h = height.saturating_sub(keyboard_h_phys).max(200);
                    let mut t = term.borrow_mut();
                    let _ = t.resize(width, pane_h);
                    let scale = platform.scale().max(0.25);
                    ui.set_terminal_height(pane_h as f32 / scale);
                    ui.set_terminal_mode(true);
                    continue;
                }
            }
            // Window gestures: drag the top band to move, the bottom-right
            // corner to resize. Presses in a grip zone are swallowed (they
            // never reach the pointer bridge, so no key fires); the gesture
            // tracks deltas in surface-local pointer coordinates, so no
            // screen-coordinate knowledge is needed.
            match event {
                SurfaceEvent::PointerPressed {
                    x,
                    y,
                    button: PointerButton::Left,
                } => {
                    let in_resize = x >= (f64::from(gw) - RESIZE_CORNER).max(0.0)
                        && y >= (f64::from(gh) - RESIZE_CORNER).max(0.0);
                    // The title strip is 22 logical px tall; the gesture
                    // band must match it in PHYSICAL px.
                    let move_band = MOVE_BAND * f64::from(platform.scale());
                    let in_move = !in_resize && y <= move_band;
                    if in_move {
                        let (sx, sy) = platform.surface_position().unwrap_or((0, 0));
                        gesture = WindowGesture::Move {
                            win_x: sx,
                            win_y: sy,
                            grab_x: x,
                            grab_y: y,
                        };
                        log::debug!("window drag started at ({x:.0},{y:.0})");
                        continue;
                    }
                    if in_resize {
                        gesture = WindowGesture::Resize {
                            start_scale: current_view_scale,
                            grab_x: x,
                            grab_y: y,
                            // The pane (window space below the keyboard) is
                            // constant across keyboard resizes; capture it so
                            // the resized window keeps it (§25).
                            pane_phys: gh.saturating_sub(keyboard_h_phys),
                        };
                        log::debug!("window resize started at ({x:.0},{y:.0})");
                        continue;
                    }
                }
                SurfaceEvent::PointerMoved { x, y } => match gesture {
                    WindowGesture::Move {
                        win_x,
                        win_y,
                        grab_x,
                        grab_y,
                    } => {
                        // Pointer coordinates are SURFACE-LOCAL: once the
                        // surface moves, the local frame moves with it, so a
                        // delta against the press point would lag/jump. The
                        // gesture tracks the window's own position (updated
                        // below), so the surface follows pointer − grab
                        // offset exactly, independent of any surface state
                        // that might be stale.
                        let nx = win_x + (x - grab_x) as i32;
                        let ny = win_y + (y - grab_y) as i32;
                        log::debug!(
                            "drag: win=({win_x},{win_y}) ptr=({x:.0},{y:.0}) grab=({grab_x:.0},{grab_y:.0}) -> ({nx},{ny})"
                        );
                        if let Err(e) = platform.set_position(nx, ny) {
                            log::warn!("window move failed: {e}");
                        }
                        // Track the target we asked for: the next delta is
                        // relative to it, so the drag stays exact even if a
                        // position re-read would be stale.
                        gesture = WindowGesture::Move {
                            win_x: nx,
                            win_y: ny,
                            grab_x,
                            grab_y,
                        };
                        continue;
                    }
                    WindowGesture::Resize {
                        start_scale,
                        grab_x,
                        grab_y,
                        pane_phys,
                    } => {
                        // Uniform (diagonal) scaling: the window always
                        // equals the keyboard scale, so no dead whitespace
                        // ever grows — dragging right or down scales the WHOLE
                        // OSK. Scale = start × (pointer distance from the
                        // fixed top-left corner ÷ distance at grab). In
                        // terminal mode the window height is the scaled
                        // keyboard PLUS the constant pane captured at grab.
                        let dist0 = (grab_x * grab_x + grab_y * grab_y).sqrt().max(1.0);
                        let dist = (x * x + y * y).sqrt();
                        let mut s = (start_scale * (dist / dist0) as f32)
                            .clamp(MIN_VIEW_SCALE, MAX_VIEW_SCALE);
                        // The window must stay on the output.
                        let ps = platform.scale().max(0.25);
                        if let Some((out_w, out_h)) = platform.output_bounds() {
                            let s_max = ((out_w as f32 / view.width as f32).min(
                                (out_h as f32 - views::TITLE_H - pane_phys as f32 / ps)
                                    / view.height as f32,
                            ))
                            .max(MIN_VIEW_SCALE);
                            s = s.min(s_max);
                        }
                        let win_w = (view.width as f32 * s * ps).round() as u32;
                        let kb_h = ((views::TITLE_H + view.height as f32 * s) * ps).round() as u32;
                        let win_h = kb_h + pane_phys;
                        if let Err(e) = platform.set_size(win_w, win_h) {
                            log::warn!("window resize failed: {e}");
                        }
                        if let Some((cx, cy)) = platform.surface_position() {
                            let _ = platform.set_position(cx, cy);
                        }
                        continue;
                    }
                    WindowGesture::Idle => {}
                },
                SurfaceEvent::PointerReleased { .. } | SurfaceEvent::TouchReleased { .. }
                    if !matches!(gesture, WindowGesture::Idle) =>
                {
                    gesture = WindowGesture::Idle;
                    continue;
                }
                _ => {}
            }
            bridge.handle_event(&ui, event);
        }

        // 2. Terminal pump: PTY output, child reap, blink, pane redraw.
        if let Some(term) = &terminal {
            let mut t = term.borrow_mut();
            for ev in t.poll(now) {
                match ev {
                    ferrokey_terminal::TerminalEvent::Output => {}
                    ferrokey_terminal::TerminalEvent::Bell => {
                        log::debug!("terminal bell");
                    }
                    ferrokey_terminal::TerminalEvent::ChildExited(exit) => {
                        log::info!("terminal child exited: {}", exit.summary());
                    }
                }
            }
            t.tick_blink(now);
            if t.is_dirty() {
                if let Some(frame) = t.render() {
                    let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &frame.buffer,
                        frame.width,
                        frame.height,
                    );
                    ui.set_terminal_image(Image::from_rgba8(pixels));
                    platform.window().request_redraw();
                }
            }
        }

        // 3. Daemon connection upkeep + replies (system destination only).
        if system_sink.is_some() {
            let mut link = link.borrow_mut();
            link.poll_connect();
            let _ = link.poll_server();
            if Instant::now().duration_since(last_ping) > Duration::from_secs(2) {
                last_ping = Instant::now();
                let _ = link.ping();
            }
        }

        // 4. Key repeat cadence.
        if let Err(e) = driver.borrow_mut().tick_repeat(now_moment()) {
            log::warn!("repeat tick failed: {e}");
        }

        // 4a. Lit-key sync: held/latched/locked modifiers light their caps.
        sync_lit_keys(&ui, &driver, &mut last_lit);

        // 4a. Adaptive geometry: the optimizer runs off the touch hot path,
        // once per UI frame when enough new evidence has accumulated (WS4).
        bridge.tick_adaptive();

        // 4c. Shell context refresh (WS5 §5.3): inspect the child's process
        // tree (shell → vim/htop/tmux/nested shell) once per second. Rows
        // are presentation-only: only the shortcut row's key sequences and
        // labels change — no keys are released/pressed, no modes, resize or
        // child restart.
        if let Some(term) = &terminal {
            if now.duration_since(last_shell_probe) > Duration::from_secs(1) {
                last_shell_probe = now;
                let ctx = term
                    .borrow()
                    .child_pid()
                    .map(ferrokey_terminal::shell::ShellContext::inspect)
                    .unwrap_or(ferrokey_terminal::shell::ShellContext::UNKNOWN);
                if ctx.is_confident() {
                    let id = ctx.row_id();
                    if id != *shell_row_id.borrow() {
                        let row = ferrokey_terminal::shell::shell_row(id);
                        bridge.set_shell_row(Some(row));
                        set_terminal_shortcut_row(&ui, view, Some(row));
                        *shell_row_id.borrow_mut() = id;
                        log::info!("terminal shell context -> {id} (process inspection)");
                    }
                }
            }
        }

        // 4b. Repeat engine diagnostics (throttled).
        if Instant::now().duration_since(last_status_update) > Duration::from_millis(500) {
            let held: Vec<_> = driver.borrow().repeat().held_keys().collect();
            if !held.is_empty() {
                log::info!("repeat held: {held:?}");
            }
        }

        // 5. Render when dirty.
        if let Err(e) = platform.render_if_dirty() {
            log::error!("render failed: {e}");
            break;
        }

        // 6. Status strip refresh (throttled). The title/status strip is
        // ALWAYS visible (it doubles as the drag handle): it shows the
        // daemon link state, and it must clear problem text as soon as the
        // link is established (it previously stayed stale forever).
        if Instant::now().duration_since(last_status_update) > Duration::from_millis(500) {
            last_status_update = Instant::now();
            if let Some(link) = &system_sink {
                let link = link.borrow();
                let state = link.state();
                if state == LinkState::Connected {
                    // On a degraded backend keep the reason visible; on a
                    // focus-preserving backend a plain "connected" suffices.
                    let text: slint::SharedString =
                        if !detection.backend.preserves_focus() && !detection.detail.is_empty() {
                            detection.detail.clone().into()
                        } else {
                            "connected".into()
                        };
                    ui.set_status_line(text);
                    ui.set_status_ok(true);
                } else {
                    ui.set_status_line(state.label().into());
                    ui.set_status_ok(false);
                }
            } else {
                // Terminal-only mode: no daemon link exists by design.
                ui.set_status_line("terminal workspace".into());
                ui.set_status_ok(true);
            }
        }
    }

    // Cleanup: release everything we might still hold.
    let _ = driver.borrow_mut().emergency_release();
    if let Some(term) = &terminal {
        term.borrow_mut().shutdown();
    }
    Ok(())
}

/// Whether a surface event targets the terminal pane (below the keyboard).
fn event_in_terminal_pane(event: SurfaceEvent, keyboard_h: u32, win_w: u32, win_h: u32) -> bool {
    use SurfaceEvent::*;
    match event {
        PointerPressed { x, y, .. } | PointerMoved { x, y, .. } | PointerReleased { x, y, .. } => {
            // The bottom-right RESIZE_CORNER square is the window-resize
            // gesture zone: a press there must reach the gesture layer even
            // though it sits inside the pane band (terminal mode). Mirrors
            // the gesture's own hit-test exactly.
            let in_resize_corner = x >= (f64::from(win_w) - RESIZE_CORNER).max(0.0)
                && y >= (f64::from(win_h) - RESIZE_CORNER).max(0.0);
            !in_resize_corner && (y as u32) >= keyboard_h
        }
        TouchPressed { y, .. } | TouchMoved { y, .. } | TouchReleased { y, .. } => {
            (y as u32) >= keyboard_h
        }
        PointerLeft | TouchCancelled | Resized { .. } | CloseRequested => false,
    }
}

/// Route a pane-targeting surface event into the terminal gesture machine.
/// Coordinates are converted to pane-relative physical px (the terminal's
/// renderer frame space; the keyboard occupies the top `keyboard_h` px).
fn route_terminal_event(
    input: &mut term_input::TerminalInput,
    terminal: &Rc<RefCell<TerminalEngine>>,
    event: SurfaceEvent,
    now: Instant,
    keyboard_h: u32,
) {
    use SurfaceEvent::*;
    let mut t = terminal.borrow_mut();
    match event {
        PointerPressed { x, y, .. } | TouchPressed { x, y } => {
            input.press(&mut t, x as u32, y as u32 - keyboard_h, now);
        }
        PointerMoved { x, y } | TouchMoved { x, y } => {
            input.move_to(&mut t, x as u32, y as u32 - keyboard_h, now);
        }
        PointerReleased { x, y, .. } | TouchReleased { x, y } => {
            input.release(&mut t, x as u32, y as u32 - keyboard_h);
        }
        PointerLeft | TouchCancelled => input.cancel(),
        Resized { .. } | CloseRequested => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_key(
    driver: &Rc<RefCell<KeyboardDriver>>,
    link: &Rc<RefCell<DaemonLink>>,
    layout: &Arc<ferrokey_core::Layout>,
    composer: &Rc<RefCell<text::TextComposer>>,
    router: &Rc<RefCell<InputRouter>>,
    ui: &MainWindow,
    action: KeyAction,
    name: &str,
) -> anyhow::Result<()> {
    let Some(physical) = ferrokey_core::PhysicalKey::from_name(name) else {
        anyhow::bail!("unknown key name {name:?}");
    };

    let destination = router.borrow().active();

    // Text-mode: characters (including dead keys and the compose key) are
    // handled by the compose engine and typed via the layout; modifiers and
    // navigation keys fall through to the keyboard path below. Up actions
    // always fall through so held modifiers release correctly in text mode.
    // Text mode only applies to the SYSTEM destination (the terminal has its
    // own symbol path through the terminal key encoder).
    if ui.get_text_mode() && destination == Destination::System && action == KeyAction::Down {
        let effective = driver.borrow().state().effective_modifiers();
        let symbol = layout
            .symbol_for(physical, effective)
            .cloned()
            .unwrap_or(ferrokey_core::KeySymbol::None);
        let outcome = {
            let mut composer = composer.borrow_mut();
            let mut driver = driver.borrow_mut();
            let outcome = composer.key_down(&mut driver, &symbol, effective);
            let pending = composer.pending_label();
            ui.set_pending_label(pending.into());
            outcome
        };
        match outcome {
            Ok(text::TextModeOutcome::Handled) => return Ok(()),
            Ok(text::TextModeOutcome::FallThrough) => {} // keyboard path below
            Err(e) => {
                log::warn!("text mode: {e}");
                ui.set_pending_label("".into());
                return Ok(());
            }
        }
    }

    // The Escape key hides the OSK — in SYSTEM destination only. In the
    // terminal, Escape is a real key (0x1B) and must reach the shell.
    if physical == ferrokey_core::PhysicalKey::Escape
        && action == KeyAction::Down
        && destination == Destination::System
    {
        composer.borrow_mut().reset();
        ui.set_pending_label("".into());
        let _ = ui.hide();
        return Ok(());
    }

    let result =
        driver
            .borrow_mut()
            .handle_action(action, VirtualKey::Physical(physical), now_moment());
    match result {
        Ok(()) => {
            // If the daemon link dropped mid-key, release locally so the
            // state machine and the sink stay consistent (system destination
            // only; the terminal path has no daemon).
            if destination == Destination::System && !link.borrow().is_connected() {
                composer.borrow_mut().reset();
                let _ = driver.borrow_mut().emergency_release();
            }
            Ok(())
        }
        Err(e) => {
            log::warn!("key action failed: {e}");
            composer.borrow_mut().reset();
            let _ = driver.borrow_mut().emergency_release();
            Ok(())
        }
    }
}

fn looks_like_xkb_spec(spec: &str) -> bool {
    spec.contains('(') || spec.contains('@')
}

/// Populate the keyboard rows from the active *view* (visual arrangement) and
/// layout (symbols). The view decides which physical keys are visible, their
/// widths and any label overrides; the layout decides every symbol; the key
/// engine (ferrokey-core) is untouched.
/// Compute the uniform keyboard scale + horizontal offset from the current
/// window size: the keyboard (`view.width × view.height`) must fit below the
/// fixed 22px title strip, centered horizontally. Returns (scale, offset_x)
/// in logical px. `pane_phys` is the terminal pane's physical height (0
/// outside terminal mode): the pane is window space that must NOT count
/// toward the keyboard's height budget.
fn view_transform(
    win_w: u32,
    win_h: u32,
    ps: f32,
    view: &views::KeyboardView,
    pane_phys: u32,
) -> (f32, f32) {
    let wl = win_w as f32 / ps;
    let hl = win_h as f32 / ps;
    let kb_budget = hl - views::TITLE_H - pane_phys as f32 / ps;
    let s = (wl / view.width as f32)
        .min(kb_budget / view.height as f32)
        .clamp(MIN_VIEW_SCALE, MAX_VIEW_SCALE);
    let ox = ((wl - view.width as f32 * s) / 2.0).max(0.0);
    (s, ox)
}

/// Apply the window→view transform: set the Slint keyboard placement/scale
/// properties and the pointer bridge's mapping. Call after the window is
/// resized (and once at startup). Returns the applied scale.
fn apply_view_transform(
    ui: &MainWindow,
    platform: &FerrokeyPlatform,
    bridge: &mut pointer::PointerBridge,
    view: &'static views::KeyboardView,
    pane_phys: u32,
) -> f32 {
    let size = platform.size();
    let ps = platform.scale().max(0.25);
    let (s, ox) = view_transform(size.width, size.height, ps, view, pane_phys);
    ui.set_keyboard_x(ox);
    ui.set_keyboard_width(view.width as f32 * s);
    ui.set_keyboard_height(view.height as f32 * s);
    ui.set_keyboard_scale(s);
    ui.set_base_width(view.base_width * s);
    ui.set_key_height(views::VIEW_KEY_HEIGHT * s);
    ui.set_min_key_width(views::VIEW_MIN_KEY_WIDTH * s);
    bridge.set_view_transform(s, ox, views::TITLE_H);
    log::debug!(
        "view transform: scale {s:.3} offset {ox:.1} (window {}x{})",
        size.width,
        size.height
    );
    s
}

/// Push the current modifier-cap lighting to the UI: each modifier is lit
/// when it is physically held OR latched OR locked, plus Caps/Num lock.
/// Only pushes when the state changes (per-frame sync is allocation-free).
fn sync_lit_keys(
    ui: &MainWindow,
    driver: &Rc<RefCell<KeyboardDriver>>,
    last: &mut (bool, bool, bool, bool, bool, bool, bool, bool),
) {
    let st = driver.borrow();
    let state = st.state();
    let held = |k: ferrokey_core::PhysicalKey| state.depressed().contains(k);
    let latched = state.latched();
    let locked = state.locked();
    let sig = (
        // Shift lights from a physical hold or a shift LATCH only. The
        // `locked` SHIFT bit is the Caps Lock mirror, so excluding it keeps
        // Caps Lock from lighting the shift cap (the caps cap shows it).
        held(ferrokey_core::PhysicalKey::LeftShift)
            || held(ferrokey_core::PhysicalKey::RightShift)
            || latched.contains(ModifierSet::SHIFT),
        held(ferrokey_core::PhysicalKey::LeftCtrl)
            || held(ferrokey_core::PhysicalKey::RightCtrl)
            || latched.contains(ModifierSet::CTRL)
            || locked.contains(ModifierSet::CTRL),
        held(ferrokey_core::PhysicalKey::LeftAlt)
            || latched.contains(ModifierSet::ALT)
            || locked.contains(ModifierSet::ALT),
        held(ferrokey_core::PhysicalKey::RightAlt)
            || latched.contains(ModifierSet::ALTGR)
            || locked.contains(ModifierSet::ALTGR),
        held(ferrokey_core::PhysicalKey::LeftMeta)
            || held(ferrokey_core::PhysicalKey::RightMeta)
            || latched.contains(ModifierSet::META)
            || locked.contains(ModifierSet::META),
        held(ferrokey_core::PhysicalKey::Menu)
            || latched.contains(ModifierSet::FN)
            || locked.contains(ModifierSet::FN),
        state.caps_lock(),
        state.num_lock(),
    );
    if sig != *last {
        *last = sig;
        ui.set_lit_state(LitState {
            shift: sig.0,
            ctrl: sig.1,
            alt: sig.2,
            altgr: sig.3,
            meta: sig.4,
            fn_key: sig.5,
            caps: sig.6,
            num: sig.7,
        });
    }
}

fn set_keyboard_view(ui: &MainWindow, layout: &ferrokey_core::Layout, view: &views::KeyboardView) {
    use slint::VecModel;

    ui.set_base_width(view.base_width);
    for (index, row) in view.rows.iter().enumerate() {
        let model: Vec<KeyData> = row
            .keys
            .iter()
            .map(|vk| {
                // The logo button renders the embedded brand image, never a
                // label or a key event (decorative key, views.rs).
                if vk.logo {
                    return KeyData {
                        name: vk.name.into(),
                        label: "".into(),
                        width: vk.width,
                        is_logo: true,
                    };
                }
                // Chord keys (terminal shortcut row) carry their display name
                // as the label; the bridge plays the chord, never a key event
                // for the placeholder name.
                let label = if vk.chord.is_some() {
                    vk.name.to_string()
                } else {
                    vk.label.map(str::to_string).unwrap_or_else(|| {
                        let key = ferrokey_core::PhysicalKey::from_name(vk.name)
                            .expect("view data validated at test time");
                        match layout.symbol_for(key, ModifierSet::empty()) {
                            Some(KeySymbol::Dead(d)) => format!("◌{}", d.name()),
                            Some(KeySymbol::Compose) => "⏽".into(),
                            Some(sym) => sym.label(),
                            None => vk.name.to_string(),
                        }
                    })
                };
                KeyData {
                    name: vk.name.into(),
                    label: label.into(),
                    width: vk.width,
                    is_logo: false,
                }
            })
            .collect();
        let model = ModelRc::from(Rc::new(VecModel::from(model)));
        match index {
            0 => ui.set_row1(model),
            1 => ui.set_row2(model),
            2 => ui.set_row3(model),
            3 => ui.set_row4(model),
            4 => ui.set_row5(model),
            5 => ui.set_row6(model),
            6 => ui.set_row7(model),
            _ => log::warn!("view {} has more than 7 rows; extra rows ignored", view.id),
        }
    }
}

/// Re-render the terminal view's shortcut row from the active shell-aware
/// row (WS5). Presentation-only (§5.10): the keys/sequences change, nothing
/// else does — no held keys are released, no keys pressed, no modes, no
/// resize, no child restart. The bridge is given the same row so its chord
/// lookup matches the rendered buttons. The brand mark stays at the row's
/// end through every swap (decorative; the bridge ignores it).
fn set_terminal_shortcut_row(
    ui: &MainWindow,
    view: &views::KeyboardView,
    row: Option<&'static [ferrokey_terminal::shell::ShellRowKey]>,
) {
    use slint::VecModel;
    if view.id != "terminal" {
        return;
    }
    let mut keys: Vec<KeyData> = match row {
        Some(row) => row
            .iter()
            .map(|k| KeyData {
                name: k.label.into(),
                label: k.label.into(),
                // The rendered width is the SAME value the pointer bridge
                // uses to hit-test the shell row (shell.rs — the per-key
                // `width`, defaulting to SHELL_KEY_WIDTH) — a mismatch would
                // make clicks land on the wrong chord.
                width: k.width,
                is_logo: false,
            })
            .collect(),
        // Fall back to the static shortcut row (the first terminal row).
        None => view
            .rows
            .first()
            .map(|row| {
                row.keys
                    .iter()
                    .map(|vk| {
                        if vk.logo {
                            KeyData {
                                name: vk.name.into(),
                                label: "".into(),
                                width: vk.width,
                                is_logo: true,
                            }
                        } else {
                            KeyData {
                                name: vk.name.into(),
                                label: vk.name.into(),
                                width: vk.width,
                                is_logo: false,
                            }
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    // The brand mark persists across shell-row swaps (presentation-only).
    // Only shell rows need it appended: the static fallback row above already
    // ends with its own logo key (views.rs), so appending again would render
    // two brand marks.
    if row.is_some() {
        keys.push(KeyData {
            name: "logo".into(),
            label: "".into(),
            width: 0.9,
            is_logo: true,
        });
    }
    ui.set_row1(ModelRc::from(Rc::new(VecModel::from(keys))));
}
