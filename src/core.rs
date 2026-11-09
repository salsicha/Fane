//! The non-UI half of Fane. Runs on a tokio runtime in background threads and talks to the
//! UI thread through [`UiEvent`]s.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

use crate::agent::{Agent, AgentResult, ToolModes};
use crate::config::{Config, RemoteCfg, Role, set_value};
use crate::connectors::builtin::{Handlers, builtin_tools};
use crate::connectors::delegate::{Delegate, Vars};
use crate::connectors::mcp::McpManager;
use crate::connectors::media::Media;
use crate::connectors::notes::Notes;
use crate::connectors::thunderbird::ThunderbirdMail;
use crate::discovery::{self, Server};
use crate::llm::{Llm, choose_model};
use crate::mail::accounts::{GoaAccount, OnlineAccounts};
use crate::mail::mailbox::{AgentMailbox, MailHost, mail_tool_names};
use crate::platform::{self, Desktop};
use crate::runtime::Runtime;
use crate::setup::Want;
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
    Servers(Result<Vec<Server>, String>),
    ServerUsed(Result<(String, String), String>),
    MailAccounts(Result<Vec<GoaAccount>, String>),
    MailSaved(Result<String, String>),
    /// egui asked a window to repaint after this delay.
    Redraw(WindowKind, Duration),
    TrayMenu(String),
    /// Offer setup (getting llama.cpp / downloading models); the text says what.
    SetupOffer {
        title: String,
        text: String,
    },
    /// Settings' Models page has news (an install finished, a GPU or route changed).
    ModelsChanged,
    /// The model server's status, for Settings.
    ModelsInfo(Value),
    /// The outcome of a Settings → Models action.
    ModelsStatus(Result<String, String>),
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
    pub llm: Arc<Llm>,
    pub suggest_llm: Arc<Llm>,
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
    /// What the installer asked for beyond the everyday model.
    pub auto_want: Mutex<Want>,
    setup_running: AtomicBool,
    /// The job whose agent is running, for tools that report progress (delegation).
    current_job: Mutex<Option<u64>>,
    /// Where things are: files and capabilities.
    pub index: Arc<crate::index::Index>,
    /// The user's standing preferences.
    pub prefs: Arc<crate::prefs::Prefs>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    /// Fane's model server, when this process runs it (not when the background service does).
    pub runtime: RwLock<Option<Arc<Runtime>>>,
    server_stop: Mutex<Option<oneshot::Sender<()>>>,
    me: Weak<Core>,
}

fn paste_keys() -> &'static str {
    if cfg!(target_os = "macos") { "Cmd+V" } else { "Ctrl+V" }
}

impl Core {
    pub async fn new(cfg: Config) -> Arc<Core> {
        let host = cfg.llm_host();
        let llm = Arc::new(Llm::new(&cfg.model, &host));
        let suggest_llm = if cfg.model.suggest_name.is_empty() {
            llm.clone()
        } else {
            let mut m = cfg.model.clone();
            m.name = cfg.model.suggest_name.clone();
            Arc::new(Llm::new(&m, &host))
        };
        let desktop = Desktop::connect().await;
        let db = crate::config::STATE_DIR.join("fane.db");
        let store = Arc::new(crate::store::Store::open(&db).unwrap_or_else(|e| {
            tracing::error!("{e:#}; keeping the index and preferences in memory this session");
            crate::store::Store::in_memory().expect("in-memory database")
        }));
        let index = Arc::new(crate::index::Index::new(store.clone(), crate::index::Scope::from_cfg(&cfg.files)));
        let prefs = Arc::new(crate::prefs::Prefs::new(store));
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
            auto_want: Mutex::new(Want::default()),
            setup_running: AtomicBool::new(false),
            current_job: Mutex::new(None),
            index,
            prefs,
            watcher: Mutex::new(None),
            runtime: RwLock::new(None),
            server_stop: Mutex::new(None),
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
        self.start_model_server().await;
        // Something missing on this computer: offer to set it up instead of just reporting the
        // error. An older setup that used Ollama keeps working until the user chooses.
        let cfg = self.cfg();
        let needs_setup = if crate::setup::is_local_ollama(&cfg.model.host) {
            if self.auto_setup.load(Ordering::SeqCst) {
                self.run_setup(*self.auto_want.lock().unwrap());
            } else if !setup_declined() {
                self.ui(UiEvent::SetupOffer {
                    title: "Run models with llama.cpp?".into(),
                    text: format!(
                        "Fane can now run {} itself, using the copy Ollama already downloaded, and share the GPU with \
                         image and video generation. Ollama stays as it is.",
                        cfg.model.name
                    ),
                });
            }
            false
        } else {
            let c = cfg.clone();
            let want =
                if self.auto_setup.load(Ordering::SeqCst) { *self.auto_want.lock().unwrap() } else { Want::default() };
            let plan = tokio::task::spawn_blocking(move || crate::setup::plan(&c, want)).await.unwrap_or_default();
            if plan.is_empty() {
                false
            } else {
                if self.auto_setup.load(Ordering::SeqCst) {
                    self.run_setup(want);
                } else if !setup_declined() {
                    self.ui(UiEvent::SetupOffer {
                        title: "Set up Fane?".into(),
                        text: format!("{}. Fane needs this to work.", plan.describe(&cfg)),
                    });
                }
                true
            }
        };
        self.resolve_models(!needs_setup).await;
        self.update_delegate_order().await;
        self.register_builtin_tools();
        let mut reg = self.tools.read().unwrap().clone();
        self.mcp.start(&mut reg).await;
        *self.tools.write().unwrap() = reg;
        for (name, e) in self.mcp.errors() {
            self.notify(format!("MCP server {name} failed: {e}"));
        }
        self.setup_mail().await;
        self.refresh_capabilities();
        self.ready.store(true, Ordering::SeqCst);
        let me = self.clone();
        self.rt.spawn(async move { me.index_loop().await });
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

    /// Ask the user on a Send/Cancel-style card; unanswered (3 minutes) = no.
    pub async fn confirm(&self, details: Value) -> bool {
        let (tx, rx) = oneshot::channel();
        self.ui(UiEvent::ConfirmSend(details, tx));
        matches!(tokio::time::timeout(Duration::from_secs(200), rx).await, Ok(Ok(true)))
    }

    /// A status line for the running job's card (no-op outside a job, e.g. emailed requests).
    pub fn job_status(&self, text: &str) {
        if let Some(job) = *self.current_job.lock().unwrap() {
            self.ui(UiEvent::JobStatus(job, text.to_string()));
        }
    }

    // --- setup -------------------------------------------------------------------------
    /// Get what's missing (llama.cpp, models, and the extras in `want`) in the background, with a
    /// progress card. An older Ollama setup switches to Fane's own models here.
    pub fn run_setup(self: &Arc<Self>, want: Want) {
        if self.setup_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = std::fs::remove_file(setup_declined_path());
        let job = self.job_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.ui(UiEvent::Working { job, title: "Setting up Fane".into() });
        let me = self.clone();
        self.rt.spawn(async move {
            if crate::setup::is_local_ollama(&me.cfg().model.host) {
                // Chosen by the user on the offer card (or in Settings): stop using Ollama.
                if let Err(e) = set_value("model", "host", "") {
                    tracing::warn!("can't save the config: {e}");
                }
                me.cfg.write().unwrap().model.host.clear();
                let host = me.cfg().llm_host();
                me.llm.set_host(&host);
                me.suggest_llm.set_host(&host);
            }
            let cfg = me.cfg();
            let status = |s: &str| me.ui(UiEvent::JobStatus(job, s.to_string()));
            let c = cfg.clone();
            let plan = tokio::task::spawn_blocking(move || crate::setup::plan(&c, want)).await.unwrap_or_default();
            let outcome = if plan.is_empty() { Ok(()) } else { crate::setup::run(&plan, &cfg, &status).await };
            me.models_changed().await;
            me.resolve_models(false).await;
            let ev = match outcome {
                Ok(()) => UiEvent::JobDone {
                    job,
                    result: Some(AgentResult {
                        text: format!("Fane is ready, using {}.", me.llm.model()),
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
        self.stop_model_server().await;
    }

    // --- Fane's model server -------------------------------------------------------------
    /// Run the model server in this process, unless one (the background service) already
    /// answers on the port.
    async fn start_model_server(self: &Arc<Self>) {
        let cfg = self.cfg();
        let url = cfg.runtime.url();
        if crate::runtime::router::answering(&url).await {
            tracing::info!("using the model server already running at {url}");
            return;
        }
        let rt = self.runtime.read().unwrap().clone().unwrap_or_else(|| Runtime::new(cfg.clone()));
        match crate::runtime::router::bind(&cfg.runtime).await {
            Ok(listener) => {
                let (tx, rx) = oneshot::channel::<()>();
                let rt2 = rt.clone();
                self.rt.spawn(async move {
                    if let Err(e) = crate::runtime::router::run(listener, rt2, async {
                        let _ = rx.await;
                    })
                    .await
                    {
                        tracing::error!("model server stopped: {e}");
                    }
                });
                *self.server_stop.lock().unwrap() = Some(tx);
                *self.runtime.write().unwrap() = Some(rt);
            }
            Err(e) => {
                tracing::error!("{e}");
                self.notify(format!("Fane's model server couldn't start: {e}"));
            }
        }
    }

    /// Stop serving (models keep running unless `everything`).
    async fn stop_serving(&self, everything: bool) {
        if let Some(tx) = self.server_stop.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if everything {
            let rt = self.runtime.write().unwrap().take();
            if let Some(rt) = rt {
                rt.shutdown().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    async fn stop_model_server(&self) {
        self.stop_serving(true).await;
    }

    /// Tell the model server the config changed (in this process, or the background service).
    async fn apply_runtime_cfg(&self) {
        let cfg = self.cfg();
        let rt = self.runtime.read().unwrap().clone();
        match rt {
            Some(rt) => {
                rt.set_cfg(cfg.clone());
                rt.redetect();
            }
            None => {
                let c = crate::llm::http_client(Duration::from_secs(5));
                let _ = c.post(format!("{}/fane/v1/reload", cfg.runtime.url())).send().await;
            }
        }
    }

    /// After installs or config changes: refresh the server, tools, harness order and Settings.
    pub async fn models_changed(self: &Arc<Self>) {
        self.apply_runtime_cfg().await;
        self.update_delegate_order().await;
        self.register_media_tools();
        if self.ready.load(Ordering::SeqCst) {
            self.refresh_capabilities();
        }
        self.ui(UiEvent::ModelsChanged);
    }

    /// The model server's status (`/fane/v1/info`).
    pub async fn models_info(&self) -> Value {
        let rt = self.runtime.read().unwrap().clone();
        if let Some(rt) = rt {
            let mut v = rt.info();
            v["service"] = json!(crate::runtime::service::installed());
            return v;
        }
        let url = self.cfg().runtime.url();
        let c = crate::llm::http_client(Duration::from_secs(5));
        match c.get(format!("{url}/fane/v1/info")).send().await {
            Ok(r) => {
                let mut v: Value = r.json().await.unwrap_or_default();
                v["service"] = json!(crate::runtime::service::installed());
                v
            }
            Err(e) => json!({"error": format!("Fane's model server isn't answering: {e}")}),
        }
    }

    /// Which GPU a kind of work uses ("auto" or a GPU id).
    pub async fn set_role_gpu(self: &Arc<Self>, role: Role, gpu: &str) -> Result<String, String> {
        let key = format!("{}_gpu", role.as_str());
        set_value("runtime", &key, gpu).map_err(|e| e.to_string())?;
        {
            let mut cfg = self.cfg.write().unwrap();
            match role {
                Role::Chat => cfg.runtime.chat_gpu = gpu.into(),
                Role::Code => cfg.runtime.code_gpu = gpu.into(),
                Role::Image => cfg.runtime.image_gpu = gpu.into(),
                Role::Video => cfg.runtime.video_gpu = gpu.into(),
            }
        }
        self.models_changed().await;
        Ok(format!("{} work will run on {gpu} (from its next start)", role.as_str()))
    }

    /// Where a kind of work goes, first choice first.
    pub async fn set_route(self: &Arc<Self>, role: Role, targets: Vec<String>) -> Result<String, String> {
        set_value("routes", role.as_str(), crate::config::str_array(&targets)).map_err(|e| e.to_string())?;
        self.cfg.write().unwrap().routes.set(role, targets.clone());
        self.models_changed().await;
        Ok(format!("{} requests go to: {}", role.as_str(), targets.join(", then ")))
    }

    /// Add another computer running Fane (or a server) to send work to. Returns its name.
    pub async fn add_remote(self: &Arc<Self>, url: &str, token: &str) -> Result<String, String> {
        let url = discovery::normalize_with_port(url, 11500).map_err(|e| e.0)?;
        let found = discovery::identify(&url, token.trim(), "").await?;
        let server = discovery::probe(&url).await.ok();
        let base = server.as_ref().map(|s| s.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| {
            url::Url::parse(&url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or(url.clone())
        });
        let name: String = base
            .split('.')
            .next()
            .unwrap_or(&base)
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
            .collect();
        let name = if name.is_empty() || ["local", "claude", "codex"].contains(&name.as_str()) {
            format!("remote-{name}")
        } else {
            name
        };
        let remote = RemoteCfg { url: url.clone(), token: token.trim().to_string(), kind: found.kind.clone() };
        crate::config::set_remote(&name, Some(&remote)).map_err(|e| e.to_string())?;
        self.cfg.write().unwrap().remotes.insert(name.clone(), remote);
        if let Some(rt) = self.runtime.read().unwrap().clone() {
            rt.forget_remote_cache(&name);
        }
        self.models_changed().await;
        Ok(name)
    }

    pub async fn remove_remote(self: &Arc<Self>, name: &str) -> Result<String, String> {
        crate::config::set_remote(name, None).map_err(|e| e.to_string())?;
        let routes = {
            let mut cfg = self.cfg.write().unwrap();
            cfg.remotes.remove(name);
            cfg.routes.clone()
        };
        for role in Role::ALL {
            let list = routes.for_role(role);
            if list.iter().any(|t| t == name) {
                let kept: Vec<String> = list.iter().filter(|t| *t != name).cloned().collect();
                let _ = self.set_route(role, kept).await;
            }
        }
        self.models_changed().await;
        Ok(format!("Removed {name}"))
    }

    /// Let other computers use this one's models (with a token), or stop. Returns the token.
    pub async fn set_share(self: &Arc<Self>, on: bool) -> Result<String, String> {
        let mut token = self.cfg().runtime.token;
        if on && token.is_empty() {
            use rand::RngExt;
            const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
            let mut rng = rand::rng();
            token = (0..20).map(|_| CHARS[rng.random_range(0..CHARS.len())] as char).collect();
            set_value("runtime", "token", token.as_str()).map_err(|e| e.to_string())?;
        }
        set_value("runtime", "share", on).map_err(|e| e.to_string())?;
        {
            let mut cfg = self.cfg.write().unwrap();
            cfg.runtime.share = on;
            cfg.runtime.token = token.clone();
        }
        // Listen on the network (or stop); models keep running.
        if self.runtime.read().unwrap().is_some() {
            self.stop_serving(false).await;
            self.start_model_server().await;
        }
        self.models_changed().await;
        Ok(token)
    }

    /// Run the model server as a background service (or stop doing so).
    pub async fn set_service(self: &Arc<Self>, on: bool) -> Result<String, String> {
        if on {
            self.stop_model_server().await;
            let note =
                tokio::task::spawn_blocking(crate::runtime::service::install).await.map_err(|e| e.to_string())??;
            let url = self.cfg().runtime.url();
            for _ in 0..40 {
                if crate::runtime::router::answering(&url).await {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            self.models_changed().await;
            Ok(note)
        } else {
            tokio::task::spawn_blocking(crate::runtime::service::uninstall).await.map_err(|e| e.to_string())??;
            tokio::time::sleep(Duration::from_millis(500)).await;
            self.start_model_server().await;
            self.models_changed().await;
            Ok("Fane's model server now runs only while Fane is open.".into())
        }
    }

    /// Harness preference from `[routes] code`: local harnesses where local (or remote) models
    /// can run the coding model, then Claude Code / Codex in the route's order.
    async fn update_delegate_order(&self) {
        let cfg = self.cfg();
        let mut order = vec![];
        for target in &cfg.routes.code {
            match target.as_str() {
                "claude" | "codex" => order.push(target.clone()),
                _ => {
                    for h in ["qwen", "codegem"] {
                        if !order.contains(&h.to_string()) {
                            order.push(h.to_string());
                        }
                    }
                }
            }
        }
        self.delegate.set_order(order);
    }

    /// generate_image / generate_video, when some route can make them.
    fn register_media_tools(&self) {
        let cfg = self.cfg();
        let media = Arc::new(Media::new(&cfg));
        let mut reg = self.tools.write().unwrap();
        for tool in media.tools() {
            let role = if tool.name == "generate_video" { Role::Video } else { Role::Image };
            if crate::connectors::media::route_exists(&cfg, role) {
                reg.add(tool);
            } else {
                reg.remove(&tool.name);
            }
        }
    }

    fn register_builtin_tools(&self) {
        let c = self.cfg();
        let notes = Arc::new(Notes::new(&c.connectors.notes_dirs));
        let mail = Arc::new(ThunderbirdMail::new(&c.connectors.thunderbird_profiles));
        let me = self.me.clone();
        let core = move || me.upgrade().ok_or_else(|| anyhow::anyhow!("Fane is shutting down"));
        let (c1, c2, c3, c4, c5) = (core.clone(), core.clone(), core.clone(), core.clone(), core.clone());
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
                let core = c5();
                let code_model = core.as_ref().map(|c| c.cfg().model.code).unwrap_or_default();
                let vars = llm
                    .upgrade()
                    .map(|l| Vars { model: l.model(), code_model: code_model.clone(), host: l.host() })
                    .unwrap_or_default();
                async move {
                    let agent = a.str("agent");
                    let core = core?;
                    // Planning and review by Claude/Codex show up on the job's progress card.
                    let progress = |s: &str| core.job_status(s);
                    Ok(json!(delegate.run_reviewed(&agent, &a.require("task")?, &a.str("cwd"), &vars, &progress).await))
                }
            }),
        };
        let available = self.delegate.available();
        let mut reg = self.tools.write().unwrap();
        for tool in builtin_tools(h, &available) {
            reg.add(tool);
        }
        drop(reg);
        self.register_file_tools();
        self.register_memory_tools();
        self.register_media_tools();
    }

    /// (Re)build the filesystem tools from `[files]`; changes are confirmed on a card.
    fn register_file_tools(&self) {
        let me = self.me.clone();
        let confirm: crate::connectors::files::Confirm = Arc::new(move |details| {
            let me = me.clone();
            Box::pin(async move {
                match me.upgrade() {
                    Some(core) => core.confirm(details).await,
                    None => false,
                }
            })
        });
        let files =
            Arc::new(crate::connectors::files::Files::new(&self.cfg().files, confirm).with_index(self.index.clone()));
        let mut reg = self.tools.write().unwrap();
        for tool in files.tools() {
            reg.add(tool);
        }
    }

    // --- index and preferences -----------------------------------------------------------
    /// Keep the index current: a full (incremental) scan now and every 30 minutes, a watcher
    /// for live changes, and preference clean-up after each pass.
    async fn index_loop(self: Arc<Self>) {
        loop {
            let index = self.index.clone();
            match tokio::task::spawn_blocking(move || index.scan()).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("indexing failed: {e:#}"),
                Err(e) => tracing::warn!("indexing crashed: {e}"),
            }
            if self.watcher.lock().unwrap().is_none() {
                *self.watcher.lock().unwrap() = crate::index::watch(self.index.clone(), self.rt.clone());
            }
            self.refresh_capabilities();
            tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        }
    }

    /// What Fane can use right now, for the capability index.
    fn capabilities(&self) -> Vec<crate::index::Capability> {
        use crate::index::Capability;
        let cfg = self.cfg();
        let mail = mail_tool_names();
        let mut caps: Vec<Capability> = self
            .tools
            .read()
            .unwrap()
            .iter()
            .map(|t| {
                let source = match t.name.split_once("__") {
                    Some((server, _)) => format!("MCP server {server}"),
                    None if mail.contains(&t.name.as_str()) => format!("Fane's email ({})", cfg.agent_mail.account),
                    None if crate::connectors::files::WRITE_TOOLS.contains(&t.name.as_str())
                        || ["list_files", "find_files", "search_files", "read_file"].contains(&t.name.as_str()) =>
                    {
                        "files".to_string()
                    }
                    None => "built-in".to_string(),
                };
                Capability { kind: "tool".into(), name: t.name.clone(), source, description: t.description.clone() }
            })
            .collect();
        for (name, description) in self.delegate.available() {
            caps.push(Capability {
                kind: "agent".into(),
                name,
                source: "coding agent (use the delegate tool)".into(),
                description,
            });
        }
        caps
    }

    /// Re-sync the capability index with the current tools, then drop preferences that depend on
    /// something no longer configured.
    pub fn refresh_capabilities(&self) {
        if self.index.sync_capabilities(&self.capabilities()) {
            self.invalidate_prefs();
        }
    }

    /// Whether a tool or agent named in a preference still exists. An MCP server that's still
    /// configured but not running right now doesn't count as gone.
    fn still_configured(&self, name: &str) -> bool {
        if self.tools.read().unwrap().contains(name) || self.delegate.available().iter().any(|(n, _)| n == name) {
            return true;
        }
        let cfg = self.cfg();
        if let Some((server, _)) = name.split_once("__") {
            let safe = |s: &str| {
                s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect::<String>()
            };
            return cfg.mcp.servers.keys().any(|k| safe(k) == server);
        }
        mail_tool_names().contains(&name) && !cfg.agent_mail.account.is_empty()
    }

    fn invalidate_prefs(&self) {
        for (pref, reason) in self.prefs.invalidate(&|t| self.still_configured(t)) {
            tracing::info!("forgot preference {} ({reason})", pref.line());
            self.notify(format!("Forgot your preference “{}”: {reason}.", pref.text));
        }
    }

    /// Names a preference might refer to (for detecting what it depends on).
    fn known_names(&self) -> Vec<String> {
        let mut names = self.tools.read().unwrap().names();
        names.extend(self.cfg().delegate.all_agents().into_iter().map(|(n, _)| n));
        names
    }

    fn register_memory_tools(&self) {
        use crate::tools::{Tool, params, prop};
        let mut reg = self.tools.write().unwrap();
        let index = self.index.clone();
        reg.add(Tool::new(
            "find_tools",
            "Find which tool, MCP server or coding agent can do a task (e.g. \"calendar\", \"send a Slack message\").",
            params(&["task"], &[("task", prop("string", "What you want to get done"))]),
            handler(move |a: Args| {
                let index = index.clone();
                async move { Ok(index.find_capabilities_json(&a.require("task")?, 8)) }
            }),
            "Looking for a tool",
        ));
        let me = self.me.clone();
        reg.add(Tool::new(
            "save_preference",
            "Remember how the user wants something done from now on (only their own instructions).",
            params(
                &["about", "preference"],
                &[
                    ("about", prop("string", "Short topic, e.g. \"email replies to Dana\"")),
                    ("preference", prop("string", "The instruction, in the user's words")),
                    ("replaces", json!({"type": "array", "items": {"type": "integer"}, "description": "Ids of preferences this changes"})),
                ],
            ),
            handler(move |a: Args| {
                let me = me.clone();
                async move {
                    let core = me.upgrade().ok_or_else(|| anyhow::anyhow!("Fane is shutting down"))?;
                    let replaces: Vec<i64> = match a.0.get("replaces") {
                        Some(Value::Array(v)) => v.iter().filter_map(|x| x.as_i64().or_else(|| x.as_str()?.parse().ok())).collect(),
                        _ => vec![],
                    };
                    let (pref, old) = core.prefs.save(&a.str("about"), &a.require("preference")?, &replaces, &core.known_names())?;
                    let note = if old.is_empty() { String::new() } else { format!(" (replacing {})", old.iter().map(|p| format!("“{}”", p.text)).collect::<Vec<_>>().join(", ")) };
                    core.notify(format!("Remembered: {}{note}", pref.text));
                    Ok(json!(format!("saved preference [{}]{note}", pref.id)))
                }
            }),
            "Remembering",
        ));
        let me = self.me.clone();
        reg.add(Tool::new(
            "forget_preference",
            "Forget one of the user's preferences by id (when they cancel it).",
            params(&["id"], &[("id", prop("integer", ""))]),
            handler(move |a: Args| {
                let me = me.clone();
                async move {
                    let core = me.upgrade().ok_or_else(|| anyhow::anyhow!("Fane is shutting down"))?;
                    match core.prefs.forget(a.int("id", -1)) {
                        Some(p) => {
                            core.notify(format!("Forgot: {}", p.text));
                            Ok(json!(format!("forgot preference [{}]", p.id)))
                        }
                        None => Ok(json!("error: no preference with that id")),
                    }
                }
            }),
            "Forgetting",
        ));
    }

    pub fn index_status(&self) -> String {
        let n = self.index.file_count();
        let caps = self.index.capabilities().len();
        if self.index.ready() {
            format!("{n} files and {caps} tools/agents indexed")
        } else {
            format!("Indexing… ({n} files so far)")
        }
    }

    pub fn rebuild_index(self: &Arc<Self>) {
        let index = self.index.clone();
        let me = self.clone();
        self.rt.spawn(async move {
            let stats = tokio::task::spawn_blocking(move || index.scan()).await;
            if let Ok(Ok(s)) = stats {
                me.notify(format!(
                    "Index updated: {} files ({} new or changed, {} removed)",
                    s.files, s.added_or_changed, s.removed
                ));
            }
            me.refresh_capabilities();
        });
    }

    /// Save the folders the file tools may use (from Settings) and apply them now.
    pub fn set_file_roots(&self, roots: Vec<String>) -> Result<String, String> {
        let roots: Vec<String> = roots.into_iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect();
        if let Some(bad) = roots.iter().find(|r| !crate::config::expand(r).is_dir()) {
            return Err(format!("{bad} isn't a folder"));
        }
        set_value("files", "roots", crate::config::str_array(&roots)).map_err(|e| e.to_string())?;
        self.cfg.write().unwrap().files.roots = roots.clone();
        self.register_file_tools();
        // Re-index for the new folders; files outside them drop out of the index.
        self.index.set_scope(crate::index::Scope::from_cfg(&self.cfg().files));
        let index = self.index.clone();
        self.rt.spawn_blocking(move || index.scan());
        Ok(if roots.is_empty() {
            "Fane can't use any files now".into()
        } else {
            format!("Fane can use files in {}", roots.join(", "))
        })
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
        let prefs = crate::prefs::prompt_section(&self.prefs.relevant(&snap.window.label(), 10));
        let sug = {
            let mut s = self.suggester.lock().await;
            s.preferences = prefs;
            s.step(&snap, forced).await?
        };
        match sug {
            Some(s) if !self.busy.load(Ordering::SeqCst) => self.ui(UiEvent::Suggestion(s)),
            None if forced => self.notify("Nothing to suggest right now."),
            _ => {}
        }
        Ok(())
    }

    // --- requests from the UI ----------------------------------------------------------
    fn agent(&self, tools: ToolRegistry, prompt: &str) -> Agent {
        let mut agent = Agent::new(self.llm.clone(), tools, self.cfg().model, self.modes.clone());
        agent.preferences = crate::prefs::prompt_section(&self.prefs.relevant(prompt, 15));
        let index = self.index.clone();
        agent.selector = Some(Arc::new(move |task: &str, n: usize| {
            index.find_capabilities(task, n).into_iter().filter(|c| c.kind == "tool").map(|c| c.name).collect()
        }));
        agent
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
            let agent = me.agent(me.tools.read().unwrap().clone(), &prompt);
            let ui = me.clone();
            let status = move |s: &str| ui.ui(UiEvent::JobStatus(job, s.to_string()));
            *me.current_job.lock().unwrap() = Some(job);
            let ev = match agent.run(&prompt, &snap, &history, &status).await {
                Ok(r) => UiEvent::JobDone { job, result: Some(r), error: None },
                Err(e) => UiEvent::JobDone { job, result: None, error: Some(e.to_string()) },
            };
            *me.current_job.lock().unwrap() = None;
            me.agent_running.store(false, Ordering::SeqCst);
            me.ui(ev);
        });
        job
    }

    /// An emailed request from the owner: reduced tools, screen only if allowed.
    pub async fn run_remote(&self, prompt: &str) -> String {
        let cfg = self.cfg().agent_mail;
        // Nobody is at the desk to confirm file changes, whatever the config says.
        let mut exclude = cfg.remote_exclude_tools.clone();
        exclude.extend(crate::connectors::files::WRITE_TOOLS.map(String::from));
        let tools = self.tools.read().unwrap().without(&exclude);
        let snap = if cfg.remote_can_see_screen {
            self.snapshot(true).await
        } else {
            Snapshot::new(WindowInfo::new("(the user is away; screen not shared)", ""))
        };
        let history = self.suggester.lock().await.history.clone();
        self.agent_running.store(true, Ordering::SeqCst);
        let result = self.agent(tools, prompt).run(prompt, &snap, &history, &|_| {}).await;
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
        drop(slot);
        if self.ready.load(Ordering::SeqCst) {
            self.refresh_capabilities();
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

    // --- other computers and servers ---------------------------------------------------------
    /// Model servers on this computer and the network (other Fanes, Ollama, ...).
    pub async fn discover_servers(&self) -> Vec<Server> {
        let cfg = self.cfg();
        let mut extra = vec![self.llm.host()];
        extra.extend(cfg.remotes.values().map(|r| r.url.clone()));
        discovery::discover(&extra).await
    }

    /// Use the server at `url`: another Fane or a stable-diffusion.cpp server is added to
    /// `[remotes]`; an Ollama (or other chat) server becomes the chat server (`[model] host`).
    /// Returns (where, model).
    pub async fn use_server(self: &Arc<Self>, url: &str, token: &str) -> Result<(String, String), String> {
        let probe_url = if url.contains("://") || url.contains(':') {
            discovery::normalize_host(url).map_err(|e| e.0)?
        } else {
            discovery::normalize_with_port(url, 11500).map_err(|e| e.0)?
        };
        let server = discovery::probe(&probe_url).await.map_err(|e| e.0)?;
        if server.kind == "fane" || server.kind == "sd" {
            let name = self.add_remote(&server.url, token).await?;
            return Ok((name, String::new()));
        }
        let mcfg = self.cfg().model;
        if choose_model(&mcfg, &server.models).is_none() {
            return Err(format!("{} has none of Fane's models (e.g. {}).", server.url, mcfg.name));
        }
        let url = server.url.clone();
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
        tracing::info!("switched to the server at {url} (model {model})");
        self.notify(format!("Fane now uses {url} ({model})"));
        Ok((url, model))
    }

    /// Go back to Fane's own models for chat (clear `[model] host`).
    pub async fn use_own_models(self: &Arc<Self>) -> Result<String, String> {
        set_value("model", "host", "").map_err(|e| e.to_string())?;
        self.cfg.write().unwrap().model.host.clear();
        let host = self.cfg().llm_host();
        self.llm.set_host(&host);
        self.suggest_llm.set_host(&host);
        self.resolve_models(true).await;
        self.models_changed().await;
        Ok(format!("Fane uses its own models ({})", self.llm.model()))
    }

    pub async fn status(&self) -> Value {
        let mb = self.mailbox.lock().await.clone();
        let cfg = self.cfg();
        json!({
            "model_server": self.llm.host(),
            "own_models": self.cfg().model.host.trim().is_empty(),
            "serving_here": self.runtime.read().unwrap().is_some(),
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
            "server_use" | "ollama_use" => {
                let me = self.clone();
                let host = arg("host");
                let token = arg("token");
                self.rt.spawn(async move {
                    if let Err(e) = me.use_server(&host, &token).await {
                        me.notify(e);
                    }
                });
            }
            "reload" => {
                // `fane` commands changed the config: pick up models, routes and remotes.
                if let Ok(fresh) = Config::load() {
                    let mut cfg = self.cfg.write().unwrap();
                    cfg.runtime = fresh.runtime;
                    cfg.routes = fresh.routes;
                    cfg.remotes = fresh.remotes;
                    cfg.media = fresh.media;
                }
                let me = self.clone();
                self.rt.spawn(async move { me.models_changed().await });
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
        match self.0.upgrade() {
            Some(core) => core.confirm(details).await,
            None => false,
        }
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
