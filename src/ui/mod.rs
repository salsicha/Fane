//! The UI thread: a winit event loop driving Fane's three windows (palette, overlay card,
//! settings), the tray icon and (macOS/Windows) the global hotkey.

mod gl;
pub mod icon;
mod overlay;
mod palette;
mod settings;
mod style;
mod tray;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{WindowAttributes, WindowId, WindowLevel};

use crate::config::CONFIG_PATH;
use crate::core::{Clip, Core, Feedback, UiEvent, UiSink, WindowKind};
use gl::GlWindow;
use overlay::{Overlay, OverlayAction};
use palette::{Palette, PaletteAction};
use settings::{Settings, SettingsAction};

const MARGIN: f64 = 14.0;

struct ProxySink(EventLoopProxy<UiEvent>);

impl UiSink for ProxySink {
    fn send(&self, ev: UiEvent) {
        let _ = self.0.send_event(ev);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Origin {
    Palette,
    Overlay,
}

pub fn run(core: Arc<Core>, rt: tokio::runtime::Handle) -> anyhow::Result<()> {
    #[allow(unused_mut)]
    let mut builder = EventLoop::<UiEvent>::with_user_event();
    let mut x11 = false;
    #[cfg(target_os = "linux")]
    {
        // Prefer XWayland: it lets the overlay sit in a screen corner and stay on top.
        use winit::platform::x11::EventLoopBuilderExtX11;
        if std::env::var_os("DISPLAY").is_some() {
            builder.with_x11();
            x11 = true;
        }
    }
    let event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();
    {
        let core = core.clone();
        let sink: Arc<dyn UiSink> = Arc::new(ProxySink(proxy.clone()));
        rt.spawn(async move { core.start(sink).await });
    }
    let mut app = App::new(core, rt, proxy, x11);
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct App {
    core: Arc<Core>,
    rt: tokio::runtime::Handle,
    proxy: EventLoopProxy<UiEvent>,
    x11: bool,
    gl_config: Option<glutin::config::Config>,
    windows: HashMap<WindowKind, GlWindow>,
    ids: HashMap<WindowId, WindowKind>,
    visible: HashSet<WindowKind>,
    palette: Palette,
    overlay: Overlay,
    settings: Settings,
    jobs: HashMap<u64, (Origin, String)>,
    repaint_at: HashMap<WindowKind, Instant>,
    tray: Option<tray::Tray>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    hotkeys: Option<global_hotkey::GlobalHotKeyManager>,
    started: bool,
}

impl App {
    fn new(core: Arc<Core>, rt: tokio::runtime::Handle, proxy: EventLoopProxy<UiEvent>, x11: bool) -> Self {
        let settings = Settings::new(core.cfg());
        Self {
            core,
            rt,
            proxy,
            x11,
            gl_config: None,
            windows: HashMap::new(),
            ids: HashMap::new(),
            visible: HashSet::new(),
            palette: Palette::default(),
            overlay: Overlay::default(),
            settings,
            jobs: HashMap::new(),
            repaint_at: HashMap::new(),
            tray: None,
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            hotkeys: None,
            started: false,
        }
    }

    // --- startup ------------------------------------------------------------------------
    fn start(&mut self, event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        match gl::gl_config(event_loop) {
            Ok(c) => self.gl_config = Some(c),
            Err(e) => tracing::error!("{e}"),
        }
        self.tray = Some(tray::Tray::new(self.proxy.clone(), &self.rt, self.core.paused.load(Ordering::SeqCst)));
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        self.register_hotkey();
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn register_hotkey(&mut self) {
        use global_hotkey::hotkey::HotKey;
        use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
        let binding = self.core.cfg().hotkey.binding;
        let Ok(manager) = GlobalHotKeyManager::new() else {
            tracing::warn!("global hotkeys unavailable");
            return;
        };
        match binding.parse::<HotKey>() {
            Ok(hk) => {
                if let Err(e) = manager.register(hk) {
                    tracing::warn!("cannot register hotkey {binding}: {e}");
                    self.overlay.toast(&format!("Couldn't register the hotkey {binding}: {e}"), 8);
                }
            }
            Err(e) => tracing::warn!("bad [hotkey] binding {binding:?}: {e}"),
        }
        let proxy = self.proxy.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if e.state == HotKeyState::Pressed {
                let _ = proxy.send_event(UiEvent::Hotkey);
            }
        }));
        self.hotkeys = Some(manager);
    }

    // --- windows ------------------------------------------------------------------------
    fn attrs(kind: WindowKind) -> WindowAttributes {
        let icon = icon::rgba(64);
        let icon = winit::window::Icon::from_rgba(icon.into_raw(), 64, 64).ok();
        let base = WindowAttributes::default().with_visible(false).with_window_icon(icon);
        #[allow(unused_mut)]
        let mut attrs = match kind {
            WindowKind::Palette => base
                .with_title("Fane")
                .with_decorations(false)
                .with_transparent(true)
                .with_resizable(false)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_inner_size(LogicalSize::new(palette::WIDTH as f64, 90.0)),
            WindowKind::Overlay => base
                .with_title("Fane Suggestion")
                .with_decorations(false)
                .with_transparent(true)
                .with_resizable(false)
                .with_active(false)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_inner_size(LogicalSize::new(overlay::WIDTH as f64, 120.0)),
            WindowKind::Settings => base
                .with_title("Fane Settings")
                .with_resizable(true)
                .with_inner_size(LogicalSize::new(660.0, 680.0))
                .with_min_inner_size(LogicalSize::new(480.0, 400.0)),
        };
        #[cfg(target_os = "linux")]
        {
            use winit::platform::x11::{WindowAttributesExtX11, WindowType};
            attrs = attrs.with_name("fane", "Fane");
            attrs = match kind {
                WindowKind::Overlay => attrs.with_x11_window_type(vec![WindowType::Notification]),
                WindowKind::Palette => attrs.with_x11_window_type(vec![WindowType::Dialog]),
                WindowKind::Settings => attrs,
            };
        }
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            if kind != WindowKind::Settings {
                attrs = attrs.with_skip_taskbar(true);
            }
        }
        attrs
    }

    fn ensure_window(&mut self, event_loop: &ActiveEventLoop, kind: WindowKind) -> bool {
        if self.windows.contains_key(&kind) {
            return true;
        }
        let Some(config) = &self.gl_config else { return false };
        match GlWindow::new(event_loop, config, Self::attrs(kind)) {
            Ok(win) => {
                let ctx = win.egui.egui_ctx.clone();
                if kind == WindowKind::Settings {
                    egui_extras_theme(&ctx);
                } else {
                    style::apply_card_style(&ctx);
                }
                let proxy = self.proxy.clone();
                ctx.set_request_repaint_callback(move |info| {
                    let _ = proxy.send_event(UiEvent::Redraw(kind, info.delay));
                });
                self.ids.insert(win.window.id(), kind);
                self.windows.insert(kind, win);
                true
            }
            Err(e) => {
                tracing::error!("cannot create the {kind:?} window: {e:#}");
                false
            }
        }
    }

    fn redraw(&self, kind: WindowKind) {
        if let Some(w) = self.windows.get(&kind) {
            w.window.request_redraw();
        }
    }

    fn set_visible(&mut self, kind: WindowKind, visible: bool) {
        if let Some(w) = self.windows.get(&kind) {
            w.window.set_visible(visible);
            if visible {
                self.visible.insert(kind);
                w.window.request_redraw();
            } else {
                self.visible.remove(&kind);
            }
        }
        self.update_busy();
    }

    fn update_busy(&self) {
        let busy = self.visible.contains(&WindowKind::Palette) || self.overlay.has_suggestion() || self.overlay.confirming();
        self.core.busy.store(busy, Ordering::SeqCst);
    }

    /// (origin, size, scale) of the monitor the windows should appear on.
    fn monitor(&self, event_loop: &ActiveEventLoop) -> Option<(PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>, f64)> {
        let m = event_loop.primary_monitor().or_else(|| event_loop.available_monitors().next())?;
        Some((m.position(), m.size(), m.scale_factor()))
    }

    fn place(&self, event_loop: &ActiveEventLoop, kind: WindowKind) {
        let Some(win) = self.windows.get(&kind) else { return };
        let Some((origin, size, scale)) = self.monitor(event_loop) else { return };
        let outer = win.window.outer_size();
        let (mw, mh) = (size.width as f64, size.height as f64);
        let (ww, wh) = (outer.width as f64, outer.height as f64);
        let margin = MARGIN * scale;
        // Room for the top bar (GNOME, macOS menu bar) or the taskbar (Windows).
        let top = if cfg!(target_os = "windows") { 0.0 } else { 36.0 * scale };
        let bottom = if cfg!(target_os = "windows") { 48.0 * scale } else { 0.0 };
        let (x, y) = match kind {
            WindowKind::Palette => ((mw - ww) / 2.0, mh * 0.2),
            WindowKind::Overlay => {
                let corner = self.core.cfg().suggestions.corner;
                let x = if corner.contains("left") { margin } else { mw - ww - margin };
                let y = if corner.contains("bottom") { mh - wh - margin - bottom } else { top + margin };
                (x, y)
            }
            WindowKind::Settings => ((mw - ww) / 2.0, (mh - wh) / 3.0),
        };
        let (x, y) = (origin.x + x.round() as i32, origin.y + y.round() as i32);
        win.window.set_outer_position(PhysicalPosition::new(x, y));
        if !self.x11 && kind != WindowKind::Settings && self.core.desktop.helper_available() {
            // Native Wayland: only the GNOME helper extension can place us.
            let core = self.core.clone();
            let title = win.title.clone();
            self.rt.spawn(async move { core.desktop.place_window(&title, x, y, true).await });
        }
    }

    // --- palette ------------------------------------------------------------------------
    fn show_palette(&mut self, event_loop: &ActiveEventLoop, snap: crate::snapshot::Snapshot, prefill: &str, submit: bool, answer: Option<String>) {
        if !self.ensure_window(event_loop, WindowKind::Palette) {
            self.core.busy.store(false, Ordering::SeqCst);
            return;
        }
        self.palette.present(snap, prefill);
        self.place(event_loop, WindowKind::Palette);
        self.set_visible(WindowKind::Palette, true);
        if let Some(w) = self.windows.get(&WindowKind::Palette) {
            w.window.focus_window();
        }
        if let Some(a) = answer {
            self.palette.show_answer(&a, false);
        } else if submit {
            if let Some(text) = self.palette.submit() {
                self.palette_submit(text);
            }
        }
    }

    fn hide_palette(&mut self) {
        self.set_visible(WindowKind::Palette, false);
    }

    fn toggle_palette(&mut self) {
        if self.visible.contains(&WindowKind::Palette) {
            self.hide_palette();
        } else {
            self.core.open_palette(String::new(), false, None);
        }
    }

    fn palette_submit(&mut self, text: String) {
        let job = self.core.ask(text.clone(), self.palette.snapshot.clone());
        self.palette.job = Some(job);
        self.jobs.insert(job, (Origin::Palette, text));
    }

    fn insert(&self, text: String) {
        let core = self.core.clone();
        self.rt.spawn(async move {
            core.insert(Clip::Text(text)).await;
        });
    }

    fn copy(&self, text: String) {
        let core = self.core.clone();
        self.rt.spawn(async move { core.copy_text(text).await });
    }

    // --- overlay ------------------------------------------------------------------------
    fn show_overlay(&mut self, event_loop: &ActiveEventLoop) {
        if !self.ensure_window(event_loop, WindowKind::Overlay) {
            return;
        }
        if !self.visible.contains(&WindowKind::Overlay) {
            self.place(event_loop, WindowKind::Overlay);
            self.set_visible(WindowKind::Overlay, true);
        }
        self.update_busy();
        self.redraw(WindowKind::Overlay);
    }

    fn toast(&mut self, event_loop: &ActiveEventLoop, text: &str, seconds: u64) {
        self.overlay.toast(text, seconds);
        if self.overlay.visible() {
            self.show_overlay(event_loop);
        }
    }

    fn on_overlay_action(&mut self, a: OverlayAction) {
        match a {
            OverlayAction::Run(s) => {
                self.core.suggestion_feedback(Feedback::Accepted);
                let snap = self.core.last_snapshot.lock().unwrap().clone();
                let job = self.core.ask(s.prompt.clone(), snap);
                self.jobs.insert(job, (Origin::Overlay, s.title.clone()));
                self.overlay.show_working(job, &s.title);
            }
            OverlayAction::Edit(s) => {
                self.core.suggestion_feedback(Feedback::Accepted);
                self.core.open_palette(s.prompt, false, None);
            }
            OverlayAction::Dismissed(Some(_)) => self.core.suggestion_feedback(Feedback::Dismissed),
            OverlayAction::Expired(Some(_)) => self.core.suggestion_feedback(Feedback::Ignored),
            OverlayAction::Dismissed(None) | OverlayAction::Expired(None) => {}
            OverlayAction::Insert(t) => self.insert(t),
            OverlayAction::Open(t) => self.core.open_palette(String::new(), false, Some(t)),
            OverlayAction::Copy(t) => self.copy(t),
        }
        self.update_busy();
    }

    // --- settings -----------------------------------------------------------------------
    fn open_settings(&mut self, event_loop: &ActiveEventLoop) {
        if !self.ensure_window(event_loop, WindowKind::Settings) {
            return;
        }
        let first = !self.visible.contains(&WindowKind::Settings);
        let actions = self.settings.open(self.core.cfg());
        if first {
            self.place(event_loop, WindowKind::Settings);
        }
        self.set_visible(WindowKind::Settings, true);
        if let Some(w) = self.windows.get(&WindowKind::Settings) {
            w.window.focus_window();
        }
        for a in actions {
            self.on_settings_action(a);
        }
    }

    fn on_settings_action(&mut self, a: SettingsAction) {
        let core = self.core.clone();
        let proxy = self.proxy.clone();
        match a {
            SettingsAction::Scan => {
                self.rt.spawn(async move {
                    let servers = core.discover_ollama().await;
                    let _ = proxy.send_event(UiEvent::Servers(Ok(servers)));
                });
            }
            SettingsAction::UseServer(url) => {
                self.rt.spawn(async move {
                    let r = core.use_ollama(&url).await;
                    let _ = proxy.send_event(UiEvent::OllamaUsed(r));
                });
            }
            SettingsAction::RefreshAccounts => {
                self.rt.spawn(async move {
                    let r = core.accounts.google_accounts().await.map_err(|e| e.to_string());
                    let _ = proxy.send_event(UiEvent::MailAccounts(r));
                });
            }
            SettingsAction::SelectAccount(addr) => {
                self.rt.spawn(async move {
                    let r = core.select_mail_account(&addr).await;
                    let _ = proxy.send_event(UiEvent::MailSaved(r));
                });
            }
            SettingsAction::SaveOwner(email) => {
                self.rt.spawn(async move {
                    let r = core.set_owner_email(&email).await;
                    let _ = proxy.send_event(UiEvent::MailSaved(r));
                });
            }
            SettingsAction::ManageAccounts => {
                let _ = std::process::Command::new("gnome-control-center").arg("online-accounts").spawn();
            }
            SettingsAction::EditConfig => {
                if let Err(e) = open::that_detached(&*CONFIG_PATH) {
                    tracing::warn!("cannot open {}: {e}", CONFIG_PATH.display());
                }
            }
            SettingsAction::Close => self.set_visible(WindowKind::Settings, false),
        }
    }

    // --- quitting -----------------------------------------------------------------------
    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        let core = self.core.clone();
        let _ = self.rt.block_on(async move { tokio::time::timeout(Duration::from_secs(5), core.shutdown()).await });
        event_loop.exit();
    }

    // --- painting -----------------------------------------------------------------------
    fn paint(&mut self, event_loop: &ActiveEventLoop, kind: WindowKind) {
        tracing::trace!("paint {kind:?} (visible: {})", self.visible.contains(&kind));
        if !self.visible.contains(&kind) {
            return;
        }
        let App { windows, palette, overlay, settings, .. } = self;
        let Some(win) = windows.get_mut(&kind) else { return };
        let mut used = egui::Vec2::ZERO;
        let mut pa = vec![];
        let mut oa = vec![];
        let mut sa = vec![];
        let clear = match kind {
            WindowKind::Settings => {
                let ctx = &win.egui.egui_ctx;
                let c = ctx.style_of(ctx.theme()).visuals.panel_fill;
                [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
            }
            _ => [0.0, 0.0, 0.0, 0.0],
        };
        win.frame(clear, |ui| {
            match kind {
                WindowKind::Palette => pa = palette.ui(ui),
                WindowKind::Overlay => oa = overlay.ui(ui),
                WindowKind::Settings => {
                    egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        sa = settings.ui(ui);
                    });
                }
            }
            used = ui.min_rect().size();
        });
        // Floating cards hug their content.
        if kind != WindowKind::Settings && used.y > 1.0 {
            let scale = win.window.scale_factor();
            let current: LogicalSize<f64> = win.window.inner_size().to_logical(scale);
            let want_h = used.y.ceil() as f64 + 1.0;
            let want_w = used.x.ceil() as f64 + 1.0;
            if (current.height - want_h).abs() > 1.0 || (current.width - want_w).abs() > 1.0 {
                let _ = win.window.request_inner_size(LogicalSize::new(want_w, want_h));
                win.window.request_redraw();
                if kind == WindowKind::Overlay && self.core.cfg().suggestions.corner.contains("bottom") {
                    self.place(event_loop, WindowKind::Overlay);
                }
            }
        }
        for a in pa {
            match a {
                PaletteAction::Submit(t) => self.palette_submit(t),
                PaletteAction::Insert(t) => self.insert(t),
                PaletteAction::Copy(t) => self.copy(t),
                PaletteAction::Hide => self.hide_palette(),
            }
        }
        for a in oa {
            self.on_overlay_action(a);
        }
        for a in sa {
            self.on_settings_action(a);
        }
        if kind == WindowKind::Overlay && self.overlay.animating() {
            self.repaint_at.insert(kind, Instant::now() + Duration::from_millis(16));
        }
    }

    fn tick_overlay(&mut self) {
        if let Some(deadline) = self.overlay.deadline {
            if Instant::now() >= deadline {
                self.overlay.deadline = None;
                if let Some(a) = self.overlay.expire() {
                    self.on_overlay_action(a);
                }
                self.redraw(WindowKind::Overlay);
            }
        }
        if self.overlay.tick() {
            self.set_visible(WindowKind::Overlay, false);
        }
    }
}

/// The settings window follows the system light/dark theme (egui's default behavior).
fn egui_extras_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::System);
}

impl ApplicationHandler<UiEvent> for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::Init) {
            self.start(event_loop);
        }
        let now = Instant::now();
        let due: Vec<WindowKind> = self.repaint_at.iter().filter(|(_, t)| **t <= now).map(|(k, _)| *k).collect();
        for k in due {
            self.repaint_at.remove(&k);
            self.redraw(k);
        }
        self.tick_overlay();
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.start(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, ev: UiEvent) {
        match ev {
            UiEvent::Suggestion(s) => {
                if !self.visible.contains(&WindowKind::Palette) {
                    self.overlay.show_suggestion(s, self.core.cfg().suggestions.display_seconds);
                    self.show_overlay(event_loop);
                }
            }
            UiEvent::JobStatus(job, text) => {
                let origin = self.jobs.get(&job).map(|j| j.0).unwrap_or(Origin::Overlay);
                if origin == Origin::Palette && self.visible.contains(&WindowKind::Palette) && self.palette.job == Some(job) {
                    self.palette.set_status(&text);
                    self.redraw(WindowKind::Palette);
                } else {
                    self.overlay.update_status(job, &text);
                    self.redraw(WindowKind::Overlay);
                }
            }
            UiEvent::JobDone { job, result, error } => {
                let (origin, title) = self.jobs.remove(&job).unwrap_or((Origin::Overlay, String::new()));
                let text = error.clone().unwrap_or_else(|| result.as_ref().map(|r| r.text.clone()).unwrap_or_default());
                if result.as_ref().is_some_and(|r| r.inserted) {
                    let note = if !text.is_empty() && text.chars().count() < 80 { format!("Inserted ✓ — {text}") } else { "Inserted ✓".into() };
                    self.toast(event_loop, &note, 4);
                    self.show_overlay(event_loop);
                } else if origin == Origin::Palette && self.visible.contains(&WindowKind::Palette) && self.palette.job == Some(job) {
                    self.palette.show_answer(&text, error.is_some());
                    self.redraw(WindowKind::Palette);
                } else {
                    let title = if title.is_empty() { "Answer".to_string() } else { title };
                    self.overlay.show_result(job, &title, &text, error.is_some());
                    self.show_overlay(event_loop);
                }
            }
            UiEvent::Notify(text) => {
                self.overlay.toast(&text, 6);
                self.show_overlay(event_loop);
            }
            UiEvent::ConfirmSend(details, tx) => {
                self.overlay.show_confirm(details, tx);
                self.show_overlay(event_loop);
            }
            UiEvent::HideForInsert(tx) => {
                self.hide_palette();
                let _ = tx.send(());
            }
            UiEvent::Toggle | UiEvent::Hotkey => self.toggle_palette(),
            UiEvent::Ask(text) => {
                let submit = !text.is_empty();
                self.core.open_palette(text, submit, None);
            }
            UiEvent::OpenSettings => self.open_settings(event_loop),
            UiEvent::Paused(p) => {
                if let Some(t) = &self.tray {
                    t.set_paused(p);
                }
                self.toast(event_loop, if p { "Suggestions paused" } else { "Suggestions on" }, 3);
                self.show_overlay(event_loop);
            }
            UiEvent::Quit => self.quit(event_loop),
            UiEvent::PaletteReady { snap, prefill, submit, answer } => self.show_palette(event_loop, *snap, &prefill, submit, answer),
            UiEvent::Servers(r) => {
                self.settings.show_servers(r);
                self.redraw(WindowKind::Settings);
            }
            UiEvent::OllamaUsed(r) => {
                self.settings.use_done(r);
                self.redraw(WindowKind::Settings);
            }
            UiEvent::MailAccounts(r) => {
                self.settings.show_accounts(r);
                self.redraw(WindowKind::Settings);
            }
            UiEvent::MailSaved(r) => {
                self.settings.mail_saved(r, self.core.cfg());
                self.redraw(WindowKind::Settings);
            }
            UiEvent::Redraw(kind, delay) => {
                if delay.is_zero() {
                    self.redraw(kind);
                } else if let Some(at) = Instant::now().checked_add(delay) {
                    let e = self.repaint_at.entry(kind).or_insert(at);
                    *e = (*e).min(at);
                }
            }
            UiEvent::TrayMenu(id) => match id.as_str() {
                "ask" => self.toggle_palette(),
                "suggest" => self.core.wake(),
                "pause" => {
                    let p = !self.core.paused.load(Ordering::SeqCst);
                    self.core.set_paused(p);
                    self.user_event(event_loop, UiEvent::Paused(p));
                }
                "settings" => self.open_settings(event_loop),
                "quit" => self.quit(event_loop),
                _ => {}
            },
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(&kind) = self.ids.get(&id) else { return };
        tracing::trace!("{kind:?}: {event:?}");
        match &event {
            WindowEvent::RedrawRequested => {
                self.paint(event_loop, kind);
                return;
            }
            WindowEvent::CloseRequested => {
                match kind {
                    WindowKind::Palette => self.hide_palette(),
                    WindowKind::Settings => self.set_visible(WindowKind::Settings, false),
                    WindowKind::Overlay => {}
                }
                return;
            }
            WindowEvent::Resized(size) => {
                if let Some(w) = self.windows.get(&kind) {
                    w.resize(*size);
                }
            }
            _ => {}
        }
        if let Some(w) = self.windows.get_mut(&kind) {
            let resp = w.egui.on_window_event(&w.window, &event);
            if resp.repaint {
                w.window.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let mut next: Option<Instant> = self.repaint_at.values().min().copied();
        if let Some(d) = self.overlay.deadline {
            next = Some(next.map_or(d, |n| n.min(d)));
        }
        if self.overlay.animating() {
            let soon = Instant::now() + Duration::from_millis(16);
            next = Some(next.map_or(soon, |n| n.min(soon)));
        }
        event_loop.set_control_flow(match next {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        for w in self.windows.values_mut() {
            w.destroy();
        }
    }
}
