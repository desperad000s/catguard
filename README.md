<img src="assets/logo.png" width="128" alt="catguard">

# catguard

Locks the keyboard when a cat steps on it. Runs in the Windows tray, plays a
harmonica at the cat, and unlocks when you type `human`. Afterwards it shows
which keys got through and takes back what keys can take back.

The mouse and the touchpad stay free. One 450 KB exe, no installer, no
runtime, no network access, no files written.

## Use it

Start `catguard.exe`. A cat head appears in the tray: lime eyes while it
watches, grey while paused. Its menu: last incident, pause, start with
Windows, exit.

When a paw lands, a small window says so and the keyboard goes dead. Type
`human` (you do not need to click anything first) or click the button. Keys
the cat is still standing on stay dead until it lets go.

`Ctrl+Alt+Del` always works. Windows does not let any program intercept it.

## What the cat did

The lock window draws a timeline: one row per key, one bar per press, as long
as the key was down. Lime bars reached your programs before the lock fell,
light bars are what you typed in the three seconds before, dark bars were
blocked. After the unlock the window stays open if anything got through, and
the tray menu brings it back later.

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
  all that arrived, the same window still has the focus, and you have not
  typed since.

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
cargo build --release           # on Windows
cargo build --release --target x86_64-pc-windows-gnu   # from Linux, needs mingw-w64
python3 assets/make_icons.py    # regenerate the .ico files, needs Pillow
```

## Layout

- `src/layout.rs`: where each scancode sits on the board
- `src/detector.rs`: the four rules
- `src/guard.rs`: lock state, which events get swallowed, the unlock word
- `src/history.rs`: the three-second history, known shortcuts, the undo plan
- `src/sound.rs`: the synthesized harmonica
- `src/win.rs`: hook thread, tray, lock window, timeline, undo
- `assets/`: icons, cut and drawn by `make_icons.py` from `src/icon-sheet.png`

## Credit

The idea is PawSense by Chris Niswander (BitBoost, 1999). catguard shares no
code with it. The rule set started from the description in
[joeyvigil/pawsense](https://github.com/joeyvigil/pawsense) (MIT).
