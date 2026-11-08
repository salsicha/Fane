# Fane

An unobtrusive, context-aware desktop assistant for Linux (GNOME), macOS and Windows. It's
a local-first take on [Fazm](https://fazm.ai), written in Rust.

Fane watches your screen and keeps prompting itself with "what would help right now?".
When it has a specific, useful idea, a small card slides into the corner of the screen
with a ready-to-run prompt. Click **Run** to do it, **Edit** to tweak the prompt first,
or ignore it and it fades away. Press a hotkey any time to type your own request.

- **Local model.** Uses a vision model served by [Ollama](https://ollama.com) (see
  [Recommended models](#recommended-models)), on this computer or
  [another one on your network](#using-ollama-on-another-computer). No screen content
  leaves your network unless you delegate a task.
- **Inserts at your cursor.** Drafts, replies and rewrites are pasted where you're already
  typing. Images work too.
- **Talks to your apps.** It reads your notes folders and Thunderbird mail directly, and any
  app that has an MCP server (Gmail, calendars, Slack, Obsidian, the filesystem and so on).
- **Has its own email** (Linux). Give it a Gmail account through GNOME Online Accounts. You
  can email it tasks from your own address, and it replies. Mail from anyone else is ignored.
- **Delegates the hard stuff.** Coding and long multi-step tasks go to Claude Code or Codex,
  using your own subscription through their command-line tools.
- **Stays out of the way.** It only re-reads the screen when something changed, suggests at
  most one thing at a time, and backs off further each time you dismiss a suggestion. It
  never looks at password managers or private browser windows.

## Install

You also need [Ollama](https://ollama.com/download) with one of the
[recommended models](#recommended-models), e.g. `ollama pull openbmb/minicpm-v4.7:35b-a3b`.

### Ubuntu 24.04+ (apt)

```bash
curl -fsSL https://salsicha.github.io/Fane/fane-archive-keyring.gpg | sudo tee /usr/share/keyrings/fane-archive-keyring.gpg >/dev/null
echo "deb [signed-by=/usr/share/keyrings/fane-archive-keyring.gpg] https://salsicha.github.io/Fane stable main" | sudo tee /etc/apt/sources.list.d/fane.list
sudo apt update && sudo apt install fane
```

Packages are built for amd64 and arm64. The `.deb` files are also attached to each
[GitHub release](https://github.com/salsicha/Fane/releases) (`sudo apt install ./fane_*.deb`).
Then set it up for your desktop:

```bash
fane install-extension        # GNOME helper (log out and back in once on Wayland)
fane setup-shortcut           # Ctrl+Super+Space opens the prompt box
fane autostart on             # optional: start at login
fane doctor --screenshot      # see what works on this machine
fane                          # run it (or open Fane from the app grid)
```

### macOS 11+

Download `Fane-<version>-macos.dmg` from the
[releases](https://github.com/salsicha/Fane/releases) and drag Fane to Applications. It's
a universal build (Apple silicon and Intel) that isn't notarized yet, so open it the first
time with right-click → **Open**. Fane lives in the menu bar. Grant it **Screen Recording**
(so it can see the screen) and **Accessibility** (so it can paste at your cursor) in System
Settings → Privacy & Security. The hotkey is <kbd>Cmd</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd>.

### Windows 10/11

Download `Fane-<version>-windows-x64-setup.exe` (per-user install, no admin rights) or the
portable `.zip` from the [releases](https://github.com/salsicha/Fane/releases). Fane lives
in the notification area. The hotkey is <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Space</kbd>.

On macOS and Windows, `fane autostart on` (from a terminal, using the full path to the
program) starts Fane at login, and the hotkey is `[hotkey] binding` in the config file.

### From source

With a [Rust toolchain](https://rustup.rs):

```bash
git clone git@github.com:salsicha/Fane.git && cd Fane
cargo install --path .        # puts `fane` in ~/.cargo/bin
```

No system libraries are needed to build on Linux: Fane loads X11, Wayland and OpenGL at
runtime.

## Using it

| Action | How |
| --- | --- |
| Ask for something | Hotkey, type, <kbd>Enter</kbd> |
| Put the answer where you're typing | **Insert at cursor** or <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>Enter</kbd> |
| Accept a suggestion | **Run** on the corner card (or **Edit** to change the prompt first) |
| Get a suggestion now | Tray menu → *Suggest something now*, or `fane suggest` |
| Pause suggestions | Tray menu, or `fane pause` / `fane resume` |
| Change settings | Tray menu → *Settings…*, or `fane settings` |
| Script it | `fane ask "summarize this page"`, `fane toggle`, `fane status`, `fane quit` |

Example requests: *"reply to this saying Thursday works"*, *"what did Dana email me about
the review?"*, *"find my notes on the Q3 plan and summarize them"*, *"fix the failing test
in ~/code/api"* (which gets delegated to Claude Code or Codex).

## Settings

**Settings…** in the tray menu covers the things you'd change most:

- **Ollama server**: pick the server Fane uses (see below).
- **Your email**: your personal address. Only mail from it is read, acted on and answered.
- **Fane's own Gmail account** (Linux): which Online Accounts mailbox Fane uses, or off.

Everything else lives in the config file (**Edit config file…**, see
[Configuration](#configuration)).

## Using Ollama on another computer

If a different machine on your network has the bigger GPU, Fane can use the Ollama
running there. Open **Settings…** from the tray menu. Fane scans this computer and your
local network (port 11434 on every address of your subnet, which takes about a second) and
lists each Ollama server it finds with its version, how many models it has, and which one
Fane would use. Pick one and click **Use this server**, or type an address such as
`192.168.1.20` or `http://gpu-box.lan:11434` if the server is on another network or port.

Fane only switches if the server answers and has one of Fane's models installed. The
choice is saved as `[model] host` and takes effect immediately, with no restart. From a
terminal:

```bash
fane ollama scan               # list the servers Fane can find
fane ollama use 192.168.1.20   # switch to one
```

Ollama only listens on localhost by default. To share it, set `OLLAMA_HOST=0.0.0.0:11434`
on the server (on Linux: `sudo systemctl edit ollama`, then add
`Environment="OLLAMA_HOST=0.0.0.0:11434"` under `[Service]`) and allow port 11434 through
its firewall.

## Fane's own email

Fane can have its own Gmail address (Linux only). It uses the Google sign-in built into
GNOME (Settings → Online Accounts), so Fane never sees or stores a password. It asks Online
Accounts for a short-lived token for **one account that you choose**.

1. Settings → Online Accounts → Google: sign in to the agent's account and keep **Mail**
   switched on.
2. In Fane's **Settings…**, pick that account under *Fane's own Gmail account*
   (or `fane mail use agent@gmail.com`). Your other Google accounts stay untouched. If you
   don't choose one, email stays off; Fane never falls back to another account.
3. Enter **Your email** (or `fane mail owner you@gmail.com`).

How Fane handles mail:

| Situation | What Fane does |
| --- | --- |
| Mail from your address that passes Google's DMARC check | Runs it as a request and replies in the same thread. It can't type on your desktop, delegate to coding agents, send other mail or see your screen (all configurable), and it sends at most 20 auto-replies an hour. |
| Mail from anyone else, or that fails the check | Ignored. It is never shown to the model, and Fane doesn't reply. |
| No address set under **Your email** | Nothing is acted on. |
| The agent wants to email someone other than you | Shows a **Send this email?** card with the full message. Nothing is sent unless you click Send, and an unanswered card cancels after 3 minutes. |
| You ask it about its mail | Uses the `fane_inbox`, `fane_read_email`, `fane_send_email` and `fane_reply_email` tools (separate from `recent_emails`, which reads *your* Thunderbird). Only your own verified messages are visible to it. |

Your address needs Gmail's own verdict: the topmost `Authentication-Results` header, which
mx.google.com adds, has to show `dmarc=pass` for your domain. A forged `From:` line isn't
enough. Other commands: `fane mail accounts`, `fane mail off` and `fane mail status`.

On macOS and Windows there's no equivalent of Online Accounts that avoids storing a
password, so Fane's own mailbox isn't available there yet. Everything else is.

## Claude Code and Codex

The `delegate` tool hands coding and long multi-step tasks to the
[Claude Code](https://claude.com/claude-code) or
[Codex](https://github.com/openai/codex) command-line tools, run non-interactively
(`claude -p …`, `codex exec …`). They use whatever sign-in you've given them, so with
`claude` logged in to your Claude subscription and `codex login` done with your ChatGPT
plan, delegated tasks run on those subscriptions. Fane never sees either credential. If
`ANTHROPIC_API_KEY` or `OPENAI_API_KEY` is set in Fane's environment, the CLIs may use the
API key instead. The commands are `[delegate] claude_cmd` / `codex_cmd` in the config.

## Recommended models

| Model | Ollama tag | Size (Q4) | Architecture | Tool calling |
| --- | --- | --- | --- | --- |
| MiniCPM-V 4.7 35B (default) | `openbmb/minicpm-v4.7:35b-a3b` | ~22 GB | MoE, 3B active | JSON protocol* |
| Gemma 4 31B | `gemma4:31b` | ~20 GB | dense | native |
| Qwen 3.8 27B | `qwen3.8:27b` | ~18 GB | dense, thinking (Fane turns thinking off) | native |

\* Ollama doesn't list tool support for MiniCPM-V 4.7, so Fane detects this and switches to
asking for a JSON action instead. You can force either convention with `[model] tool_mode`.

These models are newer than older Ollama releases, so if a pull or load fails, update Ollama
first. MiniCPM-V 4.7 is the default because it scored best in `fane eval`, and Fane
falls back through the list in order to whichever one you have installed. On a GPU with
less than ~24 GB of memory, part of each model runs on the CPU. The dense models slow down
the most when that happens, while MiniCPM's mixture-of-experts design only activates 3B
parameters per token, so it should hold up better. The
[evaluation](#evaluating-models) measures all of this on your hardware.

You can use a different model for the agent and keep MiniCPM for the background suggestions:

```toml
[model]
name = "gemma4:31b"
suggest_name = "openbmb/minicpm-v4.7:35b-a3b"
```

Both models only stay loaded at once if they fit in memory together. Otherwise Ollama
swaps between them, which costs a reload every time.

## Evaluating models

`fane eval` runs Fane's real prompts and tool definitions against each model, using
synthetic screenshots and mock connectors, and then scores the results:

```bash
fane pause                     # if Fane is running, so it doesn't compete for the GPU
fane eval                      # all three recommended models
fane eval --models qwen3.8:27b --only agent --repeat 3
fane eval --list               # show the cases
```

| Area | What's checked |
| --- | --- |
| Screen understanding | The `activity` sentence names what's on screen: an email, a failing test, a Slack request, a spreadsheet error, an article, photos or an empty desktop. |
| Helpful suggestions | A confident and *specific* suggestion appears where one is clearly useful, such as "reply to Dana" or "fix the KeyError". |
| Staying quiet | No confident suggestion appears on an idle desktop or while browsing photos. |
| Agent tasks | Seven tasks scored on the tool trace: replying at the cursor without preamble, looking up email, opening and summarizing a note, reading an error off the screen, delegating a code fix, rewriting the clipboard, and answering from an MCP calendar tool. Unnecessary actions are penalized, such as delegating or inserting when the user only asked a question. |
| Speed and fit | Median latency for suggestions and for agent tasks, tokens per second, tool calls and tokens per task, hidden reasoning (some models think even when asked not to, which is where the latency goes), how much of the model is on the GPU, and cold load time. |

Each run writes `report.md` (summary, per-case table, failures and a suggested `[model]`
config), `results.json` (full transcripts) and the screenshots it used to
`<state dir>/fane/evals/<time>/` (`~/.local/state` on Linux). The checks are simple keyword
and trace tests. They're good for comparing models, not for proving one is correct, so read
the failures section.

## Configuration

`config.toml` is created on the first run in `~/.config/fane/` (Linux),
`~/Library/Application Support/fane/` (macOS) or `%APPDATA%\fane\` (Windows). Every option
and its default is listed in [`src/config.rs`](src/config.rs). The main ones:

```toml
[model]
name = "openbmb/minicpm-v4.7:35b-a3b"   # falls back to the other recommended/installed models
host = "http://127.0.0.1:11434"         # or pick one in Settings
suggest_name = ""                       # optional faster model for proactive suggestions
num_ctx = 8192                          # keeps more of the model on the GPU

[suggestions]
interval_seconds = 25
min_confidence = 0.65            # raise this for fewer, more certain suggestions
corner = "top-right"

[privacy]
blocked_apps = ["keepassxc", "1password", "bitwarden"]
blocked_titles = ["Private Browsing", "Incognito", "password"]

[hotkey]
binding = "<Control><Super>space" # Linux (GNOME syntax); e.g. "Cmd+Shift+Space" on macOS

[connectors]
notes_dirs = ["~/Notes", "~/Obsidian"]

[agent_mail]
account = "my-assistant@gmail.com"  # from Settings → Online Accounts; empty = email off
owner_email = "me@gmail.com"        # only DMARC-verified mail from you is acted on
remote_can_see_screen = false

[mcp.servers.filesystem]          # any stdio MCP server; its tools become available
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/Documents"]
```

## How it works

```
 GNOME Shell ── fane-helper extension (D-Bus: focused window, screenshot, place window)
      │            (macOS/Windows: native APIs for the same things)
 ┌────┴──────────────── fane (one process) ──────────────────────────────┐
 │ UI thread (winit + egui)            tokio runtime                     │
 │  ├─ Palette (hotkey prompt box)      ├─ Suggester ─ every N s: look →  │
 │  ├─ Overlay (corner cards)           │    changed? → ask model for     │
 │  ├─ Settings                         │    ≤1 suggestion as JSON         │
 │  └─ Tray + hotkey                    ├─ Agent ─ tool loop on Ollama    │
 │                                      ├─ Tools: notes, Thunderbird,     │
 │  `fane …` ──► local socket ─────────►│   windows, clipboard, paste,    │
 │                                      │   MCP, mail, delegate → CLIs    │
 │                                      └─ Ollama discovery, mail polling  │
 └────────────────────────────────────────────────────────────────────────┘
```

The source is laid out by concern: [`src/core.rs`](src/core.rs) (the runtime half),
[`src/ui/`](src/ui/) (windows, tray), [`src/agent.rs`](src/agent.rs),
[`src/suggester.rs`](src/suggester.rs), [`src/llm.rs`](src/llm.rs) (Ollama client),
[`src/discovery.rs`](src/discovery.rs), [`src/connectors/`](src/connectors/),
[`src/mail/`](src/mail/), [`src/platform/`](src/platform/) (everything OS-specific) and
[`src/evals/`](src/evals/).

On Linux, Wayland deliberately stops apps from seeing or controlling each other, so Fane
works around it in a few places:

- **Seeing the screen and the focused window.** The bundled GNOME Shell extension
  ([`gnome-extension/`](gnome-extension/)) exposes these over D-Bus. Without it, Fane falls back to the
  xdg-desktop-portal screenshot API, which may ask for permission, and it can't tell which
  window is focused.
- **Typing into other apps.** Fane copies the text to the clipboard and presses
  <kbd>Ctrl</kbd>+<kbd>V</kbd> (or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> in terminals) through a virtual keyboard on
  `/dev/uinput`. ydotool and xdotool are fallbacks. If none of these work, the text stays
  on the clipboard and Fane tells you to paste it yourself.
- **Placing the corner card.** Fane runs its windows through XWayland so it can position
  them and keep them on top. On native Wayland the extension places them instead.
- **The global hotkey.** This is a GNOME custom shortcut that runs `fane toggle`.

On macOS and Windows, Fane uses the system's own APIs for screenshots, the focused window,
pasting and the global hotkey.

## Security notes

- The helper extension lets any process in your session read window titles and take
  screenshots. Screenshots can only be written to `$XDG_RUNTIME_DIR/fane/`. Any program
  running as you could already do most of this through other means, but you should know
  about it.
- Pasting on Linux needs write access to `/dev/uinput`. Some distros grant this to the
  logged-in user, and some need a udev rule (the package ships an example in
  `/usr/share/doc/fane/examples/`) or the `input` group. That access also lets a process
  type anywhere.
- Fane's mailbox is a way in from the internet, so it only takes instructions from your
  own address, and only when Google's DMARC check passes. Emailed requests get a reduced set
  of tools, and every message to anyone else needs your click. The Online Accounts token
  covers the whole mailbox, so give Fane an account of its own, not your personal one.
- With a remote Ollama server, your prompts and screenshots cross the network over plain
  HTTP, and Ollama itself has no login, so anyone who can reach the port can use it. Only
  do this on a network you trust, or put the server behind a VPN like Tailscale (type its
  address by hand; the scan only covers your local subnets).
- Claude Code and Codex run non-interactively with their default permission settings. To
  give them more autonomy, change `[delegate] claude_cmd` / `codex_cmd`.

## Development

```bash
cargo test                     # unit tests (no model, GPU or desktop needed)
cargo clippy --all-targets
cargo run -- run -v            # run from the checkout with debug logging
```

`FANE_DEBUG_SNAPSHOT=<dir>` makes each window save what it renders as a PNG, which helps when
debugging the UI over SSH or under Wayland.

Releases are built by [`.github/workflows/release.yml`](.github/workflows/release.yml) when
a `v*` tag is pushed: `.deb` packages (amd64, arm64) with `cargo deb`, a universal macOS
`.dmg` ([`packaging/macos/`](packaging/macos/)), a Windows installer and zip
([`packaging/windows/`](packaging/windows/)), and the signed apt repository on GitHub Pages
([`packaging/apt/make-repo.sh`](packaging/apt/make-repo.sh)). The apt repository needs a
one-time setup described at the top of that workflow.

## Status and roadmap

Version 0.2 is a rewrite of the original Python/Qt version in Rust, and adds macOS and
Windows. These have been tested on Ubuntu: the agent loop against a local model, Ollama
discovery, the UI windows, the IPC, the connectors (including a live MCP server), the mail
logic, the eval harness, and installing the `.deb` from the apt repository. The macOS and
Windows builds compile and pass the unit tests in CI but haven't been tried on a real
desktop yet, and neither have the GNOME extension and live Gmail.

Ideas for next steps:

- Read the focused text field through AT-SPI / accessibility APIs, which is more precise
  than the screenshot.
- Fane's own mailbox on macOS and Windows.
- Built-in Evolution, GNOME Calendar and browser-tab connectors.
- Streaming answers, and cancelling a running task.
- Learn from which suggestions you accept and dismiss.
