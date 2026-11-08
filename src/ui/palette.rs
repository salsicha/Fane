//! The command palette: summoned by hotkey, or by editing a suggestion.

use egui::{FontId, Key, Modifiers, RichText};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use crate::snapshot::Snapshot;
use crate::ui::style;

pub const WIDTH: f32 = 680.0;

pub enum PaletteAction {
    Submit(String),
    Insert(String),
    Copy(String),
    Hide,
}

#[derive(Default)]
pub struct Palette {
    pub job: Option<u64>,
    pub snapshot: Option<Snapshot>,
    input: String,
    status: Option<String>,
    answer: Option<(String, bool)>, // (text, is_error)
    history: Vec<String>,
    hist_pos: usize,
    focus: bool,
    md: CommonMarkCache,
}

impl Palette {
    pub fn present(&mut self, snap: Snapshot, prefill: &str) {
        self.snapshot = Some(snap);
        self.job = None;
        self.status = None;
        self.answer = None;
        self.input = prefill.to_string();
        self.focus = true;
    }

    pub fn submit(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.history.retain(|h| h != &text);
        self.history.push(text.clone());
        let excess = self.history.len().saturating_sub(30);
        self.history.drain(..excess);
        self.hist_pos = self.history.len();
        self.answer = None;
        self.status = Some("Thinking…".into());
        Some(text)
    }

    pub fn set_status(&mut self, text: &str) {
        self.status = Some(text.to_string());
    }

    pub fn show_answer(&mut self, text: &str, error: bool) {
        self.status = None;
        self.answer = Some((text.to_string(), error));
    }

    fn answer_text(&self) -> Option<String> {
        self.answer.as_ref().filter(|(t, e)| !e && !t.is_empty()).map(|(t, _)| t.clone())
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Vec<PaletteAction> {
        let mut actions = vec![];
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            actions.push(PaletteAction::Hide);
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter))
            && let Some(t) = self.answer_text()
        {
            actions.push(PaletteAction::Insert(t));
        }
        style::card().show(ui, |ui| {
            ui.set_width(WIDTH - 28.0);
            ui.horizontal(|ui| {
                ui.label(style::brand());
                if let Some(s) = &self.snapshot {
                    let label = s.window.label();
                    let label: String = if label.chars().count() > 70 {
                        label.chars().take(70).collect::<String>() + "…"
                    } else {
                        label
                    };
                    ui.label(style::meta(&format!("· {label}")));
                }
            });

            // History with Up/Down while the input has focus.
            let input_id = ui.make_persistent_id("palette-input");
            let has_focus = ui.memory(|m| m.has_focus(input_id));
            if has_focus && !self.history.is_empty() {
                let step = if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                    -1
                } else if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                    1
                } else {
                    0
                };
                if step != 0 {
                    self.hist_pos = (self.hist_pos as i64 + step).clamp(0, self.history.len() as i64) as usize;
                    self.input = self.history.get(self.hist_pos).cloned().unwrap_or_default();
                }
            }
            let edit = egui::TextEdit::singleline(&mut self.input)
                .id(input_id)
                .hint_text("Ask, or tell me what to do here…")
                .font(FontId::proportional(18.0))
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY);
            let resp = ui.add(edit);
            if self.focus {
                resp.request_focus();
                self.focus = false;
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.command) {
                if let Some(text) = self.submit() {
                    actions.push(PaletteAction::Submit(text));
                }
                resp.request_focus();
            }

            if let Some(s) = &self.status {
                ui.label(style::status(s));
            }
            if let Some((text, error)) = self.answer.clone() {
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 8))
                    .corner_radius(8)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if error {
                                ui.label(RichText::new(format!("Error: {text}")).color(style::ERROR));
                            } else {
                                CommonMarkViewer::new().show(ui, &mut self.md, &text);
                            }
                        });
                    });
                if !error && !text.is_empty() {
                    ui.add_space(4.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if style::primary(ui, "Insert at cursor").clicked() {
                            actions.push(PaletteAction::Insert(text.clone()));
                        }
                        if style::button(ui, "Copy").clicked() {
                            actions.push(PaletteAction::Copy(text.clone()));
                        }
                    });
                }
            }
        });
        actions
    }
}
