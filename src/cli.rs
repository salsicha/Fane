//! `fane` command line: start the app, talk to it, and set up the desktop.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::config::{self, CONFIG_PATH, Config, RECOMMENDED_MODELS, Role, STATE_DIR};
use crate::ipc;

#[derive(Parser)]
#[command(name = "fane", version, about = "Unobtrusive local desktop assistant.")]
pub struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start Fane (default)
    Run {
        #[arg(short, long)]
        verbose: bool,
        /// Get llama.cpp and the model if they're missing, without asking
        #[arg(long)]
        setup: bool,
        /// With --setup: also the coding model
        #[arg(long)]
        code: bool,
        /// With --setup: also image generation
        #[arg(long)]
        images: bool,
        /// With --setup: also video generation
        #[arg(long)]
        video: bool,
    },
    /// Get llama.cpp and Fane's model (Gemma 4 26B; Ollama's copy is used if it has one), and
    /// optionally the coding, image and video models
    Setup {
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
        /// Also the coding model (Qwen 3.8 27B)
        #[arg(long)]
        code: bool,
        /// Also image generation (stable-diffusion.cpp and Z-Image Turbo)
        #[arg(long)]
        images: bool,
        /// Also video generation (stable-diffusion.cpp and Wan 2.2)
        #[arg(long)]
        video: bool,
        /// Everything: --code --images --video
        #[arg(long)]
        all: bool,
        /// Use this model instead of the configured one (a name, or "hf:user/repo/file.gguf")
        #[arg(long)]
        model: Option<String>,
    },
    /// Run only Fane's model server, without a window (what the background service runs)
    Serve {
        #[arg(short, long)]
        verbose: bool,
    },
    /// List this computer's GPUs, or choose one for a kind of work
    Gpus {
        /// chat, code, image or video
        role: Option<String>,
        /// A GPU id from the list, or "auto"
        gpu: Option<String>,
    },
    /// List the models Fane can run here, and where each kind of work goes
    Models,
    /// Set where a kind of work goes, first choice first: "local", a remote's name, "claude",
    /// "codex" (e.g. `fane route code nuc claude`)
    Route {
        role: String,
        #[arg(required = true)]
        targets: Vec<String>,
    },
    /// Other computers running Fane (or servers) to send work to
    Remote {
        #[command(subcommand)]
        action: RemoteCmd,
    },
    /// Apps Fane can set up and use through MCP (Blender, FreeCAD, a web browser)
    Apps {
        #[command(subcommand)]
        action: Option<AppsCmd>,
    },
    /// Let other computers on your network use this one's models (prints the token they need)
    Share { state: OnOff },
    /// Run Fane's model server in the background, also when Fane isn't open
    Service { state: OnOff },
    /// Show/hide the prompt box
    Toggle,
    /// Open the prompt box and run a request
    Ask {
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Suggest something now
    Suggest,
    /// Pause proactive suggestions
    Pause,
    /// Resume suggestions
    Resume,
    /// Show the running app's status
    Status,
    /// Open the settings window
    Settings,
    /// Stop the app
    Quit,
    /// Bind a GNOME keyboard shortcut to `fane toggle` (Linux)
    SetupShortcut {
        /// e.g. '<Control><Super>space'
        #[arg(long)]
        binding: Option<String>,
    },
    /// Install the GNOME Shell helper extension (Linux)
    InstallExtension,
    /// Start Fane when you log in
    Autostart { state: OnOff },
    /// Fane's own email account (GNOME Online Accounts, Linux)
    Mail {
        #[command(subcommand)]
        action: MailCmd,
    },
    /// Find model servers (other Fanes, Ollama, ...) on your network and pick one
    #[command(alias = "ollama")]
    Servers {
        #[command(subcommand)]
        action: OllamaCmd,
    },
    /// Update the index of your files now and show what's in it
    Index {
        /// Look something up in the index
        #[arg(long)]
        query: Option<String>,
    },
    /// List or forget your saved preferences
    Prefs {
        /// Forget the preference with this id
        #[arg(long)]
        forget: Option<i64>,
    },
    /// Check what works on this machine
    Doctor {
        /// Also test screen capture
        #[arg(long)]
        screenshot: bool,
    },
    /// Compare local models on Fane's prompts and tools
    Eval(EvalArgs),
    /// Write Fane's icon as a PNG (used by packaging)
    #[command(hide = true)]
    RenderIcon { path: PathBuf, size: u32 },
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Subcommand)]
enum MailCmd {
    /// List the Google accounts in Online Accounts
    Accounts,
    /// Use this account as Fane's mailbox
    Use { address: String },
    /// Turn Fane's email off
    Off,
    /// Show or set your personal address: only mail from it is read, acted on and answered
    Owner {
        /// Your address ("none" to clear)
        address: Option<String>,
    },
    /// Show the email setup
    Status,
}

#[derive(Subcommand)]
enum OllamaCmd {
    /// List the model servers Fane can find
    Scan,
    /// Use a server (host, host:port or URL): another Fane or a stable-diffusion.cpp server is
    /// added as a remote; Ollama becomes the chat server
    Use {
        address: String,
        /// Another Fane's sharing token
        #[arg(long, default_value = "")]
        token: String,
    },
}

#[derive(Subcommand)]
enum AppsCmd {
    /// Install the app if needed, its MCP add-on and server, and connect Fane to it
    Setup {
        app: String,
        #[arg(short, long)]
        yes: bool,
    },
    /// Stop using an app's MCP server (the app stays installed)
    Remove { app: String },
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// List the remotes and what they have
    List,
    /// Add another computer running Fane (host, host:port or URL) with its sharing token
    Add {
        address: String,
        #[arg(long, default_value = "")]
        token: String,
    },
    /// Remove a remote (and take it out of the routes)
    Remove { name: String },
}

#[derive(clap::Args)]
struct EvalArgs {
    /// Models (default: the recommended ones)
    #[arg(long, num_args = 1..)]
    models: Vec<String>,
    /// Run each case N times
    #[arg(long, default_value_t = 1)]
    repeat: usize,
    /// Run one part of the suite
    #[arg(long, value_parser = ["suggest", "agent", "harness"])]
    only: Option<String>,
    /// Also have each model's paired coding harness (CodeGem, Qwen Code) fix a failing test
    #[arg(long)]
    harness: bool,
    /// In the harness task, have Claude Code (or Codex) plan and review the local harness's
    /// work, as Fane does when they're installed (uses that subscription)
    #[arg(long)]
    advisor: bool,
    /// Only cases whose name contains this
    #[arg(long)]
    filter: Option<String>,
    /// Seconds per case
    #[arg(long, default_value_t = 600.0)]
    timeout: f64,
    /// Override [runtime] context (applies when the eval starts its own model server)
    #[arg(long)]
    num_ctx: Option<u32>,
    /// Override [model] temperature
    #[arg(long)]
    temperature: Option<f64>,
    /// Override [model] tool_mode
    #[arg(long, value_parser = ["auto", "native", "json"])]
    tool_mode: Option<String>,
    /// Output directory (default <state dir>/evals/<time>)
    #[arg(long)]
    out: Option<PathBuf>,
    /// List the cases and exit
    #[arg(long)]
    list: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn wants_console(args: &[String]) -> bool {
    args.get(1).is_some_and(|a| a != "run")
}

pub fn main() -> i32 {
    let cli = Cli::parse();
    let result =
        match cli.cmd.unwrap_or(Cmd::Run { verbose: false, setup: false, code: false, images: false, video: false }) {
            Cmd::Run { verbose, setup, code, images, video } => {
                cmd_run(verbose, setup.then_some(crate::setup::Want { code, images, video }))
            }
            Cmd::Setup { yes, code, images, video, all, model } => cmd_setup(
                yes,
                crate::setup::Want { code: code || all, images: images || all, video: video || all },
                model,
            ),
            Cmd::Serve { verbose } => cmd_serve(verbose),
            Cmd::Gpus { role, gpu } => cmd_gpus(role, gpu),
            Cmd::Models => cmd_models(),
            Cmd::Route { role, targets } => cmd_route(&role, targets),
            Cmd::Remote { action } => cmd_remote(action),
            Cmd::Apps { action } => cmd_apps(action),
            Cmd::Share { state } => cmd_share(matches!(state, OnOff::On)),
            Cmd::Service { state } => cmd_service(matches!(state, OnOff::On)),
            Cmd::Toggle => cmd_simple("toggle", json!({})),
            Cmd::Ask { text } => cmd_simple("ask", json!({"text": text.join(" ")})),
            Cmd::Suggest => cmd_simple("suggest", json!({})),
            Cmd::Pause => cmd_simple("pause", json!({})),
            Cmd::Resume => cmd_simple("resume", json!({})),
            Cmd::Status => cmd_simple("status", json!({})),
            Cmd::Settings => cmd_simple("settings", json!({})),
            Cmd::Quit => cmd_simple("quit", json!({})),
            Cmd::SetupShortcut { binding } => message(crate::platform::setup_shortcut(
                &binding.unwrap_or_else(|| Config::load().map(|c| c.hotkey.binding).unwrap_or_default()),
            )),
            Cmd::InstallExtension => message(crate::platform::install_extension()),
            Cmd::Autostart { state } => message(crate::platform::set_autostart(matches!(state, OnOff::On))),
            Cmd::Mail { action } => cmd_mail(action),
            Cmd::Servers { action } => cmd_ollama(action),
            Cmd::Doctor { screenshot } => cmd_doctor(screenshot),
            Cmd::Prefs { forget } => cmd_prefs(forget),
            Cmd::Index { query } => cmd_index(query),
            Cmd::Eval(args) => cmd_eval(args),
            Cmd::RenderIcon { path, size } => crate::ui::icon::rgba(size).save(&path).map(|_| 0).map_err(Into::into),
        };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn message(r: anyhow::Result<String>) -> anyhow::Result<i32> {
    println!("{}", r?);
    Ok(0)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime")
}

fn setup_logging(verbose: bool) {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let _ = std::fs::create_dir_all(&*STATE_DIR);
    let level = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(format!("fane={level},warn")));
    let file = tracing_appender::rolling::never(&*STATE_DIR, "fane.log");
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(fmt::layer().with_ansi(false).with_writer(file))
        .try_init();
}

fn cmd_run(verbose: bool, setup: Option<crate::setup::Want>) -> anyhow::Result<i32> {
    if ipc::running() {
        println!("Fane is already running. Use `fane toggle` to open it.");
        return Ok(0);
    }
    setup_logging(verbose);
    let cfg = Config::load()?;
    let rt = runtime();
    let core = rt.block_on(crate::core::Core::new(cfg));
    if let Some(want) = setup {
        core.auto_setup.store(true, std::sync::atomic::Ordering::SeqCst);
        *core.auto_want.lock().unwrap() = want;
    }
    crate::ui::run(core, rt.handle().clone())?;
    rt.shutdown_timeout(Duration::from_secs(2));
    Ok(0)
}

fn confirm(question: &str) -> anyhow::Result<bool> {
    use std::io::Write;
    print!("{question} [Y/n] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(!answer.trim().to_lowercase().starts_with('n'))
}

fn cmd_setup(yes: bool, want: crate::setup::Want, model: Option<String>) -> anyhow::Result<i32> {
    use std::io::Write;
    let mut cfg = Config::load()?;
    if let Some(m) = model {
        if m.starts_with("hf:") {
            // A model by file: run it under its file name.
            let name = m.rsplit('/').next().unwrap_or(&m).trim_end_matches(".gguf").to_lowercase();
            cfg.runtime.models.insert(name.clone(), config::LocalModelCfg { file: m, ..Default::default() });
            cfg.model.name = name;
        } else {
            cfg.model.name = m;
        }
    }
    if crate::setup::is_local_ollama(&cfg.model.host) {
        println!("Fane uses Ollama at {} for chat. With setup it runs models itself (llama.cpp),", cfg.model.host);
        println!("using what Ollama already downloaded. Ollama is left as it is.");
        if !yes && !confirm("Switch to Fane's own models?")? {
            println!("Nothing changed.");
            return Ok(1);
        }
        config::set_value("model", "host", "")?;
        cfg.model.host.clear();
    }
    let plan = crate::setup::plan(&cfg, want);
    for note in &plan.notes {
        println!("Note: {note}.");
    }
    if plan.is_empty() {
        println!("Everything is set up. Nothing to do.");
        reload_running();
        return Ok(0);
    }
    println!("Fane will: {}.", plan.describe(&cfg));
    println!("Into {} (llama.cpp and stable-diffusion.cpp come from their GitHub releases).", cfg.runtime.home);
    if !yes && !confirm("Continue?")? {
        println!("Nothing changed.");
        return Ok(1);
    }
    let progress = |s: &str| {
        print!("\r\x1b[2K{s}");
        let _ = std::io::stdout().flush();
    };
    match runtime().block_on(crate::setup::run(&plan, &cfg, &progress)) {
        Ok(()) => {
            println!("\nDone. Start Fane with `fane` (or from your app menu).");
            reload_running();
            Ok(0)
        }
        Err(e) => {
            eprintln!("\nSetup didn't finish: {e}");
            Ok(1)
        }
    }
}

/// Tell a running Fane (or the background model server) that the config changed.
fn reload_running() {
    if ipc::send("reload", json!({})).is_some() {
        return;
    }
    if let Ok(cfg) = Config::load() {
        let url = cfg.runtime.url();
        runtime().block_on(async {
            let c = crate::llm::http_client(Duration::from_secs(5));
            let _ = c.post(format!("{url}/fane/v1/reload")).send().await;
        });
    }
}

fn cmd_serve(verbose: bool) -> anyhow::Result<i32> {
    setup_logging(verbose);
    let cfg = Config::load()?;
    let rt = runtime();
    rt.block_on(async {
        if crate::runtime::router::answering(&cfg.runtime.url()).await {
            anyhow::bail!("a Fane model server is already running at {}", cfg.runtime.url());
        }
        let runtime = crate::runtime::Runtime::new(cfg.clone());
        let listener = crate::runtime::router::bind(&cfg.runtime).await.map_err(anyhow::Error::msg)?;
        let stop = async {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
        };
        crate::runtime::router::run(listener, runtime.clone(), stop).await.map_err(anyhow::Error::msg)?;
        runtime.shutdown().await;
        Ok(())
    })?;
    Ok(0)
}

fn parse_role(role: &str) -> anyhow::Result<Role> {
    Role::parse(&role.to_lowercase()).ok_or_else(|| anyhow::anyhow!("{role}: use chat, code, image or video"))
}

fn cmd_gpus(role: Option<String>, gpu: Option<String>) -> anyhow::Result<i32> {
    let cfg = Config::load()?;
    crate::runtime::install::set_home(cfg.runtime.home_dir());
    let gpus = crate::runtime::gpu::detect();
    if let (Some(role), Some(gpu)) = (&role, &gpu) {
        let role = parse_role(role)?;
        if gpu != "auto" && !gpus.iter().any(|g| &g.id == gpu) {
            eprintln!(
                "No GPU {gpu}. These are here: {}",
                gpus.iter().map(|g| g.id.as_str()).collect::<Vec<_>>().join(", ")
            );
            return Ok(1);
        }
        config::set_value("runtime", &format!("{}_gpu", role.as_str()), gpu.as_str())?;
        reload_running();
        println!("{} work runs on {gpu} from its next start.", role.as_str());
        return Ok(0);
    }
    for g in &gpus {
        let uses: Vec<&str> = Role::ALL
            .into_iter()
            .filter(|r| crate::runtime::gpu::pick(&gpus, cfg.runtime.gpu_for(*r)).id == g.id)
            .map(|r| r.as_str())
            .collect();
        let uses = if uses.is_empty() { String::new() } else { format!("  ← {}", uses.join(", ")) };
        println!("{:<10} {}{uses}", g.id, g.label());
    }
    println!("\nChoose with e.g. `fane gpus chat vulkan:0` (\"auto\" = the most capable).");
    Ok(0)
}

fn cmd_models() -> anyhow::Result<i32> {
    use crate::runtime::catalog;
    let cfg = Config::load()?;
    crate::runtime::install::set_home(cfg.runtime.home_dir());
    println!("Language models this computer can run:");
    let available = catalog::available_llms(&cfg.runtime);
    for name in &available {
        let f = catalog::resolve_llm(name, &cfg.runtime);
        let src = f.as_ref().map(|f| f.source).unwrap_or("?");
        let mark = if *name == cfg.model.name {
            "  ← everyday"
        } else if *name == cfg.model.code {
            "  ← coding"
        } else {
            ""
        };
        println!("  {name}  ({})  {mark}", if src == "ollama" { "Ollama's download" } else { "Fane's download" });
    }
    if available.is_empty() {
        println!("  none — run `fane setup`");
    }
    println!("Image and video models:");
    for spec in &catalog::SD_MODELS {
        let have = catalog::sd_installed(spec);
        let used = spec.name == cfg.runtime.image_model || spec.name == cfg.runtime.video_model;
        println!(
            "  {} {}{}",
            if have { "✓" } else { "·" },
            spec.label,
            if have {
                String::new()
            } else {
                format!(" — fane setup --{}", if spec.video { "video" } else { "images" })
            } + if used { "" } else { " (not in use)" }
        );
    }
    println!("Routes (first choice first):");
    for role in Role::ALL {
        println!("  {:<6} {}", role.as_str(), cfg.routes.for_role(role).join(" → "));
    }
    if !cfg.model.host.is_empty() {
        println!("Chat goes to {} ([model] host).", cfg.model.host);
    }
    Ok(0)
}

fn cmd_route(role: &str, targets: Vec<String>) -> anyhow::Result<i32> {
    let role = parse_role(role)?;
    let cfg = Config::load()?;
    for t in &targets {
        if !["local", "claude", "codex"].contains(&t.as_str()) && !cfg.remotes.contains_key(t) {
            eprintln!("{t} isn't \"local\", \"claude\", \"codex\" or a remote (see `fane remote list`).");
            return Ok(1);
        }
    }
    config::set_value("routes", role.as_str(), config::str_array(&targets))?;
    reload_running();
    println!("{} requests go to: {}", role.as_str(), targets.join(", then "));
    Ok(0)
}

fn cmd_remote(action: RemoteCmd) -> anyhow::Result<i32> {
    let cfg = Config::load()?;
    let rt = runtime();
    match action {
        RemoteCmd::List => {
            if cfg.remotes.is_empty() {
                println!("No remotes. Add one with `fane remote add HOST --token TOKEN`.");
            }
            for (name, r) in &cfg.remotes {
                let what = match rt.block_on(crate::discovery::identify(&r.url, &r.token, &r.kind)) {
                    Ok(f) if f.kind == "sd" => "stable-diffusion.cpp server".to_string(),
                    Ok(f) => format!("{} — {}", f.kind, f.models.join(", ")),
                    Err(e) => format!("not reachable: {e}"),
                };
                let routes: Vec<&str> = Role::ALL
                    .into_iter()
                    .filter(|role| cfg.routes.for_role(*role).contains(name))
                    .map(|r| r.as_str())
                    .collect();
                println!(
                    "{name}  {}  {what}  (routes: {})",
                    r.url,
                    if routes.is_empty() { "none".into() } else { routes.join(", ") }
                );
            }
        }
        RemoteCmd::Add { address, token } => {
            let url = crate::discovery::normalize_with_port(&address, 11500).map_err(|e| anyhow::anyhow!(e.0))?;
            let found = match rt.block_on(crate::discovery::identify(&url, &token, "")) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{e}");
                    return Ok(1);
                }
            };
            let host = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
            let name: String = host
                .split('.')
                .next()
                .unwrap_or("remote")
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
                .collect();
            config::set_remote(&name, Some(&config::RemoteCfg { url: url.clone(), token, kind: found.kind.clone() }))?;
            reload_running();
            println!("Added {name} ({}). Send work to it with e.g. `fane route code {name} local`.", found.kind);
        }
        RemoteCmd::Remove { name } => {
            config::set_remote(&name, None)?;
            for role in Role::ALL {
                let list = cfg.routes.for_role(role);
                if list.contains(&name) {
                    let kept: Vec<String> = list.iter().filter(|t| **t != name).cloned().collect();
                    config::set_value("routes", role.as_str(), config::str_array(&kept))?;
                }
            }
            reload_running();
            println!("Removed {name}.");
        }
    }
    Ok(0)
}

fn cmd_apps(action: Option<AppsCmd>) -> anyhow::Result<i32> {
    use crate::connectors::apps;
    let cfg = Config::load()?;
    crate::runtime::install::set_home(cfg.runtime.home_dir());
    match action {
        None => {
            for a in &apps::APPS {
                let state = match (apps::find(a).is_some(), cfg.mcp.servers.contains_key(a.id)) {
                    (_, true) => "set up",
                    (true, false) => "installed, not connected",
                    (false, false) => "not installed",
                };
                println!("{:<8} {:<12} {state:<26} {}", a.id, a.name, a.what);
            }
            println!("\nSet one up with e.g. `fane apps setup blender`.");
        }
        Some(AppsCmd::Setup { app, yes }) => {
            let Some(spec) = apps::spec(&app) else {
                eprintln!("No app {app:?}; see `fane apps`.");
                return Ok(1);
            };
            let steps = match apps::plan(spec) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{e}");
                    return Ok(1);
                }
            };
            println!("To {}, Fane will:", spec.what);
            for s in &steps {
                println!("  • {s}");
            }
            if !yes && !confirm("Continue?")? {
                println!("Nothing changed.");
                return Ok(1);
            }
            // A running Fane connects to the server right away; otherwise just configure it.
            if ipc::running() {
                ipc::send("app_setup", json!({"app": app}));
                println!("Setting {} up in Fane (watch its progress card).", spec.name);
                return Ok(0);
            }
            let progress = |s: &str| {
                use std::io::Write;
                print!("\r\x1b[2K{s}");
                let _ = std::io::stdout().flush();
            };
            match runtime().block_on(apps::setup(spec, false, &progress)) {
                Ok(server) => {
                    config::set_mcp_server(spec.id, Some(&server))?;
                    println!("\n{} is set up; Fane uses it from its next start.", spec.name);
                }
                Err(e) => {
                    eprintln!("\n{} isn't set up: {e}", spec.name);
                    return Ok(1);
                }
            }
        }
        Some(AppsCmd::Remove { app }) => {
            config::set_mcp_server(&app, None)?;
            println!("Fane no longer uses {app} (restart Fane if it's running; the app stays installed).");
        }
    }
    Ok(0)
}

fn cmd_share(on: bool) -> anyhow::Result<i32> {
    let cfg = Config::load()?;
    let mut token = cfg.runtime.token.clone();
    if on && token.is_empty() {
        use rand::RngExt;
        const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
        let mut rng = rand::rng();
        token = (0..20).map(|_| CHARS[rng.random_range(0..CHARS.len())] as char).collect();
        config::set_value("runtime", "token", token.as_str())?;
    }
    config::set_value("runtime", "share", on)?;
    if on {
        println!("Other computers can use this one's models at port {} with the token:\n  {token}", cfg.runtime.port);
        println!("On the other computer: fane remote add {} --token {token}", crate::runtime::hostname());
        println!("Restart Fane (or its background service) to start listening on the network.");
    } else {
        println!("Sharing is off (after Fane or its service restarts).");
    }
    reload_running();
    Ok(0)
}

fn cmd_service(on: bool) -> anyhow::Result<i32> {
    if on {
        if ipc::running() {
            println!(
                "Fane is running and serving models itself; turn the service on in Settings → Models, or quit Fane first."
            );
            return Ok(1);
        }
        println!("{}", crate::runtime::service::install().map_err(anyhow::Error::msg)?);
    } else {
        crate::runtime::service::uninstall().map_err(anyhow::Error::msg)?;
        println!("The background model server is off.");
    }
    Ok(0)
}

/// Start the app in the background (when the hotkey is pressed while it isn't running).
fn spawn_app() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    cmd.spawn()?;
    Ok(())
}

fn cmd_simple(cmd: &str, args: Value) -> anyhow::Result<i32> {
    let reply = match ipc::send(cmd, args.clone()) {
        Some(r) => r,
        None if cmd == "toggle" || cmd == "ask" => {
            spawn_app()?;
            println!("Starting Fane…");
            // Deliver the request once it's listening.
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(200));
                if ipc::running() {
                    std::thread::sleep(Duration::from_millis(500));
                    ipc::send(cmd, args);
                    return Ok(0);
                }
            }
            return Ok(0);
        }
        None => {
            eprintln!("Fane is not running (start it with `fane run`).");
            return Ok(1);
        }
    };
    if cmd == "status" {
        println!("{}", serde_json::to_string_pretty(&reply)?);
    }
    Ok(if reply["ok"].as_bool().unwrap_or(true) { 0 } else { 1 })
}

fn cmd_mail(action: MailCmd) -> anyhow::Result<i32> {
    use crate::mail::accounts::OnlineAccounts;
    let cfg = Config::load()?;
    let current = cfg.agent_mail.account.clone();
    let rt = runtime();
    match action {
        MailCmd::Accounts => {
            let accs = rt.block_on(OnlineAccounts::new().google_accounts())?;
            if accs.is_empty() {
                println!("No Google accounts in Settings → Online Accounts. Add the agent's account there.");
            }
            for a in &accs {
                let mark = if a.email.eq_ignore_ascii_case(&current) { "●" } else { "○" };
                let note = if a.mail_disabled { "  (mail switched off in Settings)" } else { "" };
                println!("{mark} {}{note}", a.email);
            }
            println!(
                "\nFane uses: {}. Change with `fane mail use ADDRESS`.",
                if current.is_empty() { "nothing (email off)" } else { &current }
            );
        }
        MailCmd::Use { address } => {
            let accs = rt.block_on(OnlineAccounts::new().google_accounts())?;
            if !accs.iter().any(|a| a.email.eq_ignore_ascii_case(&address)) {
                eprintln!("{address} isn't in Settings → Online Accounts (see `fane mail accounts`).");
                return Ok(1);
            }
            if ipc::send("mail_use", json!({"address": address})).is_none() {
                config::set_value("agent_mail", "account", address.as_str())?;
            }
            println!("Fane's email account: {address}");
        }
        MailCmd::Off => {
            if ipc::send("mail_use", json!({"address": ""})).is_none() {
                config::set_value("agent_mail", "account", "")?;
            }
            println!("Fane's email is off.");
        }
        MailCmd::Owner { address: None } => match cfg.agent_mail.owner() {
            Some(o) => println!("Only mail from {o} is read, acted on and answered."),
            None => println!("Not set: Fane won't act on any email. Set it with `fane mail owner you@example.com`."),
        },
        MailCmd::Owner { address: Some(address) } => {
            let address =
                if address.eq_ignore_ascii_case("none") { String::new() } else { address.trim().to_lowercase() };
            if !address.is_empty() && !crate::core::valid_email(&address) {
                eprintln!("{address} doesn't look like an email address.");
                return Ok(1);
            }
            config::set_value("agent_mail", "owner_email", address.as_str())?;
            ipc::send("mail_reload", json!({}));
            if address.is_empty() {
                println!("Cleared: Fane won't act on any email.");
            } else {
                println!("Fane will only read and answer mail from {address}.");
            }
        }
        MailCmd::Status => {
            println!("account:  {}", if current.is_empty() { "(off)" } else { &current });
            println!(
                "you:      {}",
                cfg.agent_mail.owner().unwrap_or_else(|| "(not set — no mail is acted on)".into())
            );
            match ipc::send("status", json!({})) {
                Some(r) => {
                    let watching = r["mail_account"].as_str().unwrap_or("nothing");
                    let err = r["mail_error"].as_str().map(|e| format!(" — error: {e}")).unwrap_or_default();
                    println!("running:  watching {watching}{err}");
                }
                None => println!("running:  no (start Fane with `fane`)"),
            }
        }
    }
    Ok(0)
}

fn cmd_ollama(action: OllamaCmd) -> anyhow::Result<i32> {
    use crate::discovery::{discover, normalize_host, probe};
    use crate::llm::choose_model;
    let cfg = Config::load()?;
    let mcfg = cfg.model.clone();
    let rt = runtime();
    match action {
        OllamaCmd::Scan => {
            println!("Looking for model servers on this computer and your network…");
            let mut extra = vec![mcfg.host.clone()];
            extra.extend(cfg.remotes.values().map(|r| r.url.clone()));
            let servers = rt.block_on(discover(&extra));
            for s in &servers {
                let used =
                    (!mcfg.host.is_empty() && s.matches(&mcfg.host)) || cfg.remotes.values().any(|r| s.matches(&r.url));
                let mark = if used { "●" } else { "○" };
                let name = if s.label() == s.address() { String::new() } else { format!("  ({})", s.label()) };
                let what = match s.kind.as_str() {
                    "fane" if s.locked => "Fane (needs its sharing token)".to_string(),
                    "fane" => format!("Fane, {} models", s.models.len()),
                    "sd" => "stable-diffusion.cpp".to_string(),
                    "ollama" => format!(
                        "Ollama {}, {} models, {}",
                        s.version,
                        s.models.len(),
                        choose_model(&mcfg, &s.models)
                            .map(|m| format!("has {m}"))
                            .unwrap_or_else(|| "none of Fane's models".into())
                    ),
                    _ => format!("model server, {} models", s.models.len()),
                };
                println!("{mark} {}{name}  {what}", s.url);
            }
            if servers.is_empty() {
                println!("None found. If one is on another network or port: fane servers use HOST:PORT");
            }
            Ok(0)
        }
        OllamaCmd::Use { address, token } => {
            let url = match normalize_host(&address) {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("{e}");
                    return Ok(1);
                }
            };
            let server = match rt.block_on(probe(&url)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{e}");
                    return Ok(1);
                }
            };
            if server.kind == "fane" || server.kind == "sd" {
                return cmd_remote(RemoteCmd::Add { address: server.url, token });
            }
            let Some(model) = choose_model(&mcfg, &server.models) else {
                eprintln!("{url} has none of Fane's models (e.g. {}).", mcfg.name);
                return Ok(1);
            };
            if ipc::send("server_use", json!({"host": url})).is_none() {
                config::set_value("model", "host", url.as_str())?;
            }
            println!("Fane now uses {url} for chat ({model}).");
            Ok(0)
        }
    }
}

fn cmd_doctor(screenshot: bool) -> anyhow::Result<i32> {
    use crate::llm::{Llm, find_installed};
    let cfg = Config::load()?;
    let ok = |b: bool| if b { "✓" } else { "✗" };
    let rt = runtime();
    rt.block_on(async {
        println!("config: {}", CONFIG_PATH.display());
        println!("os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
        crate::runtime::install::set_home(cfg.runtime.home_dir());
        let gpus = crate::runtime::gpu::detect();
        println!("  GPUs: {}", gpus.iter().map(|g| format!("{} {}", g.id, g.label())).collect::<Vec<_>>().join("; "));
        for b in [
            crate::runtime::gpu::Backend::Cuda,
            crate::runtime::gpu::Backend::Vulkan,
            crate::runtime::gpu::Backend::Metal,
        ] {
            if let Some(dir) = crate::runtime::install::installed_llama(b) {
                println!("{} llama.cpp ({}): {}", ok(true), b.as_str(), dir.display());
            }
            if let Some(dir) = crate::runtime::install::installed_sd(b) {
                println!("{} stable-diffusion.cpp ({}): {}", ok(true), b.as_str(), dir.display());
            }
        }
        let llm = Llm::new(&cfg.model, &cfg.llm_host());
        let serving = crate::runtime::router::answering(&cfg.runtime.url()).await;
        if cfg.model.host.is_empty() && !serving {
            println!("  model server: not running (it runs inside Fane, or as `fane serve`)");
            for name in RECOMMENDED_MODELS {
                let have = crate::runtime::catalog::resolve_llm(name, &cfg.runtime).is_some();
                let hint = if have { String::new() } else { " — fane setup".to_string() };
                println!("  {} {name}{hint}", ok(have));
            }
        } else {
            match llm.resolve_model().await {
                Ok(m) => {
                    println!("{} model: {m} on {}", ok(true), llm.host());
                    if let Ok(installed) = llm.installed_models().await {
                        for name in RECOMMENDED_MODELS {
                            let have = find_installed(name, &installed).is_some();
                            let hint = if have { String::new() } else { " — fane setup --code".to_string() };
                            println!("  {} recommended {name}{hint}", ok(have));
                        }
                    }
                }
                Err(e) => println!("{} model: {e}", ok(false)),
            }
        }
        let desktop = crate::platform::Desktop::connect().await;
        let win = desktop.focused_window().await;
        println!("{} focused window: {}", ok(!win.title.is_empty() || !win.app.is_empty()), win.label());
        if screenshot {
            let img = desktop.screenshot().await;
            let what = img
                .map(|i| format!("{}x{} via {}", i.width(), i.height(), desktop.screen_backend().unwrap_or_default()));
            println!("{} screenshot: {}", ok(what.is_some()), what.unwrap_or_else(|| "no backend worked".into()));
        } else {
            println!("  screenshot: skipped (pass --screenshot to test capture)");
        }
        for (good, text) in crate::platform::doctor_checks() {
            println!("{} {text}", ok(good));
        }
        for (i, (name, agent)) in cfg.delegate.all_agents().iter().enumerate() {
            let program = agent.cmd.first().map(String::as_str).unwrap_or("?");
            let installed = which::which(program).is_ok();
            let default = if i == 0 { ", default" } else { "" };
            let hint = match (installed, program) {
                (false, "superqode") => " — install: uv tool install superqode",
                (false, "qwen") => " — install: npm install -g @qwen-code/qwen-code",
                _ => "",
            };
            println!("{} delegate to {name} ({program}{default}){hint}", ok(installed));
        }
        match crate::connectors::review::advisor(&cfg.delegate) {
            Some((name, _)) => println!(
                "{} planning and review: {} (must be signed in; `[delegate] advisor`)",
                ok(true),
                if name == "claude" { "Claude Code" } else { "Codex" }
            ),
            None => println!("  planning and review: off (install Claude Code or Codex, or set `[delegate] advisor`)"),
        }
        if let Ok(store) = crate::store::Store::open(&STATE_DIR.join("fane.db")) {
            let store = std::sync::Arc::new(store);
            let index = crate::index::Index::new(store.clone(), crate::index::Scope::from_cfg(&cfg.files));
            let n = index.file_count();
            println!("{} index: {n} files, {} tools/agents", ok(index.ready()), index.capabilities().len());
            println!("  preferences: {}", crate::prefs::Prefs::new(store).list().len());
        }
        let mail = &cfg.agent_mail.account;
        println!(
            "{} Fane's email: {}",
            ok(!mail.is_empty()),
            if mail.is_empty() { "off (fane mail accounts / fane mail use)" } else { mail }
        );
        if !mail.is_empty() {
            let owner = cfg.agent_mail.owner();
            println!(
                "{} your email: {}",
                ok(owner.is_some()),
                owner.unwrap_or_else(|| "not set — no mail will be acted on".into())
            );
        }
        let servers: Vec<&String> = cfg.mcp.servers.keys().collect();
        println!(
            "  MCP servers configured: {}",
            if servers.is_empty() {
                "none".into()
            } else {
                servers.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            }
        );
        println!("  running: {}", if ipc::running() { "yes" } else { "no" });
    });
    Ok(0)
}

fn cmd_index(query: Option<String>) -> anyhow::Result<i32> {
    let cfg = Config::load()?;
    let db = STATE_DIR.join("fane.db");
    let store = std::sync::Arc::new(crate::store::Store::open(&db)?);
    let index = crate::index::Index::new(store, crate::index::Scope::from_cfg(&cfg.files));
    if let Some(q) = query {
        for hit in index.search_files(&q, 20) {
            let snippet = if hit.snippet.is_empty() { String::new() } else { format!("  — {}", hit.snippet) };
            println!("{}  ({}){snippet}", hit.path, hit.modified);
        }
        return Ok(0);
    }
    println!("Indexing {}…", cfg.files.roots.join(", "));
    let t0 = std::time::Instant::now();
    let s = index.scan()?;
    let size = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0)
        + std::fs::metadata(db.with_extension("db-wal")).map(|m| m.len()).unwrap_or(0);
    println!(
        "{} files ({} new or changed, {} removed) in {:.1}s; index is {:.0} MB",
        s.files,
        s.added_or_changed,
        s.removed,
        t0.elapsed().as_secs_f64(),
        size as f64 / 1e6
    );
    Ok(0)
}

fn cmd_prefs(forget: Option<i64>) -> anyhow::Result<i32> {
    let store = std::sync::Arc::new(crate::store::Store::open(&STATE_DIR.join("fane.db"))?);
    let prefs = crate::prefs::Prefs::new(store);
    if let Some(id) = forget {
        match prefs.forget(id) {
            Some(p) => println!("Forgot: {}", p.text),
            None => {
                eprintln!("No preference [{id}].");
                return Ok(1);
            }
        }
        return Ok(0);
    }
    let all = prefs.list();
    if all.is_empty() {
        println!("No preferences yet. Tell Fane how you want something done (\"from now on…\").");
    }
    for p in all {
        println!("{}", p.line());
    }
    Ok(0)
}

fn cmd_eval(args: EvalArgs) -> anyhow::Result<i32> {
    use crate::evals::runner::{EvalOptions, list_cases, run};
    if args.list {
        for line in list_cases() {
            println!("{line}");
        }
        return Ok(0);
    }
    let mut cfg = Config::load()?;
    if let Some(n) = args.num_ctx {
        cfg.runtime.context = n;
    }
    if let Some(t) = args.temperature {
        cfg.model.temperature = t;
    }
    if let Some(m) = args.tool_mode {
        cfg.model.tool_mode = m;
    }
    if ipc::running() {
        println!("Note: Fane is running; pause it (`fane pause`) so it doesn't compete for the GPU.");
    }
    let opts = EvalOptions {
        models: args.models,
        repeat: args.repeat,
        only: args.only,
        filter: args.filter,
        timeout_s: args.timeout,
        out: args.out,
        harness: args.harness || args.advisor,
        advisor: args.advisor,
    };
    runtime().block_on(run(cfg, opts))
}
