//! Small, non-focusable card in the notification corner for suggestions and results.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::suggester::Suggestion;
use crate::ui::style;

pub const WIDTH: f32 = 380.0;
const FADE: Duration = Duration::from_millis(180);

pub enum Card {
    Suggestion(Suggestion),
    Working {
        job: u64,
        title: String,
        status: String,
    },
    Result {
        job: u64,
        title: String,
        text: String,
        error: bool,
    },
    Toast(String),
    /// "Send this email?" owns the card until answered; unanswered = not sent.
    Confirm {
        details: Value,
        tx: Option<oneshot::Sender<bool>>,
    },
    /// First-run setup offer.
    Offer {
        title: String,
        text: String,
    },
}

pub enum OverlayAction {
    Run(Suggestion),
    Edit(Suggestion),
    Dismissed(Option<Suggestion>),
    Expired(Option<Suggestion>),
    Insert(String),
    Open(String),
    Copy(String),
    SetupAccepted,
    SetupDeclined,
}

#[derive(Default)]
pub struct Overlay {
    card: Option<Card>,
    pub deadline: Option<Instant>,
    shown_at: Option<Instant>,
    hiding_since: Option<Instant>,
    deferred: VecDeque<(Card, u64)>,
    md: CommonMarkCache,
    hovered: bool,
}

impl Overlay {
    /// Whether the window should be on screen.
    pub fn visible(&self) -> bool {
        self.card.is_some()
    }

    pub fn has_suggestion(&self) -> bool {
        matches!(self.card, Some(Card::Suggestion(_))) && self.hiding_since.is_none()
    }

    pub fn confirming(&self) -> bool {
        matches!(self.card, Some(Card::Confirm { .. }))
    }

    pub fn animating(&self) -> bool {
        let busy = |t: Option<Instant>| t.is_some_and(|t| t.elapsed() < FADE + Duration::from_millis(20));
        busy(self.shown_at) || self.hiding_since.is_some()
    }

    fn job(&self) -> Option<u64> {
        match &self.card {
            Some(Card::Working { job, .. } | Card::Result { job, .. }) => Some(*job),
            _ => None,
        }
    }

    fn present(&mut self, card: Card, seconds: u64) {
        let was_visible = self.card.is_some() && self.hiding_since.is_none();
        self.card = Some(card);
        self.hiding_since = None;
        if !was_visible {
            self.shown_at = Some(Instant::now());
        }
        self.deadline = (seconds > 0).then(|| Instant::now() + Duration::from_secs(seconds));
    }

    /// Queue `card` if a confirmation owns the overlay. Returns true if deferred.
    fn defer(&mut self, card: Card, seconds: u64) -> Option<(Card, u64)> {
        if self.confirming() {
            self.deferred.push_back((card, seconds));
            None
        } else {
            Some((card, seconds))
        }
    }

    pub fn show_suggestion(&mut self, s: Suggestion, seconds: u64) {
        if self.confirming() {
            return; // stale by the time the user is free; drop it
        }
        self.present(Card::Suggestion(s), seconds);
    }

    pub fn show_working(&mut self, job: u64, title: &str) {
        if let Some((c, s)) = self.defer(Card::Working { job, title: title.into(), status: "Working…".into() }, 0) {
            self.present(c, s);
        }
    }

    pub fn update_status(&mut self, job: u64, text: &str) {
        if let Some(Card::Working { job: j, status, .. }) = &mut self.card
            && *j == job
        {
            *status = text.to_string();
        }
    }

    pub fn show_result(&mut self, job: u64, title: &str, text: &str, error: bool) {
        let card = Card::Result { job, title: title.into(), text: text.into(), error };
        if let Some((c, s)) = self.defer(card, 45) {
            if self.job().is_some_and(|j| j != job) {
                return;
            }
            self.present(c, s);
        }
    }

    pub fn toast(&mut self, text: &str, seconds: u64) {
        if let Some((c, s)) = self.defer(Card::Toast(text.into()), seconds) {
            if self.has_suggestion() {
                return; // don't clobber a pending suggestion with a notice
            }
            self.present(c, s);
        }
    }

    pub fn show_offer(&mut self, title: &str, text: &str) {
        if let Some((c, s)) = self.defer(Card::Offer { title: title.into(), text: text.into() }, 120) {
            self.present(c, s);
        }
    }

    pub fn show_confirm(&mut self, details: Value, tx: oneshot::Sender<bool>) {
        if let Some((c, s)) = self.defer(Card::Confirm { details, tx: Some(tx) }, 180) {
            self.present(c, s);
        }
    }

    fn resolve(&mut self, ok: bool) {
        if let Some(Card::Confirm { tx, .. }) = &mut self.card
            && let Some(tx) = tx.take()
        {
            let _ = tx.send(ok);
        }
        self.card = None;
        self.drain();
    }

    fn drain(&mut self) {
        if let Some((card, secs)) = self.deferred.pop_front() {
            self.hiding_since = None;
            self.present(card, secs);
        } else {
            self.fade_out();
        }
    }

    fn fade_out(&mut self) {
        self.deadline = None;
        if self.card.is_some() && self.hiding_since.is_none() {
            self.hiding_since = Some(Instant::now());
        } else if self.card.is_none() {
            self.hiding_since = Some(Instant::now() - FADE);
        }
    }

    fn take_suggestion(&mut self) -> Option<Suggestion> {
        match self.card.take() {
            Some(Card::Suggestion(s)) => Some(s),
            other => {
                self.card = other;
                None
            }
        }
    }

    /// Called when the deadline passes.
    pub fn expire(&mut self) -> Option<OverlayAction> {
        if self.hovered {
            self.deadline = Some(Instant::now() + Duration::from_secs(3));
            return None;
        }
        if self.confirming() {
            self.resolve(false);
            return None;
        }
        let s = self.take_suggestion();
        let had = s.is_some();
        if had {
            self.card = Some(Card::Toast(String::new())); // keep something to fade
        }
        self.fade_out();
        Some(OverlayAction::Expired(s))
    }

    /// Advance the fade-out; returns true when the window should now be hidden.
    pub fn tick(&mut self) -> bool {
        if let Some(t) = self.hiding_since
            && t.elapsed() >= FADE
        {
            self.hiding_since = None;
            self.card = None;
            self.shown_at = None;
            return true;
        }
        false
    }

    fn opacity(&self) -> f32 {
        let f = FADE.as_secs_f32();
        if let Some(t) = self.hiding_since {
            return (1.0 - t.elapsed().as_secs_f32() / f).clamp(0.0, 1.0);
        }
        self.shown_at.map_or(1.0, |t| (t.elapsed().as_secs_f32() / f).clamp(0.0, 1.0))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Vec<OverlayAction> {
        let mut actions = vec![];
        ui.set_opacity(self.opacity());
        let mut close = false;
        let mut buttons: Vec<(String, bool, u8)> = vec![];
        let (meta, title, body, markdown, max_h) = match &self.card {
            None => return actions,
            Some(Card::Suggestion(s)) => {
                buttons = vec![("Edit".into(), false, 1), ("Run".into(), true, 2)];
                (short_app(&s.window), s.title.clone(), s.prompt.clone(), false, 64.0)
            }
            Some(Card::Working { title, status, .. }) => (String::new(), title.clone(), status.clone(), false, 40.0),
            Some(Card::Result { title, text, error, .. }) => {
                if !error {
                    buttons = vec![("Open".into(), false, 3), ("Copy".into(), false, 4), ("Insert".into(), true, 5)];
                }
                let t = if *error { "Couldn't do that".to_string() } else { title.clone() };
                (String::new(), t, text.clone(), !*error, 240.0)
            }
            Some(Card::Toast(text)) => (String::new(), text.clone(), String::new(), false, 0.0),
            Some(Card::Offer { title, text }) => {
                buttons = vec![("Not now".into(), false, 8), ("Set up".into(), true, 9)];
                ("· setup".into(), title.clone(), text.clone(), false, 80.0)
            }
            Some(Card::Confirm { details, .. }) if details.get("title").is_some() => {
                // Generic confirmation (file changes, ...): title, meta, text, ok label.
                let s = |k: &str| details[k].as_str().unwrap_or("").to_string();
                let ok = if s("ok").is_empty() { "OK".to_string() } else { s("ok") };
                buttons = vec![("Cancel".into(), false, 6), (ok, true, 7)];
                (s("meta"), s("title"), s("text"), false, 220.0)
            }
            Some(Card::Confirm { details, .. }) => {
                buttons = vec![("Cancel".into(), false, 6), ("Send".into(), true, 7)];
                let list = |k: &str| {
                    details[k]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default()
                };
                let mut to = list("to");
                let cc = list("cc");
                if !cc.is_empty() {
                    to = format!("{to}, {cc}");
                }
                let s = |k: &str| details[k].as_str().unwrap_or("").to_string();
                let body = format!("To: {to}\nSubject: {}\n\n{}", s("subject"), s("body"));
                (format!("· from {}", s("from")), "Send this email?".into(), body, false, 220.0)
            }
        };
        let mut clicked: Option<u8> = None;
        let resp = style::card().show(ui, |ui| {
            ui.set_width(WIDTH - 28.0);
            ui.horizontal(|ui| {
                ui.label(style::brand());
                ui.label(style::meta(&meta));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if style::close_button(ui).clicked() {
                        close = true;
                    }
                });
            });
            if !title.is_empty() {
                ui.label(style::title(&title));
            }
            if !body.is_empty() {
                egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if markdown {
                        CommonMarkViewer::new().show(ui, &mut self.md, &body);
                    } else {
                        ui.label(style::body(&body));
                    }
                });
            }
            if !buttons.is_empty() {
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (label, primary, id) in buttons.iter().rev() {
                        let r = if *primary { style::primary(ui, label) } else { style::button(ui, label) };
                        if r.clicked() {
                            clicked = Some(*id);
                        }
                    }
                });
            }
        });
        self.hovered =
            resp.response.rect.contains(ui.input(|i| i.pointer.hover_pos()).unwrap_or(egui::pos2(-1.0, -1.0)));

        if close && matches!(self.card, Some(Card::Offer { .. })) {
            self.fade_out();
            actions.push(OverlayAction::SetupDeclined);
            close = false;
        }
        if close {
            if self.confirming() {
                self.resolve(false);
            } else {
                let s = self.take_suggestion();
                if s.is_some() {
                    self.card = Some(Card::Toast(String::new()));
                }
                self.fade_out();
                actions.push(OverlayAction::Dismissed(s));
            }
        }
        let text = match &self.card {
            Some(Card::Result { text, .. }) => text.clone(),
            _ => String::new(),
        };
        match clicked {
            Some(1) | Some(2) => {
                if let Some(s) = self.take_suggestion() {
                    self.card = Some(Card::Toast(String::new()));
                    self.fade_out();
                    actions.push(if clicked == Some(1) { OverlayAction::Edit(s) } else { OverlayAction::Run(s) });
                }
            }
            Some(3) => {
                self.fade_out();
                actions.push(OverlayAction::Open(text));
            }
            Some(4) => {
                self.fade_out();
                actions.push(OverlayAction::Copy(text));
            }
            Some(5) => {
                self.fade_out();
                actions.push(OverlayAction::Insert(text));
            }
            Some(8) => {
                self.fade_out();
                actions.push(OverlayAction::SetupDeclined);
            }
            Some(9) => {
                self.fade_out();
                actions.push(OverlayAction::SetupAccepted);
            }
            Some(6) => self.resolve(false),
            Some(7) => self.resolve(true),
            _ => {}
        }
        actions
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        if self.confirming() {
            self.resolve(false);
        }
    }
}

fn short_app(label: &str) -> String {
    let app = label.split(" — ").next().unwrap_or("");
    if app.is_empty() { String::new() } else { format!("· {}", app.chars().take(40).collect::<String>()) }
}
