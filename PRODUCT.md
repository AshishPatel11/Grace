# Product

<!-- impeccable:product-schema 1 -->

## Platform

windows-native (a Win32 + Direct2D desktop app, not web; impeccable's list has no native-Windows value, so this is recorded as is)

## Users

Developers on Windows who keep several Claude Code chats open all day, next to a few PowerShell / Git Bash tabs. It starts with the author's own daily work; publishing it for others is planned.

## Product Purpose

Grace is a terminal made for Claude Code on Windows. It exists because general terminals (Windows Terminal, Warp) waste RAM and CPU and do not understand Claude chats. Success: many chats open at once without the PC slowing down, and Claude's output easy to read and act on.

## Positioning

Light and fast (low RAM and CPU, ~1-2 ms frames, nothing running while idle), made for Claude Code: sleeping tabs that free a chat's RAM, Claude's chat drawn like a chat app (code boxes with Copy, headings, links with a menu), voice typing with an on-device model, and a search over every old Claude chat on the PC.

## Operating Context

- Used all day on one Windows PC, often with 4-6 tabs; most tabs are Claude Code in full-screen mode, some are shells.
- Works with the user's real projects, git repos, dev servers (port buttons), and VS Code (the editor for code links).
- Talks to other tools through a local pipe (ClaudeSnap, tests).

## Capabilities and Constraints

- Tabs (sleep / wake, rename, split panes), a toolbar under each tab (mic, Rich input, folder, Changes, dev server ports), a RAM meter.
- Claude chat look (chat.rs), link hover + dropdown (linkmenu.rs), old chat finder (finder.rs), diff viewer (diffview.rs).
- Drawing is Direct2D/DirectWrite on the UI thread; text rows are cached by content.
- **Binding:** no speed loss from design work: frames stay around 1-2 ms, RAM stays low, nothing animates or redraws while idle.
- **Binding:** motion is calm: smooth glides, soft deceleration, 240-320 ms for movement, ~180 ms for colour/opacity; no scale pops, springs, bounces or tilts.
- Open: whether the monospace "terminal feel" must stay everywhere (not made binding by the user).
- Open: Windows 10 support (not made binding; Windows 11 is the daily machine).

## Brand Commitments

- Name: Grace (renamed from PulseTerm). App icon in `assets/grace.ico` (made by `assets/make_icon.py`).
- Not an Anthropic product: it may say it is made for Claude Code, but must not use Anthropic's Claude logo or look official.

## Evidence on Hand

- Measured numbers in README.md (frame times, RAM, finder indexing speed) from the author's PC.
- No users, testimonials or download counts yet; do not invent any.

## Product Principles

1. Speed is a feature: a design change that costs frame time or idle CPU is wrong, however good it looks.
2. The chat is the work: Claude's output and the input box get the space and the clearest contrast; chrome stays quiet.
3. Always show state: which tab is open, which chat is busy, asking or asleep must be readable at a glance.
4. Calm, not flashy: motion explains a change and then stops.
