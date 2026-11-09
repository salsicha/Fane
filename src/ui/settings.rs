//! Settings window: Fane's models (what runs where, GPUs, sharing), other computers and
//! servers, files, preferences, and Fane's own email.

use egui::{Color32, RichText};
use serde_json::Value;

use crate::config::{Config, Role};
use crate::discovery::Server;
use crate::llm::choose_model;
use crate::mail::accounts::GoaAccount;
use crate::setup::Want;

pub enum SettingsAction {
    Scan,
    /// Use a server: another Fane (with its token), stable-diffusion.cpp, or Ollama for chat.
    UseServer(String, String),
    UseOwnModels,
    RemoveRemote(String),
    RefreshModels,
    SetRoute(Role, Vec<String>),
    SetGpu(Role, String),
    Install(Want),
    SetShare(bool),
    SetService(bool),
    SetupApp(String),
    RemoveApp(String),
    Copy(String),
    RefreshAccounts,
    SelectAccount(String),
    SaveOwner(String),
    ManageAccounts,
    SaveFileRoots(Vec<String>),
    ForgetPref(i64),
    RebuildIndex,
    EditConfig,
    Close,
}

pub struct Settings {
    cfg: Config,
    /// The model server's /fane/v1/info.
    info: Value,
    models_status: String,
    servers: Vec<Server>,
    selected: Option<usize>,
    address: String,
    token: String,
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
    apps: Vec<crate::connectors::apps::AppStatus>,
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Chat => "Everyday",
        Role::Code => "Coding",
        Role::Image => "Images",
        Role::Video => "Video",
    }
}

fn target_label(t: &str) -> String {
    match t {
        "local" => "This computer".into(),
        "claude" => "Claude Code".into(),
        "codex" => "Codex".into(),
        other => other.to_string(),
    }
}

impl Settings {
    pub fn new(cfg: Config) -> Self {
        Self {
            address: String::new(),
            token: String::new(),
            owner: cfg.agent_mail.owner().unwrap_or_default(),
            roots: cfg_roots(&cfg),
            cfg,
            info: Value::Null,
            models_status: String::new(),
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
            apps: vec![],
        }
    }

    pub fn set_apps(&mut self, apps: Vec<crate::connectors::apps::AppStatus>) {
        self.apps = apps;
    }

    /// Fresh config when the window opens; returns the actions to kick off.
    pub fn open(&mut self, cfg: Config) -> Vec<SettingsAction> {
        self.owner = cfg.agent_mail.owner().unwrap_or_default();
        self.roots = cfg_roots(&cfg);
        self.roots_status.clear();
        self.cfg = cfg;
        self.mail_status.clear();
        let mut actions = vec![SettingsAction::RefreshModels];
        if !self.scanning {
            self.scanning = true;
            self.server_status = "Looking for other computers and servers on your network…".into();
            actions.push(SettingsAction::Scan);
        }
        if cfg!(target_os = "linux") {
            actions.push(SettingsAction::RefreshAccounts);
        }
        actions
    }

    pub fn set_cfg(&mut self, cfg: Config) {
        self.cfg = cfg;
    }

    pub fn show_models(&mut self, info: Value) {
        self.info = info;
    }

    pub fn models_status(&mut self, result: Result<String, String>) {
        self.models_status = match result {
            Ok(m) => format!("✓ {m}"),
            Err(e) => format!("✗ {e}"),
        };
    }

    pub fn show_servers(&mut self, servers: Result<Vec<Server>, String>) {
        self.scanning = false;
        match servers {
            Ok(servers) => {
                self.server_status = match servers.len() {
                    0 => "Nothing found. Type an address if it's on another network or port.".into(),
                    1 => "Found 1 server.".into(),
                    n => format!("Found {n} servers."),
                };
                self.selected = None;
                self.servers = servers;
            }
            Err(e) => self.server_status = format!("Scan failed: {e}"),
        }
    }

    pub fn use_done(&mut self, result: Result<(String, String), String>) {
        self.using = false;
        match result {
            Ok((name, model)) if model.is_empty() => {
                self.server_status = format!("✓ Added {name}. Choose what to send to it above.");
                self.token.clear();
            }
            Ok((host, model)) => {
                self.cfg.model.host = host.clone();
                self.server_status = format!("✓ Chat now uses {host} ({model}).");
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
            self.models_ui(ui, &mut actions, dim);
            ui.add_space(12.0);
            ui.separator();
            self.servers_ui(ui, &mut actions, dim);
            ui.add_space(12.0);
            ui.separator();
            self.apps_ui(ui, &mut actions, dim);
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

    fn models_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<SettingsAction>, dim: Color32) {
        ui.heading("Models");
        let own = self.cfg.model.host.trim().is_empty();
        if !own {
            ui.horizontal(|ui| {
                ui.label(format!("Everyday chat goes to {}.", self.cfg.model.host));
                if ui.button("Use Fane's own models").clicked() {
                    actions.push(SettingsAction::UseOwnModels);
                }
            });
        }
        if let Some(e) = self.info["error"].as_str() {
            ui.label(RichText::new(e).color(dim));
        }
        let gpus: Vec<(String, String)> = self.info["gpus"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|g| Some((g["id"].as_str()?.to_string(), g["label"].as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let remotes: Vec<String> = self.cfg.remotes.keys().cloned().collect();
        let mut missing = Want::default();
        let mut chat_missing = false;
        egui::Grid::new("roles").num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
            for role in Role::ALL {
                let r = &self.info["roles"][role.as_str()];
                let model = r["model"].as_str().unwrap_or("");
                let ready = r["ready"].as_bool().unwrap_or(false);
                let route = self.cfg.routes.for_role(role).to_vec();
                let first = route.first().cloned().unwrap_or_else(|| "local".into());
                ui.label(RichText::new(role_label(role)).strong());
                let state = if model.is_empty() {
                    "off".to_string()
                } else if first != "local" || ready {
                    model.to_string()
                } else {
                    format!("{model} (not downloaded)")
                };
                ui.label(if first == "local" && !ready {
                    RichText::new(state).color(dim)
                } else {
                    RichText::new(state)
                });
                // Where it runs: this computer, a remote, or (coding/images) Claude Code / Codex.
                let mut options = vec!["local".to_string()];
                options.extend(remotes.iter().cloned());
                match role {
                    Role::Code => options.extend(["claude".into(), "codex".into()]),
                    Role::Image => options.push("codex".into()),
                    _ => {}
                }
                egui::ComboBox::from_id_salt(("where", role.as_str()))
                    .selected_text(target_label(&first))
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for o in &options {
                            if ui.selectable_label(*o == first, target_label(o)).clicked() && *o != first {
                                let mut new = vec![o.clone()];
                                new.extend(route.iter().filter(|t| *t != o).cloned());
                                actions.push(SettingsAction::SetRoute(role, new));
                            }
                        }
                    });
                if first == "local" {
                    let current = match role {
                        Role::Chat => &self.cfg.runtime.chat_gpu,
                        Role::Code => &self.cfg.runtime.code_gpu,
                        Role::Image => &self.cfg.runtime.image_gpu,
                        Role::Video => &self.cfg.runtime.video_gpu,
                    };
                    let shown = if current == "auto" {
                        format!("Auto ({})", r["gpu_label"].as_str().unwrap_or("best GPU"))
                    } else {
                        gpus.iter().find(|(id, _)| id == current).map(|(_, l)| l.clone()).unwrap_or(current.clone())
                    };
                    egui::ComboBox::from_id_salt(("gpu", role.as_str())).selected_text(shown).width(260.0).show_ui(
                        ui,
                        |ui| {
                            if ui.selectable_label(current == "auto", "Auto (the most capable GPU)").clicked() {
                                actions.push(SettingsAction::SetGpu(role, "auto".into()));
                            }
                            for (id, label) in &gpus {
                                if ui.selectable_label(current == id, label).clicked() {
                                    actions.push(SettingsAction::SetGpu(role, id.clone()));
                                }
                            }
                        },
                    );
                    if !ready && !model.is_empty() {
                        match role {
                            Role::Chat => chat_missing = true,
                            Role::Code => missing.code = true,
                            Role::Image => missing.images = true,
                            Role::Video => missing.video = true,
                        }
                    }
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
        if (chat_missing && own) || missing != Want::default() {
            let what: Vec<&str> = [
                (chat_missing && own, "the everyday model"),
                (missing.code, "the coding model (about 19 GB)"),
                (missing.images, "image generation (about 11 GB)"),
                (missing.video, "video generation (about 13 GB)"),
            ]
            .into_iter()
            .filter(|(b, _)| *b)
            .map(|(_, t)| t)
            .collect();
            ui.horizontal(|ui| {
                if ui.button("Download").clicked() {
                    actions.push(SettingsAction::Install(missing));
                }
                ui.label(
                    RichText::new(format!("Gets {} (reusing Ollama's downloads where it can).", what.join(", ")))
                        .color(dim),
                );
            });
        }
        let loaded: Vec<String> = self.info["loaded"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|m| {
                        let gpu = m["gpu"].as_str().unwrap_or("");
                        let gpu = gpus
                            .iter()
                            .find(|(id, _)| id == gpu)
                            .map(|(_, l)| l.split(" (").next().unwrap_or(l).to_string())
                            .unwrap_or(gpu.to_string());
                        format!("{} on {gpu}", m["name"].as_str().unwrap_or("?"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        ui.label(
            RichText::new(if loaded.is_empty() {
                "Nothing loaded right now.".to_string()
            } else {
                format!("Loaded now: {}.", loaded.join(", "))
            })
            .color(dim)
            .small(),
        );
        ui.label(
            RichText::new("Models on the same GPU take turns when they don't fit in its memory together; idle models stop after 15 minutes.")
                .color(dim)
                .small(),
        );
        ui.add_space(6.0);
        let mut share = self.cfg.runtime.share;
        if ui.checkbox(&mut share, "Let other computers on my network use this computer's models").changed() {
            actions.push(SettingsAction::SetShare(share));
        }
        if self.cfg.runtime.share && !self.cfg.runtime.token.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Token for the other computer:");
                ui.label(RichText::new(&self.cfg.runtime.token).monospace());
                if ui.small_button("Copy").clicked() {
                    actions.push(SettingsAction::Copy(self.cfg.runtime.token.clone()));
                }
            });
            ui.label(
                RichText::new(format!(
                    "On the other computer, add {} under Other computers with this token. Traffic isn't encrypted; use it on networks you trust.",
                    self.info["name"].as_str().unwrap_or("this computer")
                ))
                .color(dim)
                .small(),
            );
        }
        let mut service = self.info["service"].as_bool().unwrap_or(false);
        if ui.checkbox(&mut service, "Keep the models available when Fane isn't open (background service)").changed() {
            actions.push(SettingsAction::SetService(service));
        }
        if !self.models_status.is_empty() {
            ui.label(&self.models_status);
        }
    }

    fn apps_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<SettingsAction>, dim: Color32) {
        ui.heading("Apps");
        ui.label("Apps Fane can work in through MCP. Set up installs the app if it's missing, adds its MCP add-on and connects Fane; Fane then opens the app when it needs it.");
        egui::Grid::new("apps").num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
            for a in &self.apps {
                ui.vertical(|ui| {
                    ui.label(RichText::new(&a.name).strong());
                    ui.label(RichText::new(&a.what).color(dim).small());
                });
                let state = match (a.configured, a.running, a.installed) {
                    (true, true, _) => "✓ connected",
                    (true, false, _) => "set up (not connected)",
                    (false, _, true) => "installed",
                    (false, _, false) => "not installed",
                };
                ui.label(state);
                if a.configured {
                    if ui.button("Remove").on_hover_text("Fane stops using it; the app stays installed").clicked() {
                        actions.push(SettingsAction::RemoveApp(a.id.clone()));
                    }
                } else if ui.button("Set up").clicked() {
                    actions.push(SettingsAction::SetupApp(a.id.clone()));
                }
                ui.end_row();
            }
        });
    }

    fn servers_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<SettingsAction>, dim: Color32) {
        ui.heading("Other computers");
        ui.label("Send work to another computer running Fane (it shares its GPU), a stable-diffusion.cpp server, or use an Ollama server for chat.");
        for (name, r) in self.cfg.remotes.clone() {
            ui.horizontal(|ui| {
                if ui.small_button("Remove").clicked() {
                    actions.push(SettingsAction::RemoveRemote(name.clone()));
                }
                ui.label(RichText::new(&name).strong());
                ui.label(RichText::new(r.url.trim_start_matches("http://")).color(dim));
            });
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_height(90.0);
            ui.set_width(ui.available_width());
            if self.servers.is_empty() {
                ui.label(RichText::new(if self.scanning { "Scanning…" } else { "Nothing found yet." }).color(dim));
            }
            for (i, s) in self.servers.iter().enumerate() {
                let where_ = s.url.trim_start_matches("http://");
                let added = self.cfg.remotes.values().any(|r| s.matches(&r.url))
                    || (!self.cfg.model.host.is_empty() && s.matches(&self.cfg.model.host));
                let title =
                    if s.label() == s.address() { where_.to_string() } else { format!("{}  —  {where_}", s.label()) };
                let detail = match s.kind.as_str() {
                    "fane" if s.locked => "Fane · needs its sharing token".to_string(),
                    "fane" => format!("Fane · {} models", s.models.len()),
                    "sd" => "stable-diffusion.cpp · images and video".to_string(),
                    "ollama" => format!(
                        "Ollama {} · {}",
                        s.version,
                        choose_model(&self.cfg.model, &s.models)
                            .map(|m| format!("has {m}"))
                            .unwrap_or_else(|| "none of Fane's models".into())
                    ),
                    _ => format!("model server · {} models", s.models.len()),
                };
                let mut t = RichText::new(format!("{title}{}\n{detail}", if added { "   (in use)" } else { "" }));
                if added {
                    t = t.strong();
                }
                let r = ui.selectable_label(self.selected == Some(i), t);
                if r.clicked() {
                    self.selected = Some(i);
                    self.address = s.url.clone();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(&self.server_status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!self.scanning, egui::Button::new("Scan again")).clicked() {
                    self.scanning = true;
                    self.server_status = "Looking for other computers and servers on your network…".into();
                    actions.push(SettingsAction::Scan);
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Address:");
            ui.add(
                egui::TextEdit::singleline(&mut self.address)
                    .hint_text("nuc.local or 192.168.1.20:11500")
                    .desired_width(220.0),
            );
            ui.label("Token:");
            ui.add(egui::TextEdit::singleline(&mut self.token).hint_text("for another Fane").desired_width(170.0));
            if ui.add_enabled(!self.using && !self.address.trim().is_empty(), egui::Button::new("Add")).clicked() {
                self.using = true;
                self.server_status = format!("Connecting to {}…", self.address.trim());
                actions.push(SettingsAction::UseServer(self.address.trim().to_string(), self.token.trim().to_string()));
            }
        });
        ui.label(
            RichText::new("Traffic to another computer isn't encrypted, so only use servers on a network you trust.")
                .color(dim)
                .small(),
        );
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
