//! One OS window = winit window + GL surface/context + its own egui instance.
//!
//! eframe wants a visible root window, which a tray app doesn't have, so Fane drives winit
//! itself and gives each of its windows (palette, overlay card, settings) one of these.

use std::num::NonZeroU32;
use std::sync::Arc;

use egui_glow::glow;
use glutin::config::{Config as GlConfig, ConfigTemplateBuilder, GlConfig as _};
use glutin::context::{ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext, PossiblyCurrentGlContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use winit::event_loop::ActiveEventLoop;
use winit::raw_window_handle::HasWindowHandle;
use winit::window::{Window, WindowAttributes};

/// Pick (once) a GL config that supports a transparent framebuffer if possible.
pub fn gl_config(event_loop: &ActiveEventLoop) -> anyhow::Result<GlConfig> {
    let template = ConfigTemplateBuilder::new()
        .prefer_hardware_accelerated(None)
        .with_depth_size(0)
        .with_stencil_size(0)
        .with_transparency(true);
    let (_, config) = glutin_winit::DisplayBuilder::new()
        .with_preference(glutin_winit::ApiPreference::FallbackEgl)
        .build(event_loop, template, |configs| {
            let configs: Vec<GlConfig> = configs.collect();
            configs
                .iter()
                .find(|c| c.supports_transparency().unwrap_or(false))
                .or(configs.first())
                .cloned()
                .expect("at least one GL config")
        })
        .map_err(|e| anyhow::anyhow!("no usable OpenGL configuration: {e}"))?;
    Ok(config)
}

pub struct GlWindow {
    pub window: Window,
    context: PossiblyCurrentContext,
    surface: Surface<WindowSurface>,
    pub egui: egui_glow::EguiGlow,
    pub gl: Arc<glow::Context>,
}

impl GlWindow {
    pub fn new(event_loop: &ActiveEventLoop, config: &GlConfig, attrs: WindowAttributes) -> anyhow::Result<Self> {
        let window = glutin_winit::finalize_window(event_loop, attrs, config)
            .map_err(|e| anyhow::anyhow!("cannot create window: {e}"))?;
        let display = config.display();
        let raw = window.window_handle()?.as_raw();
        let attrs_gl = ContextAttributesBuilder::new().build(Some(raw));
        let attrs_gles = ContextAttributesBuilder::new().with_context_api(ContextApi::Gles(None)).build(Some(raw));
        // SAFETY: the raw window handle is valid for the lifetime of `window`, which we own.
        let not_current = unsafe {
            display
                .create_context(config, &attrs_gl)
                .or_else(|_| display.create_context(config, &attrs_gles))
                .map_err(|e| anyhow::anyhow!("cannot create an OpenGL context: {e}"))?
        };
        let (w, h): (u32, u32) = window.inner_size().into();
        let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw,
            NonZeroU32::new(w).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(h).unwrap_or(NonZeroU32::MIN),
        );
        // SAFETY: as above.
        let surface = unsafe { display.create_window_surface(config, &surface_attrs)? };
        let context = not_current.make_current(&surface)?;
        let _ = surface.set_swap_interval(&context, SwapInterval::Wait(NonZeroU32::MIN));
        // SAFETY: the context is current on this thread.
        let gl = Arc::new(unsafe {
            glow::Context::from_loader_function(|s| {
                let s = std::ffi::CString::new(s).expect("GL symbol name");
                display.get_proc_address(&s)
            })
        });
        let egui = egui_glow::EguiGlow::new(event_loop, gl.clone(), None, Some(window.scale_factor() as f32), true);
        Ok(Self { window, context, surface, egui, gl })
    }

    fn make_current(&self) {
        if !self.context.is_current() {
            let _ = self.context.make_current(&self.surface);
        }
    }

    pub fn resize(&self, size: winit::dpi::PhysicalSize<u32>) {
        self.make_current();
        self.surface.resize(
            &self.context,
            NonZeroU32::new(size.width).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(size.height).unwrap_or(NonZeroU32::MIN),
        );
    }

    /// Run one egui pass and paint it. `clear` is the background (alpha 0 = see-through).
    pub fn frame(&mut self, clear: [f32; 4], run_ui: impl FnMut(&mut egui::Ui)) {
        self.make_current();
        self.egui.run(&self.window, run_ui);
        use glow::HasContext as _;
        // SAFETY: the context is current.
        unsafe {
            self.gl.clear_color(clear[0], clear[1], clear[2], clear[3]);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        self.egui.paint(&self.window);
        if let Some(dir) = std::env::var_os("FANE_DEBUG_SNAPSHOT") {
            self.save_frame(std::path::Path::new(&dir));
        }
        if let Err(e) = self.surface.swap_buffers(&self.context) {
            tracing::warn!("swap_buffers failed: {e}");
        }
    }

    /// Debugging aid: write what was just rendered to `<dir>/<window title>.png`.
    fn save_frame(&self, dir: &std::path::Path) {
        use glow::HasContext as _;
        let (w, h): (u32, u32) = self.window.inner_size().into();
        let mut px = vec![0u8; (w * h * 4) as usize];
        // SAFETY: the context is current and `px` holds exactly w*h RGBA pixels.
        unsafe {
            self.gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut px)));
        }
        if let Some(img) = image::RgbaImage::from_raw(w, h, px) {
            let img = image::imageops::flip_vertical(&img);
            let _ = img.save(dir.join(format!("{}.png", self.window.title())));
        }
    }

    pub fn destroy(&mut self) {
        self.make_current();
        self.egui.destroy();
    }
}
