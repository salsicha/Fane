//! The non-UI half of Fane. Runs on a tokio runtime in background threads and talks to the
//! UI thread through [`UiEvent`]s.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

use crate::agent::{Agent, AgentResult, ToolModes};
use crate::config::{Config, set_value};
use crate::connectors::builtin::{Handlers, builtin_tools};
use crate::connectors::delegate::{Delegate, Vars};
use crate::connectors::mcp::McpManager;
use crate::connectors::notes::Notes;
use crate::connectors::thunderbird::ThunderbirdMail;
use crate::discovery::{self, OllamaServer};
use crate::llm::{Ollama, choose_model};
use crate::mail::accounts::{GoaAccount, OnlineAccounts};
use crate::mail::mailbox::{AgentMailbox, MailHost, mail_tool_names};
use crate::platform::{self, Desktop};
use crate::snapshot::{Snapshot, WindowInfo, is_private};
use crate::suggester::{Suggester, Suggestion};
use crate::tools::{Args, ToolRegistry, handler};

/// Titles of Fane's own windows; never treated as the user's context.
pub const OWN_TITLES: [&str; 3] = ["Fane", "Fane Suggestion", "Fane Settings"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowKind {
    Palette,
    Overlay,
    Settings,
}

/// Everything the core (and the IPC/tray/hotkey plumbing) asks of the UI thread.
#[derive(Debug)]
pub enum UiEvent {
    Suggestion(Suggestion),
    JobStatus(u64, String),
    JobDone {
        job: u64,
        result: Option<AgentResult>,
        error: Option<String>,
    },
    Notify(String),
    /// Show a Send/Cancel card; answer on the channel (dropping it means "no").
    ConfirmSend(Value, oneshot::Sender<bool>),
    /// Hide the palette so focus returns to the user's app, then acknowledge.
    HideForInsert(oneshot::Sender<()>),
    Toggle,
    Ask(String),
    OpenSettings,
    Paused(bool),
    Quit,
    PaletteReady {
        snap: Box<Snapshot>,
        prefill: String,
        submit: bool,
        answer: Option<String>,
    },
    Servers(Result<Vec<OllamaServer>, String>),
    OllamaUsed(Result<(String, String), String>),
    MailAccounts(Result<Vec<GoaAccount>, String>),
    MailSaved(Result<String, String>),
    /// egui asked a window to repaint after this delay.
    Redraw(WindowKind, Duration),
    TrayMenu(String),
    /// Offer first-run setup (installing Ollama / downloading the model); the text says what.
    SetupOffer(String),
    /// Show a progress card for a background task that isn't an agent job.
    Working {
        job: u64,
        title: String,
    },
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Hotkey,
}

pub trait UiSink: Send + Sync {
    fn send(&self, ev: UiEvent);
}

pub struct Core {
    pub cfg: RwLock<Config>,
    pub llm: Arc<Ollama>,
    pub suggest_llm: Arc<Ollama>,
    pub tools: RwLock<ToolRegistry>,
    modes: ToolModes,
    pub desktop: Desktop,
    pub suggester: tokio::sync::Mutex<Suggester>,
    pub paused: AtomicBool,
    /// Set by the UI while the palette is open or a card wants the user's attention.
    pub busy: AtomicBool,
    agent_running: AtomicBool,
    pub ready: AtomicBool,
    pub last_snapshot: Mutex<Option<Snapshot>>,
    job_seq: AtomicU64,
    wake: Notify,
    mcp: Arc<McpManager>,
    delegate: Arc<Delegate>,
    pub accounts: Arc<OnlineAccounts>,
    mailbox: tokio::sync::Mutex<Option<Arc<AgentMailbox>>>,
    mail_error: Mutex<Option<String>>,
    ui: OnceLock<Arc<dyn UiSink>>,
    clipboard: Arc<Mutex<Option<arboard::Clipboard>>>,
    /// Lets UI-thread callers start work on the runtime.
    rt: tokio::runtime::Handle,
    /// Run setup without asking (`fane run --setup`, used by the Windows installer).
    pub auto_setup: AtomicBool,
    setup_running: AtomicBool,
    me: Weak<Core>,
}

fn paste_keys() -> &'static str {
    if cfg!(target_os = "macos") { "Cmd+V" } else { "Ctrl+V" }
}

impl Core {
    pub async fn new(cfg: Config) -> Arc<Core> {
        let llm = Arc::new(Ollama::new(&cfg.model));
        let suggest_llm = if cfg.model.suggest_name.is_empty() {
            llm.clone()
        } else {
            let mut m = cfg.model.clone();
            m.name = cfg.model.suggest_name.clone();
            Arc::new(Ollama::new(&m))
        };
        let desktop = Desktop::connect().await;
        Arc::new_cyclic(|me| Core {
            suggester: tokio::sync::Mutex::new(Suggester::new(
                suggest_llm.clone(),
                cfg.suggestions.clone(),
                cfg.model.max_image_side,
            )),
            paused: AtomicBool::new(!cfg.suggestions.enabled),
            mcp: Arc::new(McpManager::new(&cfg.mcp.servers)),
            delegate: Arc::new(Delegate::new(cfg.delegate.clone())),
            cfg: RwLock::new(cfg),
            llm,
            suggest_llm,
            tools: RwLock::new(ToolRegistry::new()),
            modes: Default::default(),
            desktop,
            busy: AtomicBool::new(false),
            agent_running: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            last_snapshot: Mutex::new(None),
            job_seq: AtomicU64::new(0),
            wake: Notify::new(),
            accounts: Arc::new(OnlineAccounts::new()),
            mailbox: tokio::sync::Mutex::new(None),
            mail_error: Mutex::new(None),
            ui: OnceLock::new(),
            clipboard: Arc::new(Mutex::new(None)),
            rt: tokio::runtime::Handle::current(),
            auto_setup: AtomicBool::new(false),
            setup_running: AtomicBool::new(false),
            me: me.clone(),
        })
    }

    fn arc(&self) -> Arc<Core> {
        self.me.upgrade().expect("core is alive while in use")
    }

    pub fn cfg(&self) -> Config {
        self.cfg.read().unwrap().clone()
    }

    pub fn ui(&self, ev: UiEvent) {
        if let Some(ui) = self.ui.get() {
            ui.send(ev);
        }
    }

    pub fn notify(&self, text: impl Into<String>) {
        self.ui(UiEvent::Notify(text.into()));
    }

    // --- lifecycle -----------------------------------------------------------------
    pub async fn start(self: &Arc<Self>, ui: Arc<dyn UiSink>) {
        let _ = self.ui.set(ui);
        let me = self.clone();
        self.rt.spawn(async move {
            if let Err(e) = crate::ipc::serve(move |req| {
                let me = me.clone();
                async move { me.handle_ipc(req).await }
            })
            .await
            {
                tracing::error!("cannot listen for `fane` commands: {e}");
            }
        });
        // Ollama or the model missing on this computer: offer to set them up instead of
        // just reporting the error.
        let setup = crate::setup::plan(&self.cfg().model).await.ok().filter(|p| !p.is_empty());
        match &setup {
            Some(_) if self.auto_setup.load(Ordering::SeqCst) => self.run_setup(),
            Some(plan) if !setup_declined() => self.ui(UiEvent::SetupOffer(plan.describe())),
            _ => {}
        }
        self.resolve_models(setup.is_none()).await;
        self.register_builtin_tools();
        let mut reg = self.tools.read().unwrap().clone();
        self.mcp.start(&mut reg).await;
        *self.tools.write().unwrap() = reg;
        for (name, e) in self.mcp.errors() {
            self.notify(format!("MCP server {name} failed: {e}"));
        }
        self.setup_mail().await;
        self.ready.store(true, Ordering::SeqCst);
        let me = self.clone();
        self.rt.spawn(async move { me.suggest_loop().await });
    }

    async fn resolve_models(&self, report_errors: bool) {
        let llms = if Arc::ptr_eq(&self.llm, &self.suggest_llm) {
            vec![self.llm.clone()]
        } else {
            vec![self.llm.clone(), self.suggest_llm.clone()]
        };
        for llm in llms {
            match llm.resolve_model().await {
                Ok(m) => tracing::info!("using model {m} on {}", llm.host()),
                Err(e) => {
                    tracing::error!("{e}");
                    if report_errors {
                        self.notify(e.to_string());
                    }
                }
            }
        }
    }

    // --- first-run setup -------------------------------------------------------------
    /// Install/start Ollama and download the model in the background, with a progress card.
    pub fn run_setup(self: &Arc<Self>) {
        if self.setup_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = std::fs::remove_file(setup_declined_path());
        let job = self.job_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.ui(UiEvent::Working { job, title: "Setting up Fane".into() });
        let me = self.clone();
        self.rt.spawn(async move {
            let cfg = me.cfg().model;
            let status = |s: &str| me.ui(UiEvent::JobStatus(job, s.to_string()));
            let outcome = match crate::setup::plan(&cfg).await {
                Ok(plan) if plan.is_empty() => Ok(()),
                Ok(plan) => crate::setup::run(&plan, true, &status).await,
                Err(e) => Err(e),
            };
            me.resolve_models(false).await;
            let ev = match outcome {
                Ok(()) => UiEvent::JobDone {
                    job,
                    result: Some(AgentResult {
                        text: format!("Fane is ready, using {} on Ollama.", me.llm.model()),
                        ..Default::default()
                    }),
                    error: None,
                },
                Err(e) => UiEvent::JobDone { job, result: None, error: Some(format!("Setup didn't finish: {e}")) },
            };
            me.setup_running.store(false, Ordering::SeqCst);
            me.ui(ev);
        });
    }

    /// "Not now" on the setup card: don't offer again at startup (Settings can still run it).
    pub fn decline_setup(&self) {
        let _ = std::fs::create_dir_all(&*crate::config::STATE_DIR);
        let _ = std::fs::write(setup_declined_path(), b"");
    }

    pub async fn shutdown(&self) {
        if let Some(mb) = self.mailbox.lock().await.take() {
            mb.stop().await;
        }
        self.mcp.stop().await;
    }

    fn register_builtin_tools(&self) {
        let c = self.cfg();
        let notes = Arc::new(Notes::new(&c.connectors.notes_dirs));
        let mail = Arc::new(ThunderbirdMail::new(&c.connectors.thunderbird_profiles));
        let me = self.me.clone();
        let core = move || me.upgrade().ok_or_else(|| anyhow::anyhow!("Fane is shutting down"));
        let (c1, c2, c3, c4) = (core.clone(), core.clone(), core.clone(), core.clone());
        let n1 = notes.clone();
        let delegate = self.delegate.clone();
        let llm = Arc::downgrade(&self.llm);
        let h = Handlers {
            insert_text: handler(move |a: Args| {
                let core = c1();
                async move { Ok(json!(core?.insert(Clip::Text(a.require("text")?)).await)) }
            }),
            insert_image: handler(move |a: Args| {
                let core = c2();
                async move { Ok(json!(core?.insert(Clip::Image(a.require("path")?)).await)) }
            }),
            read_clipboard: handler(move |_| {
                let core = c3();
                async move { Ok(json!(core?.read_clipboard().await)) }
            }),
            list_windows: handler(move |_| {
                let core = c4();
                async move { Ok(serde_json::to_value(core?.desktop.list_windows().await)?) }
            }),
            search_notes: handler(move |a: Args| {
                let notes = n1.clone();
                async move { notes.search(&a.str("query"), a.int("limit", 8).clamp(1, 50) as usize).await }
            }),
            read_note: handler(move |a: Args| {
                let notes = notes.clone();
                async move { Ok(json!(notes.read(&a.require("path")?).await?)) }
            }),
            recent_emails: handler(move |a: Args| {
                let mail = mail.clone();
                async move {
                    mail.recent(a.int("limit", 10).clamp(1, 100) as usize, &a.str("query"), a.bool("include_body"))
                        .await
                }
            }),
            delegate: handler(move |a: Args| {
                let delegate = delegate.clone();
                let vars = llm.upgrade().map(|l| Vars { model: l.model(), ollama_host: l.host() }).unwrap_or_default();
                async move {
                    let agent = a.str("agent");
                    Ok(json!(delegate.run(&agent, &a.require("task")?, &a.str("cwd"), &vars).await))
                }
            }),
        };
        let available = self.delegate.available();
        let mut reg = self.tools.write().unwrap();
        for tool in builtin_tools(h, &available) {
            reg.add(tool);
        }
    }

    // --- clipboard and pasting -------------------------------------------------------
    async fn with_clipboard<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error> + Send + 'static,
    ) -> anyhow::Result<T> {
        let cb = self.clipboard.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = cb.lock().unwrap();
            if guard.is_none() {
                *guard = Some(arboard::Clipboard::new()?);
            }
            Ok(f(guard.as_mut().unwrap())?)
        })
        .await?
    }

    pub async fn read_clipboard(&self) -> String {
        match self.with_clipboard(|c| c.get_text()).await {
            Ok(t) if !t.is_empty() => t,
            Ok(_) => "(clipboard is empty)".into(),
            Err(e) => format!("(clipboard unavailable: {e})"),
        }
    }

    pub async fn copy_text(&self, text: String) {
        if let Err(e) = self.with_clipboard(move |c| c.set_text(text)).await {
            self.notify(format!("Couldn't copy: {e}"));
        }
    }

    /// Put content on the clipboard and press the paste shortcut in the user's app.
    pub async fn insert(&self, clip: Clip) -> String {
        let (tx, rx) = oneshot::channel();
        self.ui(UiEvent::HideForInsert(tx));
        let _ = tokio::time::timeout(Duration::from_secs(1), rx).await;
        let cfg = self.cfg().insert;
        let terminal = self.last_snapshot.lock().unwrap().as_ref().is_some_and(|s| s.window.is_terminal());

        let restore = cfg.restore_clipboard && matches!(clip, Clip::Text(_));
        let previous = if restore { self.with_clipboard(|c| c.get_text()).await.ok() } else { None };
        let set = match clip {
            Clip::Text(text) => self.with_clipboard(move |c| c.set_text(text)).await,
            Clip::Image(path) => {
                let img = match image::open(&path) {
                    Ok(i) => i.to_rgba8(),
                    Err(e) => return format!("error: could not load image {path}: {e}"),
                };
                let (w, h) = img.dimensions();
                let data = arboard::ImageData { width: w as usize, height: h as usize, bytes: img.into_raw().into() };
                self.with_clipboard(move |c| c.set_image(data)).await
            }
        };
        if let Err(e) = set {
            return format!("error: clipboard unavailable: {e}");
        }
        tokio::time::sleep(Duration::from_millis(cfg.paste_delay_ms)).await;
        let method = tokio::task::spawn_blocking(move || platform::send_paste(terminal)).await.ok().flatten();
        match method {
            None => {
                self.notify(format!("Copied to clipboard — press {} to paste.", paste_keys()));
                "Could not type into the app, so the content was copied to the clipboard and the user \
                 was asked to paste it."
                    .into()
            }
            Some(m) => {
                if let Some(prev) = previous {
                    let me = self.arc();
                    self.rt.spawn(async move {
                        tokio::time::sleep(Duration::from_millis(600)).await;
                        let _ = me.with_clipboard(move |c| c.set_text(prev)).await;
                    });
                }
                format!("inserted ({m})")
            }
        }
    }

    // --- context ---------------------------------------------------------------------
    pub async fn snapshot(&self, with_screen: bool) -> Snapshot {
        let window = self.desktop.focused_window().await;
        let mut snap = Snapshot::new(window);
        if is_private(&snap.window, &self.cfg().privacy) {
            snap.private = true;
        } else if with_screen {
            snap.image = self.desktop.screenshot().await.map(Arc::new);
        }
        snap
    }

    /// Capture context right before the palette covers the screen.
    pub async fn prepare_palette(&self) -> Snapshot {
        let snap = match tokio::time::timeout(Duration::from_millis(1500), self.snapshot(true)).await {
            Ok(s) => s,
            Err(_) => {
                self.last_snapshot.lock().unwrap().clone().unwrap_or_else(|| Snapshot::new(WindowInfo::default()))
            }
        };
        *self.last_snapshot.lock().unwrap() = Some(snap.clone());
        snap
    }

    /// Open the palette (from the hotkey, tray or IPC) once context is captured.
    pub fn open_palette(self: &Arc<Self>, prefill: String, submit: bool, answer: Option<String>) {
        self.busy.store(true, Ordering::SeqCst); // don't pop suggestions while the user is typing
        let me = self.clone();
        self.rt.spawn(async move {
            let snap = me.prepare_palette().await;
            me.ui(UiEvent::PaletteReady { snap: Box::new(snap), prefill, submit, answer });
        });
    }

    // --- proactive loop --------------------------------------------------------------
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub fn suggestion_feedback(self: &Arc<Self>, event: Feedback) {
        let me = self.clone();
        self.rt.spawn(async move {
            let mut s = me.suggester.lock().await;
            match event {
                Feedback::Accepted => s.accepted(),
                Feedback::Dismissed => s.dismissed(),
                Feedback::Ignored => s.ignored(),
            }
        });
    }

    async fn suggest_loop(self: Arc<Self>) {
        loop {
            let interval = self.cfg().suggestions.interval_seconds.max(5);
            let forced = tokio::select! {
                _ = self.wake.notified() => true,
                _ = tokio::time::sleep(Duration::from_secs(interval)) => false,
            };
            let paused = self.paused.load(Ordering::SeqCst);
            if (paused && !forced) || self.agent_running.load(Ordering::SeqCst) || self.busy.load(Ordering::SeqCst) {
                continue;
            }
            if let Err(e) = self.suggest_once(forced).await {
                tracing::warn!("suggestion failed: {e}");
            }
        }
    }

    async fn suggest_once(&self, forced: bool) -> Result<(), crate::llm::LlmError> {
        let snap = self.snapshot(true).await;
        if snap.private || OWN_TITLES.contains(&snap.window.title.as_str()) {
            return Ok(());
        }
        *self.last_snapshot.lock().unwrap() = Some(snap.clone());
        let sug = self.suggester.lock().await.step(&snap, forced).await?;
        match sug {
            Some(s) if !self.busy.load(Ordering::SeqCst) => self.ui(UiEvent::Suggestion(s)),
            None if forced => self.notify("Nothing to suggest right now."),
            _ => {}
        }
        Ok(())
    }

    // --- requests from the UI ----------------------------------------------------------
    fn agent(&self, tools: ToolRegistry) -> Agent {
        Agent::new(self.llm.clone(), tools, self.cfg().model, self.modes.clone())
    }

    pub fn ask(self: &Arc<Self>, prompt: String, snap: Option<Snapshot>) -> u64 {
        let job = self.job_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let me = self.clone();
        self.rt.spawn(async move {
            me.agent_running.store(true, Ordering::SeqCst);
            let snap = match snap.or_else(|| me.last_snapshot.lock().unwrap().clone()) {
                Some(s) => s,
                None => me.snapshot(true).await,
            };
            let history = me.suggester.lock().await.history.clone();
            let agent = me.agent(me.tools.read().unwrap().clone());
            let ui = me.clone();
            let status = move |s: &str| ui.ui(UiEvent::JobStatus(job, s.to_string()));
            let ev = match agent.run(&prompt, &snap, &history, &status).await {
                Ok(r) => UiEvent::JobDone { job, result: Some(r), error: None },
                Err(e) => UiEvent::JobDone { job, result: None, error: Some(e.to_string()) },
            };
            me.agent_running.store(false, Ordering::SeqCst);
            me.ui(ev);
        });
        job
    }

    /// An emailed request from the owner: reduced tools, screen only if allowed.
    pub async fn run_remote(&self, prompt: &str) -> String {
        let cfg = self.cfg().agent_mail;
        let tools = self.tools.read().unwrap().without(&cfg.remote_exclude_tools);
        let snap = if cfg.remote_can_see_screen {
            self.snapshot(true).await
        } else {
            Snapshot::new(WindowInfo::new("(the user is away; screen not shared)", ""))
        };
        let history = self.suggester.lock().await.history.clone();
        self.agent_running.store(true, Ordering::SeqCst);
        let result = self.agent(tools).run(prompt, &snap, &history, &|_| {}).await;
        self.agent_running.store(false, Ordering::SeqCst);
        match result {
            Ok(r) => r.text,
            Err(e) => format!("Sorry, I couldn't do that: {e}"),
        }
    }

    // --- Fane's own email ----------------------------------------------------------------
    /// (Re)connect to the one Online Accounts mailbox the user selected, if any.
    pub async fn setup_mail(&self) {
        let mut slot = self.mailbox.lock().await;
        if let Some(mb) = slot.take() {
            mb.stop().await;
        }
        {
            let mut reg = self.tools.write().unwrap();
            for name in mail_tool_names() {
                reg.remove(name);
            }
        }
        *self.mail_error.lock().unwrap() = None;
        let cfg = self.cfg().agent_mail;
        if cfg.account.trim().is_empty() {
            return;
        }
        let host: Arc<dyn MailHost> = Arc::new(MailBridge(self.me.clone()));
        match AgentMailbox::connect(self.accounts.clone(), cfg, host).await {
            Ok(mb) => {
                let mut reg = self.tools.write().unwrap();
                for tool in mb.tools() {
                    reg.add(tool);
                }
                *slot = Some(mb);
            }
            Err(e) => {
                tracing::warn!("agent email disabled: {e:#}");
                *self.mail_error.lock().unwrap() = Some(format!("{e:#}"));
                self.notify(format!("Fane's email is off: {e:#}"));
            }
        }
    }

    pub async fn select_mail_account(&self, address: &str) -> Result<String, String> {
        set_value("agent_mail", "account", address.trim()).map_err(|e| e.to_string())?;
        self.cfg.write().unwrap().agent_mail.account = address.trim().to_string();
        self.setup_mail().await;
        Ok(if address.trim().is_empty() {
            "Fane's email is off".into()
        } else if let Some(e) = self.mail_error.lock().unwrap().clone() {
            return Err(e);
        } else {
            format!("Fane now uses {} for email", address.trim())
        })
    }

    pub async fn set_owner_email(&self, email: &str) -> Result<String, String> {
        let email = email.trim().to_lowercase();
        if !email.is_empty() && !valid_email(&email) {
            return Err(format!("{email} doesn't look like an email address"));
        }
        set_value("agent_mail", "owner_email", email.as_str()).map_err(|e| e.to_string())?;
        self.cfg.write().unwrap().agent_mail.owner_email = email.clone();
        self.setup_mail().await;
        Ok(if email.is_empty() {
            "Fane won't act on any email until you set your address".into()
        } else {
            format!("Fane will only read and answer mail from {email}")
        })
    }

    /// Re-read [agent_mail] from disk (after `fane mail ...`) and reconnect.
    pub async fn reload_mail_config(&self) {
        if let Ok(fresh) = Config::load() {
            self.cfg.write().unwrap().agent_mail = fresh.agent_mail;
        }
        self.setup_mail().await;
    }

    // --- which Ollama server ---------------------------------------------------------------
    pub async fn discover_ollama(&self) -> Vec<OllamaServer> {
        discovery::discover(&[self.llm.host()]).await
    }

    /// Switch to the Ollama server at `url` and save it. Returns (host, model).
    /// Nothing changes unless the server answers and has one of Fane's models installed.
    pub async fn use_ollama(&self, url: &str) -> Result<(String, String), String> {
        let url = discovery::normalize_host(url).map_err(|e| e.0)?;
        let server = discovery::probe(&url).await.map_err(|e| e.0)?;
        let mcfg = self.cfg().model;
        if choose_model(&mcfg, &server.models).is_none() {
            return Err(format!(
                "{url} has none of Fane's models. Pull one there first, e.g. ollama pull {}",
                mcfg.name
            ));
        }
        set_value("model", "host", url.as_str()).map_err(|e| e.to_string())?;
        self.cfg.write().unwrap().model.host = url.clone();
        let mut llms = vec![self.llm.clone()];
        if !Arc::ptr_eq(&self.llm, &self.suggest_llm) {
            llms.push(self.suggest_llm.clone());
        }
        for llm in &llms {
            llm.set_host(&url);
            llm.resolve_model().await.map_err(|e| e.to_string())?;
        }
        let model = self.llm.model();
        tracing::info!("switched to Ollama at {url} (model {model})");
        self.notify(format!("Fane now uses Ollama at {url} ({model})"));
        Ok((url, model))
    }

    pub async fn status(&self) -> Value {
        let mb = self.mailbox.lock().await.clone();
        let cfg = self.cfg();
        json!({
            "ollama_host": self.llm.host(),
            "model": self.llm.model(),
            "suggest_model": self.suggest_llm.model(),
            "mail_account": mb.as_ref().map(|m| m.address.clone()),
            "owner_email": cfg.agent_mail.owner(),
            "mail_error": self.mail_error.lock().unwrap().clone().or_else(|| mb.and_then(|m| m.error())),
            "helper": self.desktop.helper_available(),
            "screen_backend": self.desktop.screen_backend(),
            "paused": self.paused.load(Ordering::SeqCst),
            "tools": self.tools.read().unwrap().names(),
            "ready": self.ready.load(Ordering::SeqCst),
        })
    }

    async fn handle_ipc(self: Arc<Self>, req: Value) -> Value {
        let cmd = req["cmd"].as_str().unwrap_or("").to_string();
        let arg = |k: &str| req[k].as_str().unwrap_or("").to_string();
        match cmd.as_str() {
            "ping" => {}
            "toggle" => self.ui(UiEvent::Toggle),
            "ask" => self.ui(UiEvent::Ask(arg("text"))),
            "settings" => self.ui(UiEvent::OpenSettings),
            "suggest" => self.wake(),
            "pause" | "resume" => {
                self.set_paused(cmd == "pause");
                self.ui(UiEvent::Paused(cmd == "pause"));
            }
            "status" => {
                let mut s = self.status().await;
                s["ok"] = json!(true);
                return s;
            }
            "mail_use" => {
                let me = self.clone();
                let address = arg("address");
                self.rt.spawn(async move {
                    match me.select_mail_account(&address).await {
                        Ok(m) | Err(m) => me.notify(m),
                    }
                });
            }
            "mail_reload" => {
                let me = self.clone();
                self.rt.spawn(async move { me.reload_mail_config().await });
            }
            "ollama_use" => {
                let me = self.clone();
                let host = arg("host");
                self.rt.spawn(async move {
                    if let Err(e) = me.use_ollama(&host).await {
                        me.notify(e);
                    }
                });
            }
            "quit" => self.ui(UiEvent::Quit),
            other => return json!({"ok": false, "error": format!("unknown command {other:?}")}),
        }
        json!({"ok": true})
    }
}

fn setup_declined_path() -> std::path::PathBuf {
    crate::config::STATE_DIR.join("setup-declined")
}

fn setup_declined() -> bool {
    setup_declined_path().exists()
}

pub fn valid_email(s: &str) -> bool {
    let Some((user, domain)) = s.split_once('@') else { return false };
    !user.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.contains(char::is_whitespace)
}

pub enum Clip {
    Text(String),
    Image(String),
}

#[derive(Debug, Clone, Copy)]
pub enum Feedback {
    Accepted,
    Dismissed,
    Ignored,
}

/// The mailbox's view of the core.
struct MailBridge(Weak<Core>);

#[async_trait::async_trait]
impl MailHost for MailBridge {
    async fn run_remote(&self, prompt: &str) -> String {
        match self.0.upgrade() {
            Some(core) => core.run_remote(prompt).await,
            None => "Fane is shutting down.".into(),
        }
    }

    fn notify(&self, text: &str) {
        if let Some(core) = self.0.upgrade() {
            core.notify(text);
        }
    }

    async fn confirm_send(&self, details: Value) -> bool {
        let Some(core) = self.0.upgrade() else { return false };
        let (tx, rx) = oneshot::channel();
        core.ui(UiEvent::ConfirmSend(details, tx));
        // The card times out after 3 minutes; unanswered = not sent.
        matches!(tokio::time::timeout(Duration::from_secs(200), rx).await, Ok(Ok(true)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_validation() {
        assert!(valid_email("me@example.com"));
        assert!(!valid_email("me@example"));
        assert!(!valid_email("me example@x.com"));
        assert!(!valid_email("@x.com"));
    }
}
