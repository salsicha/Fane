//! Tray icon and menu. Linux: StatusNotifierItem over D-Bus (ksni), shown by GNOME's
//! AppIndicator extension (on by default in Ubuntu). macOS/Windows: tray-icon.
//!
//! Menu items send `UiEvent::TrayMenu(id)` with ids: ask, suggest, pause, settings, quit.

use winit::event_loop::EventLoopProxy;

use crate::core::UiEvent;

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use ksni::menu::{CheckmarkItem, StandardItem};
    use ksni::{MenuItem, TrayMethods};

    pub struct FaneTray {
        pub paused: bool,
        pub proxy: EventLoopProxy<UiEvent>,
    }

    impl FaneTray {
        fn send(&self, id: &str) {
            let _ = self.proxy.send_event(UiEvent::TrayMenu(id.into()));
        }
    }

    fn item(label: &str, id: &'static str) -> MenuItem<FaneTray> {
        StandardItem {
            label: label.into(),
            activate: Box::new(move |t: &mut FaneTray| t.send(id)),
            ..Default::default()
        }
        .into()
    }

    impl ksni::Tray for FaneTray {
        fn id(&self) -> String {
            "fane".into()
        }

        fn title(&self) -> String {
            "Fane".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [22u32, 32, 64]
                .iter()
                .map(|&size| {
                    let img = crate::ui::icon::rgba(size);
                    // RGBA -> ARGB32, network byte order.
                    let data = img.pixels().flat_map(|p| [p.0[3], p.0[0], p.0[1], p.0[2]]).collect();
                    ksni::Icon { width: size as i32, height: size as i32, data }
                })
                .collect()
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send("ask");
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                item("Ask Fane…", "ask"),
                item("Suggest something now", "suggest"),
                CheckmarkItem {
                    label: "Pause suggestions".into(),
                    checked: self.paused,
                    activate: Box::new(|t: &mut FaneTray| t.send("pause")),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                item("Settings…", "settings"),
                item("Quit", "quit"),
            ]
        }
    }

    pub struct Tray {
        handle: Option<ksni::Handle<FaneTray>>,
        rt: tokio::runtime::Handle,
    }

    impl Tray {
        pub fn new(proxy: EventLoopProxy<UiEvent>, rt: &tokio::runtime::Handle, paused: bool) -> Self {
            let handle = rt.block_on(async {
                match (FaneTray { paused, proxy }).spawn().await {
                    Ok(h) => Some(h),
                    Err(e) => {
                        tracing::warn!("no tray icon ({e}); use the hotkey or `fane toggle`");
                        None
                    }
                }
            });
            Self { handle, rt: rt.clone() }
        }

        pub fn set_paused(&self, paused: bool) {
            if let Some(h) = self.handle.clone() {
                self.rt.spawn(async move {
                    h.update(|t| t.paused = paused).await;
                });
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod imp {
    use super::*;
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub struct Tray {
        _icon: Option<TrayIcon>,
        pause: CheckMenuItem,
    }

    impl Tray {
        /// Must be called on the main thread once the event loop is running.
        pub fn new(proxy: EventLoopProxy<UiEvent>, _rt: &tokio::runtime::Handle, paused: bool) -> Self {
            let menu = Menu::new();
            let pause = CheckMenuItem::with_id("pause", "Pause suggestions", true, paused, None);
            let _ = menu.append_items(&[
                &MenuItem::with_id("ask", "Ask Fane…", true, None),
                &MenuItem::with_id("suggest", "Suggest something now", true, None),
                &pause,
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id("settings", "Settings…", true, None),
                &MenuItem::with_id("quit", "Quit", true, None),
            ]);
            let p = proxy.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let _ = p.send_event(UiEvent::TrayMenu(e.id().0.clone()));
            }));
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                    if cfg!(target_os = "windows") {
                        let _ = proxy.send_event(UiEvent::TrayMenu("ask".into()));
                    }
                }
            }));
            let img = crate::ui::icon::rgba(64);
            let icon = tray_icon::Icon::from_rgba(img.into_raw(), 64, 64).ok();
            let mut builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Fane");
            if let Some(icon) = icon {
                builder = builder.with_icon(icon);
            }
            let built = match builder.build() {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!("no tray icon: {e}");
                    None
                }
            };
            Self { _icon: built, pause }
        }

        pub fn set_paused(&self, paused: bool) {
            self.pause.set_checked(paused);
        }
    }
}

pub use imp::Tray;
