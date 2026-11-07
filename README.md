# Fane

An unobtrusive, context-aware desktop assistant for Ubuntu (GNOME). It's a local-first
take on [Fazm](https://fazm.ai) for Linux.

Fane watches your screen and keeps prompting itself with "what would help right now?".
When it has a specific, useful idea, a small card slides into the notification corner
with a ready-to-run prompt. Click **Run** to do it, **Edit** to tweak the prompt first,
or ignore it and it fades away. Press a hotkey any time to type your own request.

- **Local model.** Uses a vision model served by [Ollama](https://ollama.com) (see
  [Recommended models](#recommended-models)). No screen content leaves your machine unless
  you delegate a task.
- **Inserts at your cursor.** Drafts, replies and rewrites are pasted where you're already
  typing. Images work too.
- **Talks to your apps.** It reads your notes folders and Thunderbird mail directly, and any
  app that has an MCP server (Gmail, calendars, Slack, Obsidian, the filesystem and so on).
- **Delegates the hard stuff.** Coding and long multi-step tasks go to Claude Code or Codex.
- **Stays out of the way.** It only re-reads the screen when something changed, suggests at
  most one thing at a time, and backs off further each time you dismiss a suggestion. It
  never looks at password managers or private browser windows.

## Install

Requirements: Ubuntu 24.04+ / GNOME 45+, Python 3.12, [uv](https://docs.astral.sh/uv/),
and Ollama.

```bash
ollama pull gemma4:31b        # or one of the other recommended models below
sudo apt install libxcb-cursor0
git clone git@github.com:salsicha/Fane.git && cd Fane
uv tool install -e .          # puts `fane` on your PATH
```

Then set it up for your desktop:

```bash
fane install-extension        # GNOME helper (log out and back in once on Wayland)
fane setup-shortcut           # Ctrl+Super+Space opens the prompt box
fane autostart on             # optional: start at login
fane doctor --screenshot      # see what works on this machine
fane                          # run it
```

## Using it

| Action | How |
| --- | --- |
| Ask for something | Hotkey (default <kbd>Ctrl</kbd>+<kbd>Super</kbd>+<kbd>Space</kbd>), type, <kbd>Enter</kbd> |
| Put the answer where you're typing | **Insert at cursor** or <kbd>Ctrl</kbd>+<kbd>Enter</kbd> |
| Accept a suggestion | **Run** on the corner card (or **Edit** to change the prompt first) |
| Get a suggestion now | Tray menu → *Suggest something now*, or `fane suggest` |
| Pause suggestions | Tray menu, or `fane pause` / `fane resume` |
| Script it | `fane ask "summarize this page"`, `fane toggle`, `fane status`, `fane quit` |

Example requests: *"reply to this saying Thursday works"*, *"what did Dana email me about
the review?"*, *"find my notes on the Q3 plan and summarize them"*, *"fix the failing test
in ~/code/api"* (which gets delegated to Claude Code or Codex).

## Recommended models

| Model | Ollama tag | Size (Q4) | Architecture | Tool calling |
| --- | --- | --- | --- | --- |
| Gemma 4 31B | `gemma4:31b` | ~20 GB | dense | native |
| Qwen 3.8 27B | `qwen3.8:27b` | ~18 GB | dense, thinking (Fane turns thinking off) | native |
| MiniCPM-V 4.7 35B | `openbmb/minicpm-v4.7:35b-a3b` | ~22 GB | MoE, 3B active | JSON protocol* |

\* Ollama doesn't list tool support for MiniCPM-V 4.7, so Fane detects this and switches to
asking for a JSON action instead. You can force either convention with `[model] tool_mode`.

These models are newer than older Ollama releases, so if a pull or load fails, update Ollama
first. Gemma 4 is the default, and Fane
falls back through the list in order to whichever one you have installed. On a GPU with
less than ~24 GB of memory, part of each model runs on the CPU. The dense models slow down
the most when that happens, while MiniCPM's mixture-of-experts design only activates 3B
parameters per token, so it should hold up better. The
[evaluation](#evaluating-models) measures all of this on your hardware.

You can use one model for the agent and a faster one for the background suggestions:

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
ollama pull gemma4:31b && ollama pull qwen3.8:27b && ollama pull openbmb/minicpm-v4.7:35b-a3b
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
`~/.local/state/fane/evals/<time>/`. The checks are simple keyword and trace tests. They're
good for comparing models, not for proving one is correct, so read the failures section.

## Configuration

`~/.config/fane/config.toml` is created on the first run. Every option and its default is
listed in [`src/fane/config.py`](src/fane/config.py). The main ones:

```toml
[model]
name = "gemma4:31b"              # falls back to the other recommended/installed models
suggest_name = ""                # optional faster model for proactive suggestions
num_ctx = 8192                   # keeps more of the model on the GPU

[suggestions]
interval_seconds = 25
min_confidence = 0.65            # raise this for fewer, more certain suggestions
corner = "top-right"

[privacy]
blocked_apps = ["keepassxc", "1password", "bitwarden"]
blocked_titles = ["Private Browsing", "Incognito", "password"]

[connectors]
notes_dirs = ["~/Notes", "~/Obsidian"]

[mcp.servers.filesystem]          # any stdio MCP server; its tools become available
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/Documents"]
```

## How it works

```
 GNOME Shell ── fane-helper extension (D-Bus: focused window, screenshot, place window)
      │
 ┌────┴──────────── fane (one process) ──────────────────────────────┐
 │ Qt thread                         asyncio thread                  │
 │  ├─ Palette (hotkey prompt box)    ├─ Suggester ─ every N s: look → │
 │  ├─ Overlay (corner cards)         │    changed? → ask model for    │
 │  ├─ Tray + IPC socket ◄─ `fane …`  │    ≤1 suggestion as JSON        │
 │  └─ Paste at cursor                ├─ Agent ─ tool loop on Ollama   │
 │      (clipboard + uinput Ctrl+V)   └─ Tools: notes, Thunderbird,    │
 │                                       windows, clipboard, MCP,      │
 │                                       delegate → claude / codex     │
 └────────────────────────────────────────────────────────────────────┘
```

Wayland deliberately stops apps from seeing or controlling each other, so Fane works
around it in a few places:

- **Seeing the screen and the focused window.** The bundled GNOME Shell extension
  ([`gnome-extension/`](gnome-extension/)) exposes these over D-Bus. Without it, Fane falls back to the
  xdg-desktop-portal screenshot API, which may ask for permission, and it can't tell which
  window is focused.
- **Typing into other apps.** Fane copies the text to the clipboard and presses
  <kbd>Ctrl</kbd>+<kbd>V</kbd> (or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> in terminals) through a virtual keyboard on
  `/dev/uinput`. ydotool and xdotool are fallbacks. If none of these work, the text stays
  on the clipboard and Fane tells you to paste it yourself.
- **Placing the corner card.** Fane runs its windows through XWayland, which needs
  `libxcb-cursor0`, so it can position them. On native Wayland the extension places them
  instead.
- **The global hotkey.** This is a GNOME custom shortcut that runs `fane toggle`.

## Security notes

- The helper extension lets any process in your session read window titles and take
  screenshots. Screenshots can only be written to `$XDG_RUNTIME_DIR/fane/`. Any program
  running as you could already do most of this through other means, but you should know
  about it.
- Pasting needs write access to `/dev/uinput`. Some distros grant this to the logged-in
  user, and some need a udev rule or the `input` group. That access also lets a process
  type anywhere.
- Claude Code and Codex run non-interactively with their default permission settings. To
  give them more autonomy, change `[delegate] claude_cmd` / `codex_cmd`.

## Status and roadmap

This is the first version. These have been tested: the suggestion and agent loop against a
local model (including MCP tool calls), the notes and mail connectors, the UI, the IPC and
the eval harness. The GNOME extension hasn't been loaded on a live session yet, and the
three recommended models haven't been evaluated yet.

Ideas for next steps:

- Read the focused text field through AT-SPI, which is more precise than the screenshot.
- Built-in Evolution, GNOME Calendar and browser-tab connectors.
- Streaming answers, and cancelling a running task.
- Draw the cards natively in GNOME Shell so they don't depend on XWayland.
- Learn from which suggestions you accept and dismiss.
