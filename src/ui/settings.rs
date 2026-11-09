//! Settings window: which Ollama server Fane talks to, and Fane's own email.

use egui::{Color32, RichText};

use crate::config::Config;
use crate::discovery::OllamaServer;
use crate::llm::choose_model;
use crate::mail::accounts::GoaAccount;

pub enum SettingsAction {
    Scan,
    UseServer(String),
    RefreshAccounts,
    SelectAccount(String),
    SaveOwner(String),
    ManageAccounts,
    /// Install Ollama / download the model if missing.
    Setup,
    SaveFileRoots(Vec<String>),
    ForgetPref(i64),
    RebuildIndex,
    EditConfig,
    Close,
}

pub struct Settings {
    cfg: Config,
    servers: Vec<OllamaServer>,
    selected: Option<usize>,
    address: String,
    scanning: bool,
    using: bool,
    server_status: String,
    accounts: Option<Result<Vec<GoaAccount>, String>>,
    owner: String,
    mail_status: String,
    roots: String,
    roots_status: String,
    prefs: Vec<crate::prefs::Pref>,
    index_status: String,
}

impl Settings {
    pub fn new(cfg: Config) -> Self {
        Self {
            address: cfg.model.host.clone(),
            owner: cfg.agent_mail.owner().unwrap_or_default(),
            roots: cfg_roots(&cfg),
            cfg,
            servers: vec![],
            selected: None,
            scanning: false,
            using: false,
            server_status: String::new(),
            accounts: None,
            mail_status: String::new(),
            roots_status: String::new(),
            prefs: vec![],
            index_status: String::new(),
        }
    }

    /// Fresh config when the window opens; returns the actions to kick off.
    pub fn open(&mut self, cfg: Config) -> Vec<SettingsAction> {
        self.address = cfg.model.host.clone();
        self.owner = cfg.agent_mail.owner().unwrap_or_default();
        self.roots = cfg_roots(&cfg);
        self.roots_status.clear();
        self.cfg = cfg;
        self.mail_status.clear();
        let mut actions = vec![];
        if !self.scanning {
            self.scanning = true;
            self.server_status = "Looking for Ollama on this computer and your network…".into();
            actions.push(SettingsAction::Scan);
        }
        if cfg!(target_os = "linux") {
            actions.push(SettingsAction::RefreshAccounts);
        }
        actions
    }

    pub fn show_servers(&mut self, servers: Result<Vec<OllamaServer>, String>) {
        self.scanning = false;
        match servers {
            Ok(servers) => {
                let n = servers.len();
                self.server_status = match n {
                    0 => "No Ollama servers found. Type an address if yours is on another network or port.".into(),
                    1 => "Found 1 Ollama server.".into(),
                    n => format!("Found {n} Ollama servers."),
                };
                self.selected = servers.iter().position(|s| s.matches(&self.cfg.model.host));
                self.servers = servers;
            }
            Err(e) => self.server_status = format!("Scan failed: {e}"),
        }
    }

    pub fn use_done(&mut self, result: Result<(String, String), String>) {
        self.using = false;
        match result {
            Ok((host, model)) => {
                self.cfg.model.host = host.clone();
                self.address = host.clone();
                self.server_status = format!("✓ Using {host} with {model}.");
                self.selected = self.servers.iter().position(|s| s.matches(&host));
            }
            Err(e) => self.server_status = format!("✗ {e}"),
        }
    }

    pub fn show_accounts(&mut self, accounts: Result<Vec<GoaAccount>, String>) {
        self.accounts = Some(accounts);
    }

    /// What Fane remembers and has indexed (refreshed when the window opens or changes).
    pub fn set_memory(&mut self, prefs: Vec<crate::prefs::Pref>, index_status: String) {
        self.prefs = prefs;
        self.index_status = index_status;
    }

    pub fn roots_saved(&mut self, result: Result<String, String>) {
        self.roots_status = match result {
            Ok(m) => format!("✓ {m}."),
            Err(e) => format!("✗ {e}"),
        };
    }

    pub fn mail_saved(&mut self, result: Result<String, String>, cfg: Config) {
        self.cfg = cfg;
        self.mail_status = match result {
            Ok(m) => format!("✓ {m}."),
            Err(e) => format!("✗ {e}"),
        };
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Vec<SettingsAction> {
        let mut actions = vec![];
        let dim = ui.visuals().weak_text_color();
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(4.0);
            ui.heading("Ollama server");
            ui.label("Fane sends its prompts and screenshots to this server. Pick one found on your network, or type an address.");
            ui.add_space(4.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_height(150.0);
                ui.set_width(ui.available_width());
                if self.servers.is_empty() {
                    ui.label(RichText::new(if self.scanning { "Scanning…" } else { "Nothing found yet." }).color(dim));
                }
                for (i, s) in self.servers.iter().enumerate() {
                    let model = choose_model(&self.cfg.model, &s.models);
                    let in_use = s.matches(&self.cfg.model.host);
                    let where_ = s.url.trim_start_matches("http://");
                    let mut title =
                        if s.label() == s.address() { where_.to_string() } else { format!("{}  —  {where_}", s.label()) };
                    if in_use {
                        title += "   (in use)";
                    }
                    let n = s.models.len();
                    let detail = format!(
                        "Ollama {} · {n} model{} · {}",
                        s.version,
                        if n == 1 { "" } else { "s" },
                        match &model {
                            None => "none of Fane's models".to_string(),
                            Some(m) => format!("{} {m}", if in_use { "using" } else { "would use" }),
                        }
                    );
                    let mut t = RichText::new(format!("{title}\n{detail}"));
                    if in_use {
                        t = t.strong();
                    }
                    if model.is_none() {
                        t = t.color(dim);
                    }
                    let r = ui.selectable_label(self.selected == Some(i), t);
                    let r = if model.is_none() {
                        r.on_hover_text(format!("Pull a model there first, e.g. ollama pull {}", self.cfg.model.name))
                    } else {
                        r
                    };
                    if r.clicked() {
                        self.selected = Some(i);
                        self.address = s.url.clone();
                    }
                    if r.double_clicked() && !self.using {
                        self.using = true;
                        self.server_status = format!("Connecting to {}…", s.url);
                        actions.push(SettingsAction::UseServer(s.url.clone()));
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(&self.server_status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!self.scanning, egui::Button::new("Scan again")).clicked() {
                        self.scanning = true;
                        self.server_status = "Looking for Ollama on this computer and your network…".into();
                        actions.push(SettingsAction::Scan);
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label("Address:");
                let use_clicked = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let b = ui.add_enabled(!self.using, egui::Button::new("Use this server")).clicked();
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.address)
                                .hint_text("192.168.1.20 or http://host:11434")
                                .desired_width(f32::INFINITY),
                        );
                        b || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    })
                    .inner;
                if use_clicked && !self.using && !self.address.trim().is_empty() {
                    self.using = true;
                    self.server_status = format!("Connecting to {}…", self.address.trim());
                    actions.push(SettingsAction::UseServer(self.address.trim().to_string()));
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("No Ollama on this computer, or no model yet?").color(dim));
                if ui.button("Install Ollama and Gemma 4…").clicked() {
                    actions.push(SettingsAction::Setup);
                }
            });
            ui.label(
                RichText::new("Traffic to another computer isn't encrypted, so only use servers on a network you trust.")
                    .color(dim)
                    .small(),
            );

            ui.add_space(12.0);
            ui.separator();
            ui.heading("Files");
            ui.label("Folders Fane may use, one per line. It reads files there freely and asks you before changing, moving or deleting anything (deleted files go to the trash). Hidden files and secrets like .ssh are always off-limits.");
            ui.add(egui::TextEdit::multiline(&mut self.roots).desired_rows(3).desired_width(f32::INFINITY).hint_text("~/Documents"));
            ui.horizontal(|ui| {
                if ui.button("Save folders").clicked() {
                    actions.push(SettingsAction::SaveFileRoots(self.roots.lines().map(String::from).collect()));
                }
                ui.label(&self.roots_status);
            });

            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.index_status).color(dim).small());
                if ui.small_button("Rebuild index").clicked() {
                    actions.push(SettingsAction::RebuildIndex);
                }
            });

            ui.add_space(12.0);
            ui.separator();
            ui.heading("Preferences");
            ui.label("How you've asked Fane to do things. Tell Fane a new preference any time (“from now on…”); it replaces an older one on the same topic, and preferences that refer to a folder or tool that's gone are dropped.");
            if self.prefs.is_empty() {
                ui.label(RichText::new("None yet.").color(dim));
            }
            for p in &self.prefs {
                ui.horizontal(|ui| {
                    if ui.small_button("Forget").clicked() {
                        actions.push(SettingsAction::ForgetPref(p.id));
                    }
                    ui.label(RichText::new(&p.about).strong());
                    ui.label(&p.text);
                });
            }

            ui.add_space(12.0);
            ui.separator();
            ui.heading("Email");
            self.mail_ui(ui, &mut actions, dim);

            ui.add_space(12.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Edit config file…").clicked() {
                    actions.push(SettingsAction::EditConfig);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        actions.push(SettingsAction::Close);
                    }
                });
            });
        });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            actions.push(SettingsAction::Close);
        }
        actions
    }

    fn mail_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<SettingsAction>, dim: Color32) {
        ui.label("Your email");
        ui.horizontal(|ui| {
            let save = ui
                .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let b = ui.button("Save").clicked();
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.owner)
                            .hint_text("you@example.com")
                            .desired_width(f32::INFINITY),
                    );
                    b || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                })
                .inner;
            if save {
                actions.push(SettingsAction::SaveOwner(self.owner.trim().to_string()));
            }
        });
        ui.label(
            RichText::new(
                "Only mail from this address (verified by Google) is read, acted on and answered. \
                 Mail from anyone else is ignored and never shown to the model.",
            )
            .color(dim)
            .small(),
        );
        ui.add_space(6.0);
        ui.label("Fane's own Gmail account");
        if !cfg!(target_os = "linux") {
            ui.label(
                RichText::new("Fane's own mailbox uses GNOME Online Accounts, so it's only available on Linux.")
                    .color(dim),
            );
        } else {
            let current = self.cfg.agent_mail.account.trim().to_lowercase();
            ui.horizontal(|ui| {
                let label = if current.is_empty() { "Off (no email access)".to_string() } else { current.clone() };
                egui::ComboBox::from_id_salt("mail-account").selected_text(label).width(320.0).show_ui(ui, |ui| {
                    match &self.accounts {
                        None => {
                            ui.label("Loading…");
                        }
                        Some(Err(e)) => {
                            ui.label(format!("Online Accounts unavailable: {e}"));
                        }
                        Some(Ok(accs)) => {
                            if accs.is_empty() {
                                ui.label("No Google accounts in Online Accounts");
                            }
                            for a in accs {
                                let text = if a.mail_disabled {
                                    format!("{} (mail off in Settings)", a.email)
                                } else {
                                    a.email.clone()
                                };
                                let selected = a.email.to_lowercase() == current;
                                if ui.add_enabled(!a.mail_disabled, egui::Button::selectable(selected, text)).clicked()
                                {
                                    actions.push(SettingsAction::SelectAccount(a.email.clone()));
                                }
                            }
                        }
                    }
                    if ui.selectable_label(current.is_empty(), "Off (no email access)").clicked() {
                        actions.push(SettingsAction::SelectAccount(String::new()));
                    }
                });
                if ui.button("Refresh").clicked() {
                    actions.push(SettingsAction::RefreshAccounts);
                }
                if ui.button("Add or manage accounts…").clicked() {
                    actions.push(SettingsAction::ManageAccounts);
                }
            });
            ui.label(
                RichText::new(
                    "Give Fane an account of its own (Settings → Online Accounts → Google), not your personal one.",
                )
                .color(dim)
                .small(),
            );
        }
        if !self.mail_status.is_empty() {
            ui.label(&self.mail_status);
        }
    }
}

fn cfg_roots(cfg: &Config) -> String {
    cfg.files.roots.join("\n")
}
