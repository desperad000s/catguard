<img src="assets/logo.png" width="128" alt="catguard">

# catguard

Locks the keyboard when a cat steps on it. Runs in the Windows tray, plays a
harmonica at the cat, and unlocks when you type `human`. Afterwards it shows
which keys got through and takes back what keys can take back.

The mouse and the touchpad stay free. One exe under 1 MB, no installer, no
network access. The only file it writes is its settings.

## Use it

Run `catguard-setup-x.y.z.exe`. It installs for your Windows account only
(no administrator rights), adds a Start menu entry and an uninstaller, and
starts catguard in the tray when you sign in. `catguard.exe` also runs on its
own without being installed.

A round cat icon appears in the tray: a lime ring while
it watches, a grey one while it is paused. A left click opens the app, a
right click has pause and exit.

When a paw lands, a black window says so and the keyboard goes dead. Type
`human` (you do not need to click anything first) or click the button. Keys
the cat is still standing on stay dead until it lets go.

`Ctrl+Alt+Del` always works. Windows does not let any program intercept it.

The app has three pages:

- **Watch** shows whether catguard is on, and a keyboard whose keys light up
  as you press them, so you can try a flat hand and see what catguard sees.
- **Incident** shows the last lock: see the next section.
- **Settings**: sensitivity (relaxed for gamers, normal, kitten), the sound,
  the unlock word, start with Windows, dark or light.

Watch also has a button that locks the keyboard by hand, for wiping it.

There are four sounds, all synthesized: the harmonica PawSense has used since
1999, a cat's hiss, two bursts like a can of compressed air, and a tone at 15
to 17 kHz that most adults barely hear. None is proven for every cat. Pick
the one yours dislikes.

The app window is a WebView2 page that exists only while it is open. Closed,
catguard is the keyboard hook and a tray icon. WebView2 is part of Windows 11
and of current Windows 10. Without it the guard still works and only the
window is missing.

## What the cat did

The incident page draws a timeline: one row per key, one bar per press, as
long as the key was down. Lime bars reached your programs while the paw was
down, grey bars are what you typed in the three seconds before, outlined bars
were blocked. If anything got through, the app opens on this page right after
the unlock.

Below the timeline catguard names what got through. For combinations it knows
(Alt+F4, Ctrl+W, Win+D, Caps Lock, mute, the touchpad key, about thirty
in all) it says what they do and, where there is one, how to take it back by
hand.

The undo button does two things and says which before you click:

- It presses a switch again: Caps Lock, Num Lock, Scroll Lock, Insert, mute,
  play/pause, Win+D, the colour filter, Narrator, and Ctrl+Win+F24, which is
  what laptops with a precision touchpad send for the touchpad key. Win+M is
  answered with Win+Shift+M.
- It sends Backspace once per typed character, but only if characters were
  all that arrived and you have not typed since.

Undo first hands the focus back to the window the cat typed into, and types
nothing if that window is gone.

What it cannot do: catguard sees keys, not what a program did with them. A
closed tab or a deleted file is the program's to restore. And anything the Fn
key does inside the keyboard never reaches Windows. Fn+Esc (Fn lock) on a
Lenovo is switched by the keyboard controller. No program can see it, block
it or reverse it.

The history covers three seconds, lives in memory, and is never written
anywhere.

## How it tells a paw from a hand

A finger presses one key. A paw covers two key units and presses everything
under it in the same instant. The detector looks only at which physical keys
are down and when they went down, so the keyboard layout does not matter.

| Rule  | Fires when                                                           | Decides after      |
|-------|----------------------------------------------------------------------|--------------------|
| Slam  | 3 keys under one paw go down within 60 ms (25 ms if in one row)      | 0 ms, on key three |
| Chord | 4 keys are down within 3.5 key units                                 | 0 ms, on key four  |
| Pair  | 2 neighbours go down within 30 ms and both stay down                 | 250 ms             |
| Sit   | 3 keys are held for 2 s                                              | 2 s                |

Why Slam can be instant: any three keys that fit under a paw across two rows
include two keys of the same finger column, and one finger needs about 100 ms
to get from one key to the next. No typist produces that pattern in 60 ms.

Why Pair exists: many laptop keyboards cannot report a third key inside the
same block of their matrix. On those a paw looks like two keys, and two keys
are only suspicious once they stay down longer than typing ever holds them.

Shift, Ctrl, Alt and Win never count. Injected input (macros, on-screen
keyboard, remote desktop) is ignored.

The thresholds live in `Thresholds::default()` in `src/detector.rs`. They come
from reasoning about hands and paws, not yet from recordings of real cats.

## Known limits

- The keys that arrive before a rule fires reach the application: two
  characters for Slam, two held keys for 250 ms for Pair.
- Games: holding two neighbouring keys that you pressed in the same 30 ms
  (W+A) looks like a paw. Use Pause in the tray menu.
- Windows hides keystrokes that go to an elevated window from a program that
  is not elevated. While an admin window has the focus, catguard sees nothing.
- The exe is not code-signed, so browsers and SmartScreen warn about an
  unknown publisher. See "Signing" below.
- A global keyboard hook is also what a keylogger uses, so antivirus software
  may ask questions about an unsigned build. Read `src/win.rs` and
  `src/history.rs`: the hook keeps the last three seconds of key codes in
  memory for the timeline and nothing else.
- Backspace undo counts key-downs. A dead key (`^`, `´` on a German layout)
  types nothing by itself, so the count can be one too high.
- Whether your laptop sends Ctrl+Win+F24 for the touchpad key shows in the
  timeline the first time it happens.

## Build

```
cargo test                      # the detection core, on any OS
cargo build --release           # on Windows, MSVC toolchain
cargo xwin build --release --target x86_64-pc-windows-msvc   # from Linux
makensis -DVERSION=0.3.0 installer/catguard.nsi                # the setup
python3 assets/make_icons.py    # all icons from one drawing; rsvg-convert, Pillow
tests/wine-smoke.sh             # the real exe under Wine: lock, swallow, unlock
```

The Linux build needs `cargo install cargo-xwin` plus `lld` and `llvm`
(`lld-link`, `llvm-rc`). The MSVC target matters: it links the WebView2
loader and the C runtime statically, so the result is a single exe. The GNU
target would need `WebView2Loader.dll` next to it.

To look at the app without Windows, open `ui/index.html` in a browser. It
runs on demo data there: `?page=incident`, `?page=settings`, `?theme=light`,
`?mode=paused`, `?mode=locked`.

## Signing

Windows and Chrome warn because nobody has vouched for the exe. No setting in
the program changes that. A publisher name needs a certificate, and even
with one SmartScreen keeps warning until enough people have installed the
signed file. The realistic routes for this project:

- SignPath Foundation signs open-source projects for free. It wants a public
  repository, an OSI licence, a release, and a build that runs in CI.
- Azure Artifact Signing, about 10 USD a month, is open to companies in the
  EU and to individuals in the USA and Canada.

Until then: the exe and the setup carry version information naming WEBSEED
OÜ, and the download page should publish their SHA-256. An installer does
not remove the warning. It is unsigned too, and the same rules apply to it.

## Layout

- `src/layout.rs`: where each scancode sits on the board
- `src/detector.rs`: the four rules
- `src/guard.rs`: lock state, which events get swallowed, the unlock word
- `src/history.rs`: the three-second history, known shortcuts, the undo plan
- `src/sound.rs`: the synthesized harmonica
- `src/settings.rs`: the settings file and its sanitizing
- `src/win.rs`: hook thread, tray, lock window, the app window and its messages
- `ui/index.html`: the app, one file, no build step
- `assets/`: every icon, rendered by `make_icons.py` from one vector drawing
- `installer/catguard.nsi`: the per-user setup
- `tests/wine-smoke.sh`: end-to-end check of the exe under Wine

## Credit

The idea is PawSense by Chris Niswander (BitBoost, 1999). catguard shares no
code with it. The rule set started from the description in
[joeyvigil/pawsense](https://github.com/joeyvigil/pawsense) (MIT).
