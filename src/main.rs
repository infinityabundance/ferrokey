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
mod views;

use anyhow::Context;
use config::UiConfig;
use daemon::DaemonLink;
use ferrokey_core::{
    KeyAction, KeySymbol, KeyboardDriver, ModifierSet, RepeatSettings, StateSettings, VirtualKey,
};
use ferrokey_layouts::builtin;
use ferrokey_surface::slint_adapter::FerrokeyPlatform;
use ferrokey_surface::{detect, fallback::NullSurface, Surface, SurfaceBackend, SurfaceEvent};
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

/// The single monotonic epoch for the keyboard engine's time input: captured
/// once at process start, so every `Moment` across modules shares one clock
/// (mixed epochs would corrupt tap/latch timing).
static STARTED: OnceLock<Instant> = OnceLock::new();

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
    let (win_width, win_height) = if terminal_cfg.enabled {
        let pane_h = terminal_cfg.pane_height.max(200);
        log::info!(
            "terminal workspace mode: pane {}px, font {}px, scrollback {}",
            pane_h,
            terminal_cfg.font_size_px,
            terminal_cfg.scrollback_lines
        );
        (keyboard_w, keyboard_h + pane_h)
    } else {
        (keyboard_w, keyboard_h)
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
            lock_enabled: config.sticky.lock_enabled,
            tap_timeout: Duration::from_millis(config.sticky.tap_timeout_ms),
            double_tap_timeout: Duration::from_millis(config.sticky.double_tap_timeout_ms),
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
        "driver: repeat enabled={} delay={}ms cadence={}ms latch={} lock={}",
        config.repeat.enabled,
        config.repeat.delay_ms,
        config.repeat.cadence_ms,
        config.sticky.latch_enabled,
        config.sticky.lock_enabled
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

    let mut bridge = pointer::PointerBridge::new(view, platform.scale());
    let mut terminal_input = term_input::TerminalInput::default();
    let keyboard_h_phys = keyboard_h;

    let mut last_ping = Instant::now();
    let mut last_status_update = Instant::now();
    loop {
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
        for event in events {
            // Terminal-pane events never reach the OSK bridge (§25).
            if let Some(term) = &terminal {
                if event_in_terminal_pane(event, keyboard_h_phys) {
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

        // 6. Status line refresh (throttled).
        if Instant::now().duration_since(last_status_update) > Duration::from_millis(500) {
            last_status_update = Instant::now();
            if let Some(link) = &system_sink {
                let link = link.borrow();
                let label = link.state().label();
                if !label.is_empty() {
                    ui.set_status_line(label.into());
                } else if !detection.detail.is_empty() && !detection.backend.preserves_focus() {
                    ui.set_status_line(detection.detail.clone().into());
                }
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
fn event_in_terminal_pane(event: SurfaceEvent, keyboard_h: u32) -> bool {
    use SurfaceEvent::*;
    match event {
        PointerPressed { y, .. }
        | PointerMoved { y, .. }
        | PointerReleased { y, .. }
        | TouchPressed { y, .. }
        | TouchMoved { y, .. }
        | TouchReleased { y, .. } => y as u32 >= keyboard_h,
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
fn set_keyboard_view(ui: &MainWindow, layout: &ferrokey_core::Layout, view: &views::KeyboardView) {
    use slint::VecModel;

    ui.set_base_width(view.base_width);
    for (index, row) in view.rows.iter().enumerate() {
        let model: Vec<KeyData> = row
            .keys
            .iter()
            .map(|vk| {
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
