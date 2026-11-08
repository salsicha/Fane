//! `fane` command line: start the app, talk to it, and set up the desktop.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::config::{self, CONFIG_PATH, Config, RECOMMENDED_MODELS, STATE_DIR};
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
        /// Install Ollama and download the model if they're missing, without asking
        #[arg(long)]
        setup: bool,
    },
    /// Install Ollama and download the default model (Gemma 4 26B) if they're missing
    Setup {
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
        /// Only install and start Ollama
        #[arg(long)]
        skip_model: bool,
        /// Download this model instead of the configured one
        #[arg(long)]
        model: Option<String>,
    },
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
    /// Find Ollama servers on your network and pick one
    Ollama {
        #[command(subcommand)]
        action: OllamaCmd,
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
    /// List the Ollama servers Fane can find
    Scan,
    /// Switch to a server (host, host:port or URL)
    Use { address: String },
}

#[derive(clap::Args)]
struct EvalArgs {
    /// Ollama tags (default: the three recommended models)
    #[arg(long, num_args = 1..)]
    models: Vec<String>,
    /// Run each case N times
    #[arg(long, default_value_t = 1)]
    repeat: usize,
    /// Run one part of the suite
    #[arg(long, value_parser = ["suggest", "agent", "harness"])]
    only: Option<String>,
    /// Also have each model's paired coding harness (CodeGem, Qwen Code, SuperQode DeepSeek) fix a
    /// failing test
    #[arg(long)]
    harness: bool,
    /// Only cases whose name contains this
    #[arg(long)]
    filter: Option<String>,
    /// Seconds per case
    #[arg(long, default_value_t = 600.0)]
    timeout: f64,
    /// Override [model] num_ctx
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
    let result = match cli.cmd.unwrap_or(Cmd::Run { verbose: false, setup: false }) {
        Cmd::Run { verbose, setup } => cmd_run(verbose, setup),
        Cmd::Setup { yes, skip_model, model } => cmd_setup(yes, skip_model, model),
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
        Cmd::Ollama { action } => cmd_ollama(action),
        Cmd::Doctor { screenshot } => cmd_doctor(screenshot),
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

fn cmd_run(verbose: bool, setup: bool) -> anyhow::Result<i32> {
    if ipc::running() {
        println!("Fane is already running. Use `fane toggle` to open it.");
        return Ok(0);
    }
    setup_logging(verbose);
    let cfg = Config::load()?;
    let rt = runtime();
    let core = rt.block_on(crate::core::Core::new(cfg));
    core.auto_setup.store(setup, std::sync::atomic::Ordering::SeqCst);
    crate::ui::run(core, rt.handle().clone())?;
    rt.shutdown_timeout(Duration::from_secs(2));
    Ok(0)
}

fn cmd_setup(yes: bool, skip_model: bool, model: Option<String>) -> anyhow::Result<i32> {
    use std::io::Write;
    let mut mcfg = Config::load()?.model;
    if let Some(m) = model {
        mcfg.name = m;
    }
    let rt = runtime();
    let mut plan = match rt.block_on(crate::setup::plan(&mcfg)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return Ok(1);
        }
    };
    if skip_model {
        plan.model = None;
    }
    if plan.is_empty() {
        println!("Ollama is running at {} and {} is installed. Nothing to do.", plan.host, mcfg.name);
        return Ok(0);
    }
    println!("Fane will: {}.", plan.describe());
    if plan.install {
        println!("Ollama comes from its official installer (https://ollama.com/download).");
    }
    if !yes {
        print!("Continue? [Y/n] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim().to_lowercase().starts_with('n') {
            println!("Nothing changed.");
            return Ok(1);
        }
    }
    let progress = |s: &str| {
        print!("\r\x1b[2K{s}");
        let _ = std::io::stdout().flush();
    };
    match rt.block_on(crate::setup::run(&plan, false, &progress)) {
        Ok(()) => {
            println!("\nDone. Start Fane with `fane` (or from your app menu).");
            Ok(0)
        }
        Err(e) => {
            eprintln!("\nSetup didn't finish: {e}");
            Ok(1)
        }
    }
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
    let mcfg = Config::load()?.model;
    let rt = runtime();
    match action {
        OllamaCmd::Scan => {
            println!("Looking for Ollama on this computer and your network…");
            let servers = rt.block_on(discover(std::slice::from_ref(&mcfg.host)));
            for s in &servers {
                let mark = if s.matches(&mcfg.host) { "●" } else { "○" };
                let name = if s.label() == s.address() { String::new() } else { format!("  ({})", s.label()) };
                let model = choose_model(&mcfg, &s.models);
                println!(
                    "{mark} {}{name}  Ollama {}, {} models, {}",
                    s.url,
                    s.version,
                    s.models.len(),
                    model.map(|m| format!("would use {m}")).unwrap_or_else(|| "none of Fane's models".into())
                );
            }
            if servers.is_empty() {
                println!("None found. If yours is on another network or port: fane ollama use HOST:PORT");
            }
            println!("\nFane uses: {}. Change with `fane ollama use ADDRESS` or in Settings.", mcfg.host);
            Ok(0)
        }
        OllamaCmd::Use { address } => {
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
            let Some(model) = choose_model(&mcfg, &server.models) else {
                eprintln!("{url} has none of Fane's models. Pull one there first, e.g. ollama pull {}", mcfg.name);
                return Ok(1);
            };
            if ipc::send("ollama_use", json!({"host": url})).is_none() {
                config::set_value("model", "host", url.as_str())?;
            }
            println!("Fane now uses Ollama at {url} ({model}).");
            Ok(0)
        }
    }
}

fn cmd_doctor(screenshot: bool) -> anyhow::Result<i32> {
    use crate::llm::{Ollama, find_installed};
    let cfg = Config::load()?;
    let ok = |b: bool| if b { "✓" } else { "✗" };
    let rt = runtime();
    rt.block_on(async {
        println!("config: {}", CONFIG_PATH.display());
        println!("os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
        let llm = Ollama::new(&cfg.model);
        if crate::setup::is_local(&llm.host()) && crate::setup::ollama_binary().is_none() {
            println!("{} Ollama: not installed — run `fane setup`", ok(false));
        }
        match llm.resolve_model().await {
            Ok(m) => {
                println!("{} model: {m} on {}", ok(true), llm.host());
                if let Ok(installed) = llm.installed_models().await {
                    for name in RECOMMENDED_MODELS {
                        let have = find_installed(name, &installed).is_some();
                        let hint = if have { String::new() } else { format!(" — ollama pull {name}") };
                        println!("  {} recommended {name}{hint}", ok(have));
                    }
                }
            }
            Err(e) => println!("{} model: {e}", ok(false)),
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
        cfg.model.num_ctx = n;
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
        harness: args.harness,
    };
    runtime().block_on(run(cfg, opts))
}
