# Grace

A light terminal for running Claude Code on Windows. Built because Warp was eating the PC.

Named after the **Sites of Grace** in Elden Ring, where you rest, level up and find your way, and
**Grace Hopper**, who wrote the first compiler and found the first computer bug. It was called
PulseTerm before. On its first start, Grace moves PulseTerm's settings, saved tabs and Whisper
download over by itself, and it still answers on PulseTerm's pipe, so ClaudeSnap keeps working.

**Measured on this laptop (2026-11-03, idle):**

| | RAM | CPU (one core) |
|---|---|---|
| Warp | 453 MB | 12.2% |
| Grace | 58 MB | 1.1% (cursor blinking, window focused) |

Why it is lighter: it starts no background processes. Warp starts a new PowerShell
about once a second just to show the git branch. Grace only draws when something changes.

## Start it

- Press the Windows key, type **Grace**, press Enter.
- Or run `%LOCALAPPDATA%\Programs\Grace\grace.exe`.
- Starting it again opens a new tab in the same window (only one window).

## What it does

- **Instant typing in Claude's own input line.** In a long chat Claude sometimes stalls before it
  draws a typed letter (measured on a 93 MB chat: usually about 30 ms, but up to 671 ms). Grace
  draws each letter at the cursor right away, and Claude's own drawing replaces it a moment later
  (same letter, same place, so you see no change). It only guesses plain letters, space and Backspace
  at the end of your text. Enter, arrows, Tab, Ctrl keys, paste and clicks show Claude's real screen
  at once. Tested by pausing Claude: "hello world" (with two Backspaces) showed while Claude was
  frozen, and Claude's text matched exactly afterwards.
- **Toolbar under every Claude tab** (like Warp's):
  - **Push to talk: hold Right Alt**, talk, let go. The words are typed into Claude's input line
    (or into Rich input when it is open) about a second after you let go. No Windows popup, nothing
    to click. A quick tap (under 0.3 s) types nothing, and Right Alt + another key (AltGr+E = é)
    still types that letter.
  - **Mic button** (or Ctrl+Shift+M): the same, but click to start and click again (or Esc) to stop.
    The button is red while it records.
  - The speech engine is **Whisper**, an AI speech model that runs on this PC (whisper.cpp): good
    with noisy mics and accents, and your voice never leaves the PC. A small voice detector (Silero
    VAD) cuts out the parts with no speech, so room noise is not turned into words.
  - **First start on a PC:** Grace downloads what Whisper needs, about 160 MB, into
    `%LOCALAPPDATA%\Grace\whisper` (took 20 s here). Every file is checked against a fixed SHA-256
    fingerprint, the same ones GitHub and Hugging Face list, so a broken or changed download is never
    used. Pressing the mic during the download shows how far it is.
  - Measured on an Intel i5-12400F: 7 s of speech turned into text in 1.2 s ("Please list all the
    files in this folder, then show me the biggest one." word for word, with punctuation).
  - Why not Windows' own speech engines: on this PC the one apps may use heard nothing at all (Win+H
    uses a newer one that apps cannot call), and the old offline one turned room noise into words.
    They are still there as settings (`voice_engine`).
  - **Rich input** (Ctrl+Shift+I): opens a Word-like typing box for that tab. Off by default.
    It edits like Word: click to put the cursor, drag / double-click / triple-click to select,
    Shift+arrows, Ctrl+arrows (word by word), Ctrl+Backspace, Ctrl+A, Ctrl+C / X / V, Ctrl+Z / Ctrl+Y,
    Home / End, Shift+Enter for a new line, Enter sends. Ctrl+Up / Ctrl+Down bring back messages
    you sent from the box.
    - When the box is empty, Enter, arrows, Tab and Backspace go to Claude, so Claude's own menus work.
    - Esc, Shift+Tab, PgUp/PgDn and Alt+keys (Alt+V = paste a picture) always go to Claude.
    - Typing `/` in an empty box sends it straight to Claude, so Claude's command list shows.
      Enter runs the command; Esc cancels it and clears it from Claude's prompt.
    - While Claude asks you something (a permission, a choice), every key goes to Claude.
- **Open Claude in another folder.** Claude cannot move to another folder once it runs (its chat
  belongs to the folder it started in), so this opens a NEW tab with a fresh Claude there; the chat
  you were in keeps running. Three ways: the folder button on the toolbar (it shows the tab's folder;
  its menu also has your recent folders, "Show this folder in Explorer" and "Copy this folder's
  path"), the ⌄ menu next to +, or Ctrl+Shift+O (Windows' folder picker). The first time in a new
  folder Claude asks whether you trust it. Recent folders are kept in
  `%APPDATA%\Grace\recent_folders.json`.
- **Picture thumbnails.** Every line in a Claude tab that shows `[Image #3]` or an image file path
  gets a small picture at the right edge (about 3 text lines tall). Click it to see the picture big
  over the window; Esc or a click closes it. Pointing the mouse at the `[Image #3]` text itself also
  pops up a preview. Works for `.png .jpg .jpeg .gif .bmp .webp .ico .tif`. Claude keeps pasted
  pictures inside the chat log; Grace reads them from there. A picture you pasted but did not
  send yet is not in the log, so Grace keeps its own copy from the clipboard when you paste
  (Ctrl+V or ClaudeSnap).
  - The thumbnails take no columns: they float on empty space at the right edge, so the terminal
    keeps its full width. They never cover text (the end of a `────` line is fine). If there is no
    empty space within a few lines, that thumbnail is not drawn; hovering the text still works.
  - **Albums.** Pictures on lines close together (a list of paths, one empty line between is OK)
    become one card: 2 side by side, 3 as one wide + two small, 4 or more as a 2×2 grid where the
    last tile says "+N". Click a tile to open it big, then ← / → to see the others.
  - Paths with spaces, and paths split over two lines, are not detected yet.
- **Select text with a normal drag**, even inside Claude's full-screen view. A click without
  dragging still goes to Claude. Double-click = word, triple-click = line. Ctrl+C copies.
- **Tabs and split panes.** Drag tabs to reorder. Drag the line between panes to resize.
  - **Split with an existing tab:** right-click a tab (or inside a pane) > Split right with tab ▸ /
    Split down with tab ▸ > pick the tab. That tab's panes move in next to it. Nothing restarts:
    Claude keeps working and the chat is not touched.
  - **Move pane to its own tab** (right-click inside a pane) undoes it.
- **RAM on every tab.** Amber above 1 GB, red above 2 GB. Hover the number to see which programs use it.
  The top-right meter shows the whole PC's RAM. Hover it for details.
- **Refresh chat** (right-click the tab, or click its RAM badge when it shows ↻). Claude restarts
  with the same chat. A long-running Claude keeps growing; a fresh one loads the same chat and uses
  far less. Measured on the same 320k-token chat: **1,655 MB (3 days old) → 580 MB fresh**.
  The badge shows ↻ when a Claude is over 1 GB and idle for 10+ minutes.
- **Bring in chats from Warp** (the ⌄ next to + > Bring in chats open in other windows). Every
  Claude chat open in another window becomes a sleeping tab here (0 MB) with its real title.
  Close it in Warp, then press Enter here. If it is still open there, Grace warns you first.
- **Zoom a pane** (Ctrl+Shift+Z): show one split pane full size; again to go back.
- **Sleep a tab** (right-click the tab > Sleep tab). This stops Claude and frees all its RAM.
  The chat is kept. Press Enter in that tab to resume it (`claude --resume` runs for you).
  This is the real fix for long chats: a 3-day-old chat can use 1.5 GB, and sleeping it gives that back.
- **Status dot** on each tab: amber = Claude working, red = Claude asks you something,
  green = idle, blue ring = asleep. It reads the small status files Claude Code already writes,
  so it costs nothing.
- **Alerts.** When Claude finishes or needs you in a tab you are not looking at, the taskbar
  flashes and Windows shows a popup. Click the popup to jump to that tab.
- **Saved tabs.** Closing Grace saves your tabs. Next start, they come back **asleep**
  (0 MB) and wait for Enter, so starting Grace never loads 5 big chats at once.
- **Clickable paths and links.** Hold Ctrl and click a path like `C:\x\app.rs:42` or a URL.
  Files open in VS Code at that line (setting `editor`).
- **Kill on close.** Closing a tab or pane stops everything started in it (Claude, node, servers).
  Nothing is left running in the background.
- **Paste.** Ctrl+V pastes text. If the clipboard holds only a picture, Ctrl+V sends Alt+V, so
  Claude pastes the picture. A multi-line paste into a plain shell asks first (each line would run).
- **Drop files** on the window to paste their paths.
- **Find** in the output: Ctrl+Shift+F.
- **Screenshots from ClaudeSnap** go straight into a tab (see below).

## Shortcuts

| Keys | What |
|---|---|
| Ctrl+Shift+T | New tab (default profile = Claude) |
| Ctrl+Shift+O | Open Claude in another folder (new tab) |
| Ctrl+Shift+I | Open / close Rich input for this tab |
| Ctrl+Shift+M | Mic: start / stop listening (Esc also stops) |
| Hold Right Alt | Push to talk: listens while held, types the words when you let go |
| Ctrl+Shift+Space, or the ⌄ next to + | New tab with another profile (PowerShell, Git Bash, cmd) |
| Ctrl+Shift+W | Close pane (or tab) |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl+1 … Ctrl+8, Ctrl+9 | Go to tab 1-8, last tab |
| Ctrl+Shift+PgUp / PgDn | Move tab left / right |
| Ctrl+Shift+D / Ctrl+Shift+E | Split right / split down |
| Ctrl+Alt+Arrow | Move to the pane in that direction |
| Ctrl+Shift+R, or double-click a tab | Rename tab |
| Ctrl+Shift+Z | Zoom the current pane (again = back) |
| Shift+Enter | New line inside Claude's prompt |
| Ctrl+C | Copy if text is selected, otherwise sends Ctrl+C |
| Ctrl+V, Ctrl+Shift+V, Shift+Insert | Paste |
| Ctrl+Shift+F | Find (Enter = older match, Shift+Enter = newer, Esc = close) |
| Shift+PgUp / PgDn, Ctrl+Shift+Home / End | Scroll |
| Ctrl+= / Ctrl+- / Ctrl+0 | Font bigger / smaller / reset |
| Ctrl+, | Open the settings file |
| Middle-click a tab | Close it |

**Selecting text inside Claude:** Claude Code (with `"tui": "fullscreen"`) uses the mouse itself.
Grace still selects on a drag and sends Claude only plain clicks and the wheel.
Set `mouse_select = false` to give Claude every mouse event (then Shift+drag selects).

## Move your Warp chats over

Easiest: the ⌄ next to + > **Bring in chats open in other windows**. Each chat arrives asleep.
Close it in Warp, then press Enter in its Grace tab.

Or by hand: each running chat has an id. Run this for each one (Grace opens a tab and resumes it):

```
%LOCALAPPDATA%\Programs\Grace\grace.exe --resume <chat id>
```

The ids of running chats are in `%USERPROFILE%\.claude\sessions\<pid>.json` (`sessionId`).
Close the Warp tab first, so the same chat is not open twice.

## Long chats and "virtual scroll"

- Grace's own history is virtual: it keeps at most `scrollback_lines` (10,000) and draws only
  the rows on screen, so a long session costs the same as a short one. Measured: 20,000 lines
  printed → exactly 10,000 kept, whole app 91 MB.
- Rows are cached by their content, so text that scrolls up is not shaped again. Symbols and emoji
  are drawn once and reused as small pictures (Windows redraws colour glyphs slowly).
  Measured while text floods in, per frame: colour emoji 120 ms → 3.8 ms, Claude's symbols
  (⏺ ⎿ ✻ ❯) 42 ms → 1.3 ms, plain text 2.8 ms.
- Claude Code itself runs full-screen and scrolls its own chat (the mouse wheel goes to it).
  The memory that grows is inside Claude, so the fix is **Refresh chat** or **Sleep tab**.

## Settings

File: `%APPDATA%\Grace\config.toml` (Ctrl+, opens it). Restart Grace or use
right-click a tab > Reload settings.

- `font_family`, `font_size` (points)
- `scrollback_lines`: old lines kept per pane (default 10,000). Older lines are dropped, so a pane's
  memory stays flat however long it runs.
- `default_profile`, `profiles`: what new tabs run, and in which folder.
- `claude_command`: used to resume chats (`--resume <id>` is added).
- `fallback_shell`: when a Claude-only tab's chat ends, this shell starts in the same folder.
- `ram_warn_mb` (default 1024), `editor` (default `code`), `copy_on_select`, `cursor_blink`,
  `confirm_multiline_paste`, `restore_tabs`, `notify_popup`.
- `cursor_blink` (default true): the cursor blinks at Windows' speed (Settings > Accessibility >
  Text cursor) with soft fades. It stays solid while you type or while it moves, and stops blinking
  after Windows' caret timeout (5 seconds) without typing, like Notepad. Measured: blinking costs
  0.8-2% of one core while it fades, 0.16% once it stops. `false` = never blink.
- `fast_symbols` (default true): the symbol/emoji picture cache. Set false if symbols look wrong.
- `input_box` (default false): open the Rich input box under Claude tabs from the start.
- `toolbar` (default true): the bar with the mic and Rich input buttons under Claude tabs.
- `instant_typing` (default true): show typed letters before Claude draws them.
- `thumbnails` (default true): small pictures at the right edge, click to open big.
- `mouse_select` (default true): a drag selects text even when the program uses the mouse.
- `voice_engine` (default "auto"): the mic. "auto" = Whisper on this PC. Or Windows' own engines:
  "windows" (modern, else offline), "modern", "offline", or "win+h" (open Windows' voice typing
  panel, the old way).
- `voice_model` (default "base.en"): the Whisper model. "small.en" (488 MB) is better with accents
  and noise but about 3x slower; it is downloaded the first time it is chosen.
- `voice_language` (default ""): language for Windows' modern engine, like "en-IN" or "en-US".
  "" = Windows' speech language. (The offline engine uses the one installed: here en-GB.)
- `voice_hold` (default true): hold Right Alt to talk.

## Testing a new build without touching the one you work in

`grace.exe --dev` starts a separate test copy: its own window ("Grace (test copy)"),
its own pipe (`\\.\pipe\grace-dev-<username>`) and its own settings folder
(`%APPDATA%\Grace-dev`). It opens behind other windows and never takes the keyboard.
Extra pipe commands for tests: `{"cmd":"type","text":"..."}`, `{"cmd":"key","vk":13,"shift":true}`,
`{"cmd":"box"}` (box text, caret, selection), `{"cmd":"boxhit","x":..,"y":..}`.
Mic tests: `{"cmd":"voice","wav":"C:/say.wav"}` makes the mic listen to a .wav file (test copy
only), `{"cmd":"voice_hold","down":true}` / `false` acts like Right Alt, `{"cmd":"voice_state"}`
shows what it heard so far, `{"cmd":"voice_heard","text":"..."}` pretends it heard words.

To update the installed copy while it is running: rename `grace.exe` to `grace.old.exe`
(Windows allows renaming a running program), copy the new one in, and restart Grace when convenient.

## ClaudeSnap

`J:\Projects\claude-snap` now sends to Grace first and falls back to Warp. With Grace
there are no fake key presses: it asks Grace to paste into the tab directly, so reordering tabs
or holding a key cannot break it. ClaudeSnap must be rebuilt and restarted to get this.

## The local pipe (for scripts)

`grace.exe --send '<json>'` or write one JSON line to `\\.\pipe\grace-<username>`
(the old name `\\.\pipe\pulseterm-<username>` works too):

- `{"cmd":"list"}` - tabs with title, status, RAM, chat id
- `{"cmd":"paste","tab":2,"text":"why?","image":true,"submit":false}`
- `{"cmd":"send","tab":2,"text":"ls\r"}` - raw keys
- `{"cmd":"dump","tab":2}` - the visible screen as text
- `{"cmd":"new_tab","profile":"PowerShell","cwd":"C:\\"}`, `{"cmd":"new_tab","resume":"<chat id>"}`
- `{"cmd":"focus","tab":2}`, `{"cmd":"split","tab":2,"vertical":true}`,
  `{"cmd":"sleep","tab":2}`, `{"cmd":"resume","tab":2}`, `{"cmd":"refresh","tab":2}`, `{"cmd":"close_tab","tab":2}`
- `{"cmd":"import"}`, `{"cmd":"zoom"}`, `{"cmd":"selection"}`, `{"cmd":"scroll","to":"top"}`
- `{"cmd":"join","tab":3,"into":1,"vertical":true}` - tab 3 moves next to tab 1's focused pane
  (`vertical: false` = below). `{"cmd":"detach","tab":1}` - tab 1's focused pane gets its own tab.
- `list` also returns speed numbers: `frame_avg_ms`, `frame_max_ms`, `perf_ms` (lock wait, copy, draw)

`tab` is 1-based. Leave it out for the active tab.

## How it is built (for whoever changes it)

- Rust. The terminal core is `alacritty_terminal` (the same one the Zed editor uses): it reads the
  codes programs send. Windows ConPTY runs the shell.
- Drawing: Direct2D + DirectWrite (the text engine Windows Terminal uses), so emoji, symbols and
  Hindi come out right. Each screen row keeps its shaped text and is only re-shaped when it changes.
  Box-drawing lines are drawn by hand so they join between rows.
- `src/procmon.rs`: one background thread. Every second it reads Claude's status files; every 4th
  second it lists processes for the RAM numbers. It starts no processes.
- Each tab's programs run inside a Windows "job". Closing the job stops all of them at once.
- Build: `cargo build --release -j 4` (first build ~4 minutes). Then copy
  `target\release\grace.exe` to `%LOCALAPPDATA%\Programs\Grace\`.
- The app icon lives inside the exe (`build.rs` adds `assets/grace.rc`). The build needs the
  Windows SDK's `rc.exe` (comes with the Visual Studio C++ build tools). To change the icon, edit
  and run `python assets/make_icon.py` (needs Pillow), then build again.
- Log file: `%APPDATA%\Grace\grace.log`. A crash writes its reason and code lines there
  (keep `grace.pdb` next to the exe for line numbers).

## Known gaps (version 1)

- Tested by script, not yet by hand: the Windows popup, drag and drop, typing in other languages
  (IME), moving the window between screens with different scaling.
- The symbol/emoji picture cache was measured but not yet seen on screen (the PC was locked).
  If symbols look wrong, set `fast_symbols = false`.
- One unexplained exit happened during a 20,000-line flood test at 98% system RAM. It did not happen
  again in 3 repeats. The crash logger is now on, so a repeat will leave its reason in the log.
- ClaudeSnap → Grace compiles, but has not been tried end to end yet.
- The tabs sit under the normal Windows title bar (no custom title bar yet).
