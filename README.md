# PulseTerm

A light terminal for running Claude Code on Windows. Built because Warp was eating the PC.

**Measured on this laptop (2026-11-03, idle):**

| | RAM | CPU (one core) |
|---|---|---|
| Warp | 453 MB | 12.2% |
| PulseTerm | 58 MB | 1.1% (cursor blinking, window focused) |

Why it is lighter: it starts no background processes. Warp starts a new PowerShell
about once a second just to show the git branch. PulseTerm only draws when something changes.

## Start it

- Press the Windows key, type **PulseTerm**, press Enter.
- Or run `%LOCALAPPDATA%\Programs\PulseTerm\pulseterm.exe`.
- Starting it again opens a new tab in the same window (only one window).

## What it does

- **Typing box under every Claude tab** (like Warp's). You type in PulseTerm's own box, so letters
  appear at once (about 2 ms to draw), even when Claude or the PC is slow. Enter sends it to Claude.
  It edits like Word: click to put the cursor, drag / double-click / triple-click to select,
  Shift+arrows, Ctrl+arrows (word by word), Ctrl+Backspace, Ctrl+A, Ctrl+C / X / V, Ctrl+Z / Ctrl+Y,
  Home / End, Shift+Enter for a new line. Ctrl+Up / Ctrl+Down bring back messages you sent from the box.
  - When the box is empty, Enter, arrows, Tab and Backspace go to Claude, so Claude's own menus work.
  - Esc, Shift+Tab, PgUp/PgDn and Alt+keys (Alt+V = paste a picture) always go to Claude.
  - Typing `/` in an empty box sends it straight to Claude, so Claude's command list shows.
    Enter runs the command; Esc cancels it and clears it from Claude's prompt.
  - While Claude asks you something (a permission, a choice), every key goes to Claude.
  - Ctrl+Shift+I hides the box for that tab (keys go straight to Claude again).
- **Voice typing**: click the mic in the box, or press Win+H. This is Windows' own voice typing;
  the words land in the box. Check them, then press Enter.
- **Select text with a normal drag**, even inside Claude's full-screen view. A click without
  dragging still goes to Claude. Double-click = word, triple-click = line. Ctrl+C copies.
- **Tabs and split panes.** Drag tabs to reorder. Drag the line between panes to resize.
- **RAM on every tab.** Amber above 1 GB, red above 2 GB. Hover the number to see which programs use it.
  The top-right meter shows the whole PC's RAM. Hover it for details.
- **Refresh chat** (right-click the tab, or click its RAM badge when it shows ↻). Claude restarts
  with the same chat. A long-running Claude keeps growing; a fresh one loads the same chat and uses
  far less. Measured on the same 320k-token chat: **1,655 MB (3 days old) → 580 MB fresh**.
  The badge shows ↻ when a Claude is over 1 GB and idle for 10+ minutes.
- **Bring in chats from Warp** (the ⌄ next to + > Bring in chats open in other windows). Every
  Claude chat open in another window becomes a sleeping tab here (0 MB) with its real title.
  Close it in Warp, then press Enter here. If it is still open there, PulseTerm warns you first.
- **Zoom a pane** (Ctrl+Shift+Z): show one split pane full size; again to go back.
- **Sleep a tab** (right-click the tab > Sleep tab). This stops Claude and frees all its RAM.
  The chat is kept. Press Enter in that tab to resume it (`claude --resume` runs for you).
  This is the real fix for long chats: a 3-day-old chat can use 1.5 GB, and sleeping it gives that back.
- **Status dot** on each tab: amber = Claude working, red = Claude asks you something,
  green = idle, blue ring = asleep. It reads the small status files Claude Code already writes,
  so it costs nothing.
- **Alerts.** When Claude finishes or needs you in a tab you are not looking at, the taskbar
  flashes and Windows shows a popup. Click the popup to jump to that tab.
- **Saved tabs.** Closing PulseTerm saves your tabs. Next start, they come back **asleep**
  (0 MB) and wait for Enter, so starting PulseTerm never loads 5 big chats at once.
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
PulseTerm still selects on a drag and sends Claude only plain clicks and the wheel.
Set `mouse_select = false` to give Claude every mouse event (then Shift+drag selects).

## Move your Warp chats over

Easiest: the ⌄ next to + > **Bring in chats open in other windows**. Each chat arrives asleep.
Close it in Warp, then press Enter in its PulseTerm tab.

Or by hand: each running chat has an id. Run this for each one (PulseTerm opens a tab and resumes it):

```
%LOCALAPPDATA%\Programs\PulseTerm\pulseterm.exe --resume <chat id>
```

The ids of running chats are in `%USERPROFILE%\.claude\sessions\<pid>.json` (`sessionId`).
Close the Warp tab first, so the same chat is not open twice.

## Long chats and "virtual scroll"

- PulseTerm's own history is virtual: it keeps at most `scrollback_lines` (10,000) and draws only
  the rows on screen, so a long session costs the same as a short one. Measured: 20,000 lines
  printed → exactly 10,000 kept, whole app 91 MB.
- Rows are cached by their content, so text that scrolls up is not shaped again. Symbols and emoji
  are drawn once and reused as small pictures (Windows redraws colour glyphs slowly).
  Measured while text floods in, per frame: colour emoji 120 ms → 3.8 ms, Claude's symbols
  (⏺ ⎿ ✻ ❯) 42 ms → 1.3 ms, plain text 2.8 ms.
- Claude Code itself runs full-screen and scrolls its own chat (the mouse wheel goes to it).
  The memory that grows is inside Claude, so the fix is **Refresh chat** or **Sleep tab**.

## Settings

File: `%APPDATA%\PulseTerm\config.toml` (Ctrl+, opens it). Restart PulseTerm or use
right-click a tab > Reload settings.

- `font_family`, `font_size` (points)
- `scrollback_lines`: old lines kept per pane (default 10,000). Older lines are dropped, so a pane's
  memory stays flat however long it runs.
- `default_profile`, `profiles`: what new tabs run, and in which folder.
- `claude_command`: used to resume chats (`--resume <id>` is added).
- `fallback_shell`: when a Claude-only tab's chat ends, this shell starts in the same folder.
- `ram_warn_mb` (default 1024), `editor` (default `code`), `copy_on_select`, `cursor_blink`,
  `confirm_multiline_paste`, `restore_tabs`, `notify_popup`.
- `fast_symbols` (default true): the symbol/emoji picture cache. Set false if symbols look wrong.
- `input_box` (default true): the typing box under Claude tabs.
- `mouse_select` (default true): a drag selects text even when the program uses the mouse.

## Testing a new build without touching the one you work in

`pulseterm.exe --dev` starts a separate test copy: its own window ("PulseTerm (test copy)"),
its own pipe (`\\.\pipe\pulseterm-dev-<username>`) and its own settings folder
(`%APPDATA%\PulseTerm-dev`). It opens behind other windows and never takes the keyboard.
Extra pipe commands for tests: `{"cmd":"type","text":"..."}`, `{"cmd":"key","vk":13,"shift":true}`,
`{"cmd":"box"}` (box text, caret, selection), `{"cmd":"boxhit","x":..,"y":..}`.

To update the installed copy while it is running: rename `pulseterm.exe` to `pulseterm.old.exe`
(Windows allows renaming a running program), copy the new one in, and restart PulseTerm when convenient.

## ClaudeSnap

`J:\Projects\claude-snap` now sends to PulseTerm first and falls back to Warp. With PulseTerm
there are no fake key presses: it asks PulseTerm to paste into the tab directly, so reordering tabs
or holding a key cannot break it. ClaudeSnap must be rebuilt and restarted to get this.

## The local pipe (for scripts)

`pulseterm.exe --send '<json>'` or write one JSON line to `\\.\pipe\pulseterm-<username>`:

- `{"cmd":"list"}` - tabs with title, status, RAM, chat id
- `{"cmd":"paste","tab":2,"text":"why?","image":true,"submit":false}`
- `{"cmd":"send","tab":2,"text":"ls\r"}` - raw keys
- `{"cmd":"dump","tab":2}` - the visible screen as text
- `{"cmd":"new_tab","profile":"PowerShell","cwd":"C:\\"}`, `{"cmd":"new_tab","resume":"<chat id>"}`
- `{"cmd":"focus","tab":2}`, `{"cmd":"split","tab":2,"vertical":true}`,
  `{"cmd":"sleep","tab":2}`, `{"cmd":"resume","tab":2}`, `{"cmd":"refresh","tab":2}`, `{"cmd":"close_tab","tab":2}`
- `{"cmd":"import"}`, `{"cmd":"zoom"}`, `{"cmd":"selection"}`, `{"cmd":"scroll","to":"top"}`
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
  `target\release\pulseterm.exe` to `%LOCALAPPDATA%\Programs\PulseTerm\`.
- Log file: `%APPDATA%\PulseTerm\pulseterm.log`. A crash writes its reason and code lines there
  (keep `pulseterm.pdb` next to the exe for line numbers).

## Known gaps (version 1)

- Tested by script, not yet by hand: the Windows popup, drag and drop, typing in other languages
  (IME), moving the window between screens with different scaling.
- The symbol/emoji picture cache was measured but not yet seen on screen (the PC was locked).
  If symbols look wrong, set `fast_symbols = false`.
- One unexplained exit happened during a 20,000-line flood test at 98% system RAM. It did not happen
  again in 3 repeats. The crash logger is now on, so a repeat will leave its reason in the log.
- ClaudeSnap → PulseTerm compiles, but has not been tried end to end yet.
- The tabs sit under the normal Windows title bar (no custom title bar yet).
