# Fane

An unobtrusive, context-aware desktop assistant for Linux (GNOME), macOS and Windows. It's
a local-first take on [Fazm](https://fazm.ai), written in Rust.

Fane watches your screen and keeps prompting itself with "what would help right now?".
When it has a specific, useful idea, a small card slides into the corner of the screen
with a ready-to-run prompt. Click **Run** to do it, **Edit** to tweak the prompt first,
or ignore it and it fades away. Press a hotkey any time to type your own request.

- **Local models.** Runs its own models with [llama.cpp](https://github.com/ggml-org/llama.cpp)
  (Gemma 4 26B every day, Qwen 3.8 27B for coding; see [Recommended models](#recommended-models)),
  reusing anything [Ollama](https://ollama.com) already downloaded. No screen content leaves
  your network unless you delegate a task.
- **Makes images and videos** with [stable-diffusion.cpp](https://github.com/leejet/stable-diffusion.cpp)
  (Z-Image Turbo and Wan 2.2). See [Images and video](#images-and-video).
- **Shares the big GPU.** Choose which GPU does what (chat on the integrated GPU, coding and
  images on the discrete one), and let your other computers use this one's GPU: Fane on a
  laptop can send coding or image requests to Fane on the desktop with the GPU, which runs
  whichever model the request needs. See [Models, GPUs and other computers](#models-gpus-and-other-computers).
- **Inserts at your cursor.** Drafts, replies and rewrites are pasted where you're already
  typing. Images work too.
- **Works with your files.** It can find, search and read files in the folders you allow,
  and create, edit, move or trash them after you confirm each change.
- **Talks to your apps.** It reads your notes folders and Thunderbird mail directly, and any
  app that has an MCP server (Gmail, calendars, Slack, Obsidian and so on).
- **Knows where things are.** It keeps an index of your files and of every tool it can use, so
  finding a document or the right tool takes milliseconds.
- **Remembers how you like things done.** Tell it once ("from now on, sign my emails *Best,
  Alex*") and it follows that preference until you change it.
- **Has its own email.** Give it an account of its own (Gmail, Fastmail, iCloud, or any IMAP
  provider, with an app password kept in the system keychain). You can email it tasks from your
  own address, and it replies. Mail from anyone else is ignored.
- **Delegates the hard stuff.** Coding and long multi-step tasks go to a command-line coding
  agent: Qwen Code or CodeGem on the local models, or Claude Code and Codex on your own
  subscription (Claude also plans and reviews the local agents' work).
- **Stays out of the way.** It only re-reads the screen when something changed, suggests at
  most one thing at a time, and backs off further each time you dismiss a suggestion. It
  never looks at password managers or private browser windows.

## Install

Fane runs its models with llama.cpp. If something's missing, Fane sets it up for you: the
Windows installer does it right away (an option that's on by default, with optional extras
for coding, images and video), and on Linux and macOS the first launch shows a **Set up
Fane?** card. You can also run `fane setup` in a terminal, or use **Settings… → Models →
Download**. Setup:

- downloads the llama.cpp build for your GPU (NVIDIA CUDA, Vulkan for other GPUs, or Metal on
  a Mac) from llama.cpp's GitHub releases into `~/.fane/runtimes`;
- uses the copy of a model that Ollama has already downloaded, if it has one (no second
  copy), and otherwise downloads it from Hugging Face into `~/.fane/models` (Gemma 4 26B is
  about 16 GB), resuming if it's interrupted;
- with `fane setup --code`, `--images`, `--video` or `--all`, also gets Qwen 3.8 27B,
  stable-diffusion.cpp and the image and video models.

It does nothing for what's already there, and it doesn't change Ollama. If Fane was using
Ollama before (`[model] host` set to it), it keeps doing so until you accept the **Run models
with llama.cpp?** card or run `fane setup`; your models stay where Ollama put them.

Optionally install the [coding agents](#coding-agents): `npm install -g @qwen-code/qwen-code`
(Qwen Code) and `uv tool install superqode` (CodeGem).

### Ubuntu 24.04+ (apt)

```bash
curl -fsSL https://salsicha.github.io/Fane/fane-archive-keyring.gpg | sudo tee /usr/share/keyrings/fane-archive-keyring.gpg >/dev/null
echo "deb [signed-by=/usr/share/keyrings/fane-archive-keyring.gpg] https://salsicha.github.io/Fane stable main" | sudo tee /etc/apt/sources.list.d/fane.list
sudo apt update && sudo apt install fane
```

Packages are built for amd64 and arm64. The `.deb` files are also attached to each
[GitHub release](https://github.com/salsicha/Fane/releases) (`sudo apt install ./fane_*.deb`).
apt doesn't run Fane's setup itself (a multi-gigabyte download inside apt would hold its lock
and break offline installs), so the first launch offers it instead. Then set it up for your
desktop:

```bash
fane setup                    # llama.cpp and Gemma 4 26B (add --code --images --video)
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
portable `.zip` from the [releases](https://github.com/salsicha/Fane/releases). The installer's
**Get llama.cpp and Gemma 4** option (on by default) starts Fane when it's done and sets them
up, along with any of the optional coding, image and video models you tick. Fane lives
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
in ~/code/api"* (which gets delegated to a [coding agent](#coding-agents)).

## Settings

**Settings…** in the tray menu covers the things you'd change most:

- **Models**: what runs where. For each kind of work (everyday, coding, images, video) pick
  this computer and which GPU, another computer, or Claude Code / Codex; download what's
  missing; share this computer's models with your other computers; and run them as a
  background service. See [Models, GPUs and other computers](#models-gpus-and-other-computers).
- **Other computers**: other Fanes, stable-diffusion.cpp servers and Ollama servers on your
  network, found automatically.
- **Files**: the folders Fane may use (see [Files](#files)).
- **Your email**: your personal address. Only mail from it is read, acted on and answered.
- **Fane's own email account**: sign in with an address and app password (or, on Linux,
  pick an Online Accounts account), and learn your provider's sender check.

Everything else lives in the config file (**Edit config file…**, see
[Configuration](#configuration)).

## Files

Fane has built-in tools for the folders you allow (`[files] roots`, your home folder by
default; change it in **Settings… → Files**):

| Tool | What it does | Asks first |
| --- | --- | --- |
| `list_files` | A folder's files and subfolders, with sizes and dates | no |
| `find_files` | Files by name or wildcard (`*.pdf`, `report*`), newest first | no |
| `search_files` | Lines containing some text, across text files | no |
| `read_file` | A text file's contents (long files a page at a time) | no |
| `write_file` | Create a file, replace it, or add to the end | yes |
| `edit_file` | Replace one exact passage in a file | yes |
| `move_file` | Move or rename a file or folder | yes |
| `delete_file` | Move a file or folder to the system trash | yes |

Changes show a card with the path and a preview (or the lines being replaced); nothing
happens unless you click, and an unanswered card cancels after 3 minutes. Hidden files and
folders (`.ssh`, `.config`, browser profiles, ...) and secret-looking files (`*.kdbx`,
`id_rsa*`, `*.pem`, `*password*`, ... see `[files] blocked`) are never opened, symlinks can't
lead outside the allowed folders, and searches skip `node_modules`, `target` and similar.
Requests emailed to Fane can read files but never change them.

These are built into Fane rather than provided by an MCP server so that they follow its
confirmation and privacy rules on every OS without needing Node.js.

## Index and preferences

**The index** knows where things are, so Fane doesn't have to search from scratch:

- **Files**: the name and folder of every file in the allowed folders, and the text of
  documents (Markdown, text, CSV, HTML, ...; code is left to the coding agents). `find_files`
  and `search_files` use it. Hidden files, secrets, and tool installs and generated folders
  (`anaconda3`, `node_modules`, `target`, ...) are left out. On the test machine, 421,000 files
  were indexed in 21 seconds (178 MB), a re-check took 5 seconds, and a search about 0.04 s.
- **Capabilities**: every tool Fane can use (built-in, MCP servers, Fane's email) and every
  installed coding agent. `find_tools` answers "where can this be done?". With more than 24
  tools, the model sees the core ones and those relevant to the request in full, and the rest
  by name, which keeps the prompt small for a local model.

The index is corrected when it stops being accurate: a re-check every 30 minutes re-indexes
files whose size or date changed and drops deleted ones (and anything outside the allowed
folders); a watcher catches changes as they happen where the OS allows; Fane's file tools
update it when they change something; every search result is re-checked on disk before it's
returned; and capabilities are replaced as soon as the set of tools changes. `fane index`
updates it now (`--query` to look something up); **Settings → Rebuild index** does the same.

**Preferences** are standing instructions you give about how something should be done:
"reply to Dana formally", "save invoices to ~/Documents/Invoices", "use Qwen Code for Python".
Fane saves one when you say "from now on…", "always…", "I prefer…" (a card confirms it), and
follows the relevant ones in every request and in its suggestions. Only your own words are
saved, never anything from emails, web pages or files. A preference is removed when it's no
longer accurate:

- a new preference on the same topic (or one you say changes it) replaces the old one;
- one that refers to a folder that no longer exists, or a tool or coding agent that's no longer
  configured, is dropped, and Fane tells you which and why (an MCP server that's configured but
  not running right now doesn't count as gone);
- you can forget one by asking, in **Settings → Preferences**, or with `fane prefs --forget ID`
  (`fane prefs` lists them).

Both live in a local SQLite database in Fane's state folder (`~/.local/state/fane/fane.db` on
Linux) and never leave your machine.

## Models, GPUs and other computers

Fane runs a small model server inside itself (on `127.0.0.1:11500`, speaking the OpenAI API)
that starts each model when it's first needed, stops it after 15 idle minutes, and routes every
request by a few lines of config. The coding agents use the same server, so they run on the
same models.

**Which GPU does what.** `fane gpus` lists the GPUs (for example `cuda:0` for an NVIDIA card,
`vulkan:0` for an Intel or AMD one), and each kind of work can use a different one:

```toml
[runtime]
chat_gpu = "vulkan:0"   # Gemma 4 on the integrated GPU (shared memory)
code_gpu = "cuda:0"     # Qwen 3.8 on the NVIDIA card...
image_gpu = "cuda:0"    # ...taking turns with image generation
video_gpu = "cuda:0"
```

(or **Settings → Models**, or `fane gpus chat vulkan:0`). `"auto"`, the default, is the most
capable GPU. Models on the same GPU share it: if they fit in its memory together they stay
loaded together, and if not, the one that's needed now replaces the least recently used idle
one. A model is never stopped while it's answering; the new request waits for it.

**Where each request goes.** `[routes]` lists, for each kind of work, where to send it, first
choice first; the first one that can take it does:

```toml
[routes]
chat  = ["local"]
code  = ["local", "claude", "codex"]   # the local coding model, else Claude Code, else Codex
image = ["local", "codex"]             # Codex makes images too
video = ["local"]
```

`"local"` is this computer, `"claude"` and `"codex"` are the command-line agents on your
subscription, and any other name is a computer or server under `[remotes]`. Change them in
**Settings → Models** or with `fane route code nuc claude`. `fane models` shows the models
this computer can run and the routes.

**Using another computer's GPU.** On the computer with the GPU (say a NUC with a V100), turn on
**Settings → Models → Let other computers on my network use this computer's models** (or
`fane share on`). Fane then listens on the network and shows a token. On the other computer
(say a Mac), **Settings → Other computers** lists the NUC; select it, paste the token and
click **Add** (or `fane remote add nuc.local --token <token>`). Then send work to it:

```toml
[remotes.nuc]
url = "http://nuc.local:11500"
token = "…"

[routes]
chat  = ["local"]                 # the Mac's own Gemma 4
code  = ["nuc", "local", "claude"]
image = ["nuc"]
video = ["nuc"]
```

A coding request from the Mac then runs Qwen 3.8 on the V100; an image request swaps it out
for Z-Image Turbo, and the next coding request swaps it back. To keep the NUC's models
available without Fane's window open, tick **Keep the models available when Fane isn't open**
(or `fane service on`): a systemd user service on Linux (it keeps running while you're signed
out if your system allows lingering), a launchd agent on macOS, or a sign-in entry on
Windows. It runs `fane serve`, which is just the model server.

**Other servers.** The same list finds stable-diffusion.cpp servers (add one and route
`image` / `video` to it; Fane loads the image or video model there if the server has its
files) and Ollama servers (choosing one makes it the server for everyday chat, `[model]
host`, as before). Ollama only listens on localhost by default; to share it, set
`OLLAMA_HOST=0.0.0.0:11434` on that machine.

```bash
fane servers scan                       # other Fanes, Ollama and stable-diffusion.cpp servers
fane remote add 192.168.1.20 --token …  # another Fane
fane servers use 192.168.1.30:11434     # an Ollama server for chat
```

## Apps

Fane can work inside desktop apps through their MCP servers. **Settings → Apps → Set up** (or
asking Fane *"set up Blender"*, or `fane apps setup blender`) does the whole job after you
confirm a card that lists each step:

1. installs the app if it's missing: from Flathub just for you (no password) or as a snap on
   Linux, with Homebrew on macOS, with winget on Windows;
2. gets the MCP server's runner: uv (downloaded into `~/.fane/bin` if you don't have it) or
   Node.js;
3. installs the app's MCP add-on and turns it on (asking the app itself where its add-ons
   go);
4. adds the server under `[mcp.servers]` and connects to it right away.

From then on you can ask for things like *"make a low-poly tree in Blender"*. If the app isn't
open, Fane opens it the first time one of its tools is used and waits for its add-on.

| App | What Fane can do | MCP server |
| --- | --- | --- |
| Blender | make and edit 3D models, scenes, materials and renders | [mcp-for-blender](https://github.com/ahujasid/mcp-for-blender) 2.2.7 |
| FreeCAD | design precise parts and assemblies, run FEM analyses | [freecad-mcp](https://github.com/neka-nat/freecad-mcp) 0.1.30 (its RPC server set to start with FreeCAD) |
| Web browser | browse sites, fill in forms, read pages, in its own browser profile | [Playwright MCP](https://github.com/microsoft/playwright-mcp) 0.0.85 (needs Node.js and Chrome) |

`fane apps` lists them, and **Remove** (or `fane apps remove blender`) disconnects one and leaves
the app installed. The servers are pinned to the versions above. Any other MCP server can
still be added by hand under `[mcp.servers]`; `launch` and `wait_port` make Fane open its app
on demand there too.

These add-ons can run code inside the app (Blender's and FreeCAD's both have an
"execute code" tool), so whatever the model asks for runs with your permissions. They only
listen on this computer. Emailed requests can't set up apps.

## Images and video

With images set up (`fane setup --images`, or the installer option), the assistant gets a
`generate_image` tool: ask *"make an image of a reading nook with a cat, warm light"* and it
saves a PNG to `~/Pictures/Fane` (`[media] dir`) and opens it. `generate_video` makes short
clips (`fane setup --video`; a 2-second 832×480 clip, optionally starting from an image).
Both run on the GPU in `image_gpu` / `video_gpu`, or wherever `[routes]` sends them; with no
image model anywhere and Codex installed, Codex makes the image instead.

| | Model | Download | GPU memory |
| --- | --- | --- | --- |
| Images | [Z-Image Turbo](https://huggingface.co/Tongyi-MAI/Z-Image-Turbo) (Q8, 9 steps) | ~11 GB | ~11 GB at 1024×1024 |
| Video | [Wan 2.2 TI2V 5B](https://huggingface.co/Wan-AI/Wan2.2-TI2V-5B) (Q8) | ~13 GB | more; the VAE is the hungry part |

stable-diffusion.cpp's own Linux builds need a newer C library than Ubuntu 24.04 has and
don't include CUDA, so on Linux Fane downloads its own builds (CUDA 12 and Vulkan, made by
[`sd-runtime.yml`](.github/workflows/sd-runtime.yml) and attached to the
[`sd-master-a3c49b1`](https://github.com/salsicha/Fane/releases/tag/sd-master-a3c49b1)
release), or compiles it on your computer as a last resort. On a Tesla V100, a 1024×1024
image takes about 11 seconds with CUDA (about a minute with Vulkan).

## Fane's own email

Fane can have an email address of its own, on Linux, macOS and Windows. You email it tasks
from your own address and it replies; mail from anyone else is ignored. It reads its mailbox
itself over IMAP and sends over SMTP; it isn't a mail client for your own mail (it reads that
through the Thunderbird connector or an MCP server).

1. Make an account just for Fane (for example a new Gmail address), not your personal one,
   and an **app password** for it: Gmail at <https://myaccount.google.com/apppasswords>
   (after turning on 2-Step Verification), Fastmail, iCloud and Yahoo in their security
   settings.
2. In Fane's **Settings → Email**, enter the address and the app password and click
   **Sign in** (or `fane mail login agent@gmail.com`, which asks for the password). Fane checks
   both servers, then keeps the password in the system keychain (macOS Keychain, Windows
   Credential Manager, or the Secret Service on Linux), never in the config file. Gmail,
   Fastmail, iCloud and Yahoo servers are filled in; for other providers, enter the IMAP and
   SMTP servers.
3. Enter **Your email** (or `fane mail owner you@gmail.com`).

On Linux you can instead pick a Google account from GNOME's Online Accounts (no password at
all: Fane asks GNOME for a short-lived token for that one account). Outlook.com and Hotmail
don't allow app passwords for other mail programs, so Fane can't sign in to them yet.

**Knowing it's really you.** A `From:` line is easy to forge, so Fane only acts on a message
when your mail provider's own server stamped it (the topmost `Authentication-Results`
header) with a DMARC pass for your address's domain. Fane knows which server that is for
Gmail (`mx.google.com`) and Fastmail. For other providers it learns it once: email Fane from
your address, then click **Learn** under Settings → Email (or `fane mail learn`). It only
accepts a server that stamped your message within the last 15 minutes, confirmed DMARC for
you, and also stamped the other recent messages in the inbox. Until then, no email is acted
on. You can also set it yourself as `[agent_mail] auth_server`.

How Fane handles mail:

| Situation | What Fane does |
| --- | --- |
| Mail from your address that passes your provider's DMARC check | Runs it as a request and replies in the same thread. It can't type on your desktop, delegate to coding agents, send other mail or see your screen (all configurable), and it sends at most 20 auto-replies an hour. |
| Mail from anyone else, or that fails the check | Ignored. It is never shown to the model, and Fane doesn't reply. |
| No address set under **Your email** | Nothing is acted on. |
| The agent wants to email someone other than you | Shows a **Send this email?** card with the full message. Nothing is sent unless you click Send, and an unanswered card cancels after 3 minutes. |
| You ask it about its mail | Uses the `fane_inbox`, `fane_read_email`, `fane_send_email` and `fane_reply_email` tools (separate from `recent_emails`, which reads *your* Thunderbird). Only your own verified messages are visible to it. |

Other commands: `fane mail status`, `fane mail logout`, and on Linux `fane mail accounts` /
`fane mail use` for Online Accounts.

## Recommended models

Fane uses two local models, each paired with a command-line coding agent ("harness") that
coding tasks are handed to:

| | Model | Name | Size (Q4) | Used for | Coding harness |
| --- | --- | --- | --- | --- | --- |
| Everyday (default) | Gemma 4 26B (MoE, 4B active) | `gemma4:26b` | ~17 GB | suggestions, requests, tools, vision | [CodeGem](https://shashikantjagtap.net/introducing-codegem-a-gemini-and-gemma-coding-harness-in-superqode/) (SuperQode) |
| Coding | Qwen 3.8 27B | `qwen3.8:27b` | ~17 GB | coding tasks | [Qwen Code](https://github.com/QwenLM/qwen-code) (preferred) |

Both run with llama.cpp's speculative decoding (each model's MTP draft), which roughly
doubles generation speed. On a Tesla V100 (32 GB), Gemma 4 26B generates about 130
tokens/s (the same as Ollama) and Qwen 3.8 27B about 70 tokens/s (33 without its draft); on the NUC's Intel Iris
Xe (Vulkan, shared memory) Gemma 4 runs at about 12 tokens/s.

On `fane eval` both models pass all 11 agent tasks (30/30 checks) and understand all seven
screens. On the V100 a median agent task takes 5.8 s with Gemma 4 and 10.7 s with Qwen 3.8,
roughly twice as long as under Ollama: generation is as fast or faster, but llama.cpp
processes long prompts (Fane's tool list, screenshots) more slowly on this older GPU, where
it can't use flash attention for these models. Each harness fixed the eval's failing test:
CodeGem in 7 s and Qwen Code in about 9 minutes (47 requests reading the project, mostly
prompt processing).

Any other model works too: anything Ollama has downloaded (by its Ollama name), or a GGUF
file:

```toml
[model]
name = "gpt-oss:20b"        # uses Ollama's download

[runtime.models.my-model]   # or a file / Hugging Face download
file = "hf:user/repo/model-Q4_K_M.gguf"
mmproj = "hf:user/repo/mmproj.gguf"   # optional: lets it see screenshots
```

If the configured model isn't available, Fane falls back through `[model] fallbacks` in
order. You can use a different model just for the background suggestions with
`[model] suggest_name`; both stay loaded if they fit on the GPU together.

## Coding agents

The `delegate` tool hands coding and long multi-step tasks to a command-line coding agent,
run non-interactively in `[delegate] cwd` (your home folder by default). Fane offers the
model every harness that's installed, best first: with `[delegate] default = "auto"`, Qwen
Code on the coding model, then CodeGem, then Claude Code and Codex in the order of
`[routes] code`.

| Name | Install | Runs |
| --- | --- | --- |
| `qwen` | `npm install -g @qwen-code/qwen-code` ([Qwen Code](https://github.com/QwenLM/qwen-code)) | `qwen --auth-type openai --openai-base-url <fane>/v1 --openai-api-key fane -m qwen3.8:27b "<task>"` |
| `codegem` | `uv tool install superqode` | `superqode harness run codegem "<task>" --provider llamacpp --model gemma4:26b --working-dir <dir>` |
| `claude` | [Claude Code](https://claude.com/claude-code) | `claude -p --permission-mode auto "<task>"` |
| `codex` | [Codex](https://github.com/openai/codex) | `codex exec --skip-git-repo-check --approve-for-me "<task>"` |

The local harnesses talk to Fane's model server, so they follow `[routes]` too: with
`code = ["nuc", …]` Qwen Code on the laptop runs on the NUC's GPU. Nothing leaves your
network. CodeGem comes from [SuperQode](https://pypi.org/project/superqode/), a Python tool;
Fane itself is a single Rust binary with no Python, so install it separately if you want it
(`fane doctor` shows what's missing and how to install it).

### Planning and review by Claude

When Claude Code is installed and signed in (or Codex, if Claude Code isn't), Fane uses it as a
senior engineer for coding work it hands to a local harness:

1. **Plan.** Claude reads the project (read-only: `claude -p --permission-mode plan`) and
   writes a concrete plan: which files to change, edge cases, how to verify.
2. **Implement.** The local harness (Qwen Code by default) makes the change, following the plan.
3. **Review.** Fane takes a snapshot of the folder before and after, and Claude reviews exactly
   that diff against the task and the plan.
4. **Fix.** If Claude finds problems, the harness fixes them, and Claude reviews again, for up
   to `[delegate] review_rounds` rounds (2 by default).

The model gets the plan, the harness's output and the final verdict back, and the progress
card shows each step. Snapshots use a private git index (inside a repository) or a throwaway
repository beside the folder (for small plain folders), so your own git state isn't touched;
your home folder and folders with more than 5,000 files outside a repository are planned but
not reviewed. If Claude isn't signed in or fails, the harness works on its own and the result
says so. `[delegate] advisor` is `"auto"` (Claude Code, else Codex), `"claude"`, `"codex"` or
`"off"`. Tasks given to Claude Code or Codex directly don't get an extra reviewer.

Claude Code and Codex use whatever sign-in you've given them, so with `claude` logged in to
your Claude subscription and `codex login` done with your ChatGPT plan, delegated tasks run
on those subscriptions. Fane never sees either credential. If `ANTHROPIC_API_KEY` or
`OPENAI_API_KEY` is set in Fane's environment, the CLIs may use the API key instead.

Every harness can be changed or added in the config. `{task}`, `{model}` (the everyday
model), `{code_model}`, `{base_url}` (Fane's model server, ending in `/v1`) and `{cwd}` are
filled in; without a `{task}` argument the task goes last:

```toml
[delegate.agents.qwen]                  # let Qwen Code edit files on its own
cmd = ["qwen", "--auth-type", "openai", "--openai-base-url", "{base_url}", "--openai-api-key", "fane",
       "-m", "{code_model}", "--approval-mode", "auto-edit", "{task}"]

[delegate.agents.aider]                 # a harness Fane doesn't know about
cmd = ["aider", "--model", "openai/{code_model}", "--openai-api-base", "{base_url}", "--yes", "--message", "{task}"]
description = "Aider, editing files in the working directory"
```

`fane doctor` lists which harnesses are installed and which is the default.

## Evaluating models

`fane eval` runs Fane's real prompts and tool definitions against each model, using
synthetic screenshots and mock connectors, and then scores the results:

```bash
fane pause                     # if Fane is running, so it doesn't compete for the GPU
fane eval                      # both recommended models
fane eval --models qwen3.8:27b --only agent --repeat 3
fane eval --list               # show the cases
```

| Area | What's checked |
| --- | --- |
| Screen understanding | The `activity` sentence names what's on screen: an email, a failing test, a Slack request, a spreadsheet error, an article, photos or an empty desktop. |
| Helpful suggestions | A confident and *specific* suggestion appears where one is clearly useful, such as "reply to Dana" or "fix the KeyError". |
| Staying quiet | No confident suggestion appears on an idle desktop or while browsing photos. |
| Agent tasks | Eleven tasks scored on the tool trace, with 40 tools available (including 20 unrelated MCP tools, so tool selection matters): replying at the cursor without preamble, looking up email, opening and summarizing a note, reading an error off the screen, delegating a code fix, rewriting the clipboard, finding a figure in the right file, following a saved preference, saving a new preference, finding the right MCP tool for a question, and answering from an MCP calendar tool. Unnecessary actions are penalized, such as delegating or inserting when the user only asked a question. |
| Speed and fit | Median latency for suggestions and for agent tasks, tokens per second, tool calls and tokens per task, hidden reasoning (some models think even when asked not to, which is where the latency goes), how much of the model is on the GPU, and cold load time. |

`fane eval --harness --advisor` runs that task with Claude (or Codex) planning and reviewing,
as Fane does in normal use; it uses that subscription.

`fane eval --harness` (or `--only harness`) also gives each model's paired coding harness a
small Python project whose test fails with a KeyError, asks it to fix the bug, and then runs
the tests itself; it checks that the test passes and that the harness didn't edit the test.
The harness gets permission to edit files in that throwaway project.

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
name = "gemma4:26b"                     # the everyday model
code = "qwen3.8:27b"                    # the coding model
host = ""                               # empty: Fane's own models; or an Ollama/OpenAI server
suggest_name = ""                       # optional faster model for proactive suggestions

[runtime]
chat_gpu = "auto"                       # or an id from `fane gpus`; also code/image/video_gpu
image_model = "z-image-turbo"           # "" = off; video_model = "wan2.2-5b"
context = 32768
idle_minutes = 15                       # stop idle models, freeing the GPU
reuse_ollama = true                     # run Ollama's downloads instead of downloading again
share = false                           # let other computers use this one's models (token)

[routes]                                # see "Models, GPUs and other computers"
code = ["local", "claude", "codex"]
image = ["local", "codex"]

[media]
dir = "~/Pictures/Fane"                 # where generated images and videos go

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

[files]
roots = ["~"]                    # folders the file tools may use

[delegate]
default = "auto"                 # or qwen, codegem, claude, codex (see Coding agents)

[agent_mail]
account = "my-assistant@gmail.com"  # sign in with `fane mail login`; empty = email off
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
 │  └─ Tray + hotkey                    ├─ Agent ─ tool loop (OpenAI API) │
 │                                      ├─ Tools: notes, Thunderbird,     │
 │  `fane …` ──► local socket ─────────►│   windows, clipboard, paste,    │
 │                                      │   MCP, mail, files, images,     │
 │                                      │   delegate → coding CLIs        │
 │                                      └─ Model server :11500 ──┐        │
 └───────────────────────────────────────────────────────────────┼────────┘
     routes ([routes]) ─┬─ local: llama-server / sd-server per GPU, started on demand,
                        │         GPUs shared by memory (least recently used idle model goes)
                        ├─ another Fane (its model server, with its sharing token)
                        └─ Ollama / stable-diffusion.cpp servers
```

The source is laid out by concern: [`src/core.rs`](src/core.rs) (the runtime half),
[`src/ui/`](src/ui/) (windows, tray), [`src/agent.rs`](src/agent.rs),
[`src/suggester.rs`](src/suggester.rs), [`src/llm.rs`](src/llm.rs) (model client),
[`src/runtime/`](src/runtime/) (model server: GPUs, downloads, llama.cpp and
stable-diffusion.cpp processes, GPU sharing, routing), [`src/discovery.rs`](src/discovery.rs), [`src/connectors/`](src/connectors/),
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
  own address, and only when your provider's DMARC check passes. Emailed requests get a
  reduced set of tools, and every message to anyone else needs your click. The app password
  (or Online Accounts token) opens the whole mailbox, so give Fane an account of its own, not
  your personal one. The password lives in the system keychain, readable by programs running
  as you once the keychain is unlocked.
- Fane's model server only listens on `127.0.0.1` unless you turn on sharing. With sharing
  on, other computers need its token (sent as a bearer token, compared in constant time);
  without it they only learn that a Fane is there. Requests from this computer need no token,
  so any program running as you can use the models (as with Ollama).
- Sharing, remote Fanes and remote Ollama servers use plain HTTP: prompts, screenshots, the
  token and generated images cross the network unencrypted. Only do this on a network you
  trust, or over a VPN like Tailscale (type the address by hand; the scan only covers your
  local subnets).
- llama.cpp and stable-diffusion.cpp come from their projects' GitHub releases (pinned to the
  builds this Fane was tested with), or from Fane's own release of stable-diffusion.cpp for
  Linux; models come from Hugging Face. All of it goes into `~/.fane`.
- Coding agents run non-interactively in `[delegate] cwd`, so nobody is there to approve
  their actions. Claude Code runs in its auto mode (its classifier approves safe actions and
  blocks risky ones) and Codex with `--approve-for-me` (automatic review, sandboxed to the
  workspace), so both can edit files and run commands on their own; the local harnesses use
  their defaults. When Claude or Codex plans and reviews, it's read-only. Change any command
  under `[delegate.agents.<name>]`.

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
desktop yet, and neither have the GNOME extension and a live mailbox (signing in with an app
password is unit-tested, and the keychain storage was tested on Linux).

The model server has been tested on a NUC with a Tesla V100 and an Intel Iris Xe: setup
(llama.cpp CUDA and Vulkan builds, Ollama's models reused, stable-diffusion.cpp compiled
locally), Gemma 4 on the Iris Xe and Qwen 3.8 on the V100 at the same time, images and
video swapping Qwen off the V100 and back, and a second Fane sending chat, coding, image
and video requests to it over the network with the sharing token. CI's smoke test runs
setup and a chat request through `fane serve` on clean macOS and Windows machines.

App setup has been tested on the same NUC: Blender from Flathub with its add-on turned on,
then *"make a red sphere and a blue cube"* opening Blender by itself and building the scene;
FreeCAD's workbench installed and starting its server with FreeCAD; and the browser's
Playwright server connecting. Driving FreeCAD and the browser on a real desktop is still to
be tried.

Ideas for next steps:

- Read the focused text field through AT-SPI / accessibility APIs, which is more precise
  than the screenshot.
- Signing in to Outlook.com / Microsoft 365 mailboxes (Microsoft's own sign-in).
- Built-in Evolution, GNOME Calendar and browser-tab connectors.
- Streaming answers, and cancelling a running task.
- Learn from which suggestions you accept and dismiss.
- Faster prompt processing on older GPUs (flash attention on Volta), and measuring each
  model's real GPU memory instead of estimating it.
- HTTPS (or a VPN-style tunnel) between Fanes.
