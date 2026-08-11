//! # ferrokey — the on-screen keyboard
//!
//! Unprivileged UI. Owns no input devices: every key event goes to
//! `ferrokeyd` over the authenticated Unix socket, which owns `/dev/uinput`.
//!
//! Slint renders and hit-tests; `ferrokey-surface` owns the window semantics
//! (layer-shell `keyboard_interactivity = none` on Wayland, ICCCM
//! `WM_HINTS.input = False` on X11); `ferrokey-core` owns the key semantics.

mod config;
mod daemon;
mod text;

use config::UiConfig;
use daemon::DaemonLink;
use ferrokey_core::{
    KeyAction, KeySymbol, KeyboardDriver, ModifierSet, RepeatSettings, StateSettings, VirtualKey,
};
use ferrokey_layouts::builtin;
use ferrokey_surface::slint_adapter::FerrokeyPlatform;
use ferrokey_surface::{detect, fallback::NullSurface, Surface, SurfaceBackend};
use slint::ModelRc;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    let config = load_ui_config()?;
    run(config)
}

fn load_ui_config() -> anyhow::Result<UiConfig> {
    let mut explicit: Option<std::path::PathBuf> = None;
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
                let layout = args.get(i).context("--layout requires an id")?;
                let mut cfg = UiConfig::default();
                cfg.layout = layout.clone();
                return Ok(cfg);
            }
            "--help" | "-h" => {
                println!("ferrokey — on-screen keyboard with focus preservation\n\nUSAGE:\n  ferrokey [--config <path>] [--layout <id>]");
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
        i += 1;
    }
    if let Some(path) = explicit {
        return Ok(UiConfig::load(&path)?);
    }
    for candidate in UiConfig::default_paths() {
        if candidate.exists() {
            return Ok(UiConfig::load(&candidate)?);
        }
    }
    Ok(UiConfig::default())
}

fn run(config: UiConfig) -> anyhow::Result<()> {
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
            let mut options = ferrokey_surface::x11::X11Options::default();
            options.display = detection.x11_display.clone().or(config.x11_display.clone());
            options.override_redirect = false;
            let surface = ferrokey_surface::x11::X11Surface::create(options)
                .context("x11 surface create failed")?;
            Box::new(surface)
        }
        SurfaceBackend::WaylandDegraded | SurfaceBackend::None => Box::new(NullSurface::new()),
    };

    // ── Platform + Slint window ────────────────────────────────────────────
    let platform = FerrokeyPlatform::new(surface, config.width, config.height);
    slint::platform::set_platform(Box::new(ferrokey_surface::slint_adapter::PlatformHandle(
        platform.clone(),
    )))
    .map_err(|e| anyhow::anyhow!("set_platform: {e}"))?;

    let ui = MainWindow::new().map_err(|e| anyhow::anyhow!("ui init: {e}"))?;
    platform.set_size(config.width, config.height)?;
    ui.set_degraded(config.force_degraded_banner || !detection.backend.preserves_focus());
    ui.set_status_line(detection.detail.clone().into());

    // ── Layout ─────────────────────────────────────────────────────────────
    let layout = Arc::new(builtin(&config.layout).with_context(|| {
        format!(
            "unknown layout {:?} (builtins: {})",
            config.layout,
            ferrokey_layouts::BUILTIN_IDS.join(", ")
        )
    })?);
    set_keyboard_rows(&ui, &layout);

    // ── Core driver over the daemon link ───────────────────────────────────
    let link = Rc::new(RefCell::new(DaemonLink::new(config.socket_path.clone())));
    let sink: Box<dyn ferrokey_core::KeySink> = Box::new(daemon::DaemonLinkSink(link.clone()));
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

    // ── Wire the UI callbacks ──────────────────────────────────────────────
    let ui_handle = ui.as_weak();
    let driver_cb = driver.clone();
    let link_cb = link.clone();
    let layout_cb = layout.clone();
    ui.on_key_pressed(move |name| {
        if let Some(ui) = ui_handle.upgrade() {
            let _ = handle_key(
                &driver_cb,
                &link_cb,
                &layout_cb,
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
    ui.on_key_released(move |name| {
        if let Some(ui) = ui_handle.upgrade() {
            let _ = handle_key(&driver_cb, &link_cb, &layout_cb, &ui, KeyAction::Up, &name);
        }
    });

    // ── Event loop ─────────────────────────────────────────────────────────
    platform.set_visible(true)?;
    log::info!("ferrokey running ({}x{})", config.width, config.height);

    let mut last_ping = Instant::now();
    let mut last_status_update = Instant::now();
    loop {
        // 1. Surface events (pointer, resize, close) into Slint.
        match platform.process_events(Some(Duration::from_millis(10))) {
            Ok(()) => {}
            Err(e) => {
                log::error!("surface event error: {e}");
                break;
            }
        }

        // 2. Daemon connection upkeep + replies.
        {
            let mut link = link.borrow_mut();
            link.poll_connect();
            let _ = link.poll_server();
            if Instant::now().duration_since(last_ping) > Duration::from_secs(2) {
                last_ping = Instant::now();
                let _ = link.ping();
            }
        }

        // 3. Key repeat cadence.
        if let Err(e) = driver.borrow_mut().tick_repeat(Instant::now()) {
            log::warn!("repeat tick failed: {e}");
        }

        // 4. Render when dirty.
        if let Err(e) = platform.render_if_dirty() {
            log::error!("render failed: {e}");
            break;
        }

        // 5. Status line refresh (throttled).
        if Instant::now().duration_since(last_status_update) > Duration::from_millis(500) {
            last_status_update = Instant::now();
            let link = link.borrow();
            let label = link.state().label();
            if !label.is_empty() {
                ui.set_status_line(label.into());
            } else if !detection.detail.is_empty() && !detection.backend.preserves_focus() {
                ui.set_status_line(detection.detail.clone().into());
            }
        }
    }

    // Cleanup: release everything we might still hold.
    let _ = driver.borrow_mut().emergency_release();
    Ok(())
}

fn handle_key(
    driver: &Rc<RefCell<KeyboardDriver>>,
    link: &Rc<RefCell<DaemonLink>>,
    layout: &Arc<ferrokey_core::Layout>,
    ui: &MainWindow,
    action: KeyAction,
    name: &str,
) -> anyhow::Result<()> {
    let Some(physical) = ferrokey_core::PhysicalKey::from_name(name) else {
        anyhow::bail!("unknown key name {name:?}");
    };

    // Text-mode: characters are typed via the layout, not raw key events.
    if ui.get_text_mode() {
        if action == KeyAction::Down {
            if let Some(KeySymbol::Char(c)) = layout.symbol_for(physical, ModifierSet::empty()) {
                let layout = layout.clone();
                let result = text::type_text(&mut driver.borrow_mut(), &c.to_string(), |c| {
                    layout.find_char(c, ModifierSet::empty())
                });
                if let Err(e) = result {
                    log::warn!("text mode: {e}");
                }
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }

    // The Escape key hides the OSK.
    if physical == ferrokey_core::PhysicalKey::Escape && action == KeyAction::Down {
        let _ = ui.hide();
        return Ok(());
    }

    let result =
        driver
            .borrow_mut()
            .handle_action(action, VirtualKey::Physical(physical), Instant::now());
    match result {
        Ok(()) => {
            // If the daemon link dropped mid-key, release locally so the
            // state machine and the sink stay consistent.
            if !link.borrow().is_connected() {
                let _ = driver.borrow_mut().emergency_release();
            }
            Ok(())
        }
        Err(e) => {
            log::warn!("key action failed: {e}");
            let _ = driver.borrow_mut().emergency_release();
            Ok(())
        }
    }
}

/// Populate the six keyboard rows from the active layout.
fn set_keyboard_rows(ui: &MainWindow, layout: &ferrokey_core::Layout) {
    use slint::VecModel;

    let rows: [&[&str]; 6] = [
        &[
            "escape", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12",
        ],
        &[
            "grave",
            "1",
            "2",
            "3",
            "4",
            "5",
            "6",
            "7",
            "8",
            "9",
            "0",
            "minus",
            "equal",
            "backspace",
        ],
        &[
            "tab",
            "q",
            "w",
            "e",
            "r",
            "t",
            "y",
            "u",
            "i",
            "o",
            "p",
            "left-bracket",
            "right-bracket",
            "backslash",
        ],
        &[
            "caps-lock",
            "a",
            "s",
            "d",
            "f",
            "g",
            "h",
            "j",
            "k",
            "l",
            "semicolon",
            "apostrophe",
            "enter",
        ],
        &[
            "left-shift",
            "z",
            "x",
            "c",
            "v",
            "b",
            "n",
            "m",
            "comma",
            "dot",
            "slash",
            "right-shift",
        ],
        &[
            "left-ctrl",
            "left-meta",
            "left-alt",
            "space",
            "right-alt",
            "menu",
            "right-ctrl",
        ],
    ];

    let wide: &[&str] = &[
        "escape",
        "backspace",
        "tab",
        "caps-lock",
        "enter",
        "left-shift",
        "right-shift",
        "space",
    ];

    for (index, row) in rows.iter().enumerate() {
        let model: Vec<KeyData> = row
            .iter()
            .map(|name| {
                let key = ferrokey_core::PhysicalKey::from_name(name).unwrap();
                let symbol = layout
                    .symbol_for(key, ModifierSet::empty())
                    .map(KeySymbol::label)
                    .unwrap_or_else(|| (*name).to_string());
                let width = if *name == "space" {
                    6.0
                } else if wide.contains(name) {
                    1.6
                } else {
                    1.0
                };
                KeyData {
                    name: (*name).into(),
                    label: symbol.into(),
                    width,
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
            _ => ui.set_row6(model),
        }
    }
}

// Re-export for the slint macro.
#[allow(unused_imports)]
use anyhow::Context as _;
