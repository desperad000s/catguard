# catguard

Locks the keyboard when a cat steps on it. Runs in the Windows tray, plays a
harmonica at the cat, and unlocks when you type `human`.

The mouse and the touchpad stay free. One 280 KB exe, no installer, no
runtime, no network access, no files written.

## Use it

Start `catguard.exe`. A shield icon appears in the tray. Its menu has three
entries: pause, start with Windows, exit.

When a paw lands, a small window says so and the keyboard goes dead. Type
`human` (you do not need to click anything first) or click the button. Keys
the cat is still standing on stay dead until it lets go.

`Ctrl+Alt+Del` always works. Windows does not let any program intercept it.

## How it tells a paw from a hand

A finger presses one key. A paw covers two key units and presses everything
under it in the same instant. catguard looks only at which physical keys are
down and when they went down. It never sees characters, so the keyboard
layout does not matter, and there is nothing to log.

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
  may ask questions about an unsigned build. Read `src/win.rs`: the hook
  passes scancodes and timestamps to the detector and keeps nothing.

## Build

```
cargo test                      # the detection core, on any OS
cargo build --release           # on Windows
cargo build --release --target x86_64-pc-windows-gnu   # from Linux, needs mingw-w64
```

## Layout

- `src/layout.rs`: where each scancode sits on the board
- `src/detector.rs`: the four rules
- `src/guard.rs`: lock state, which events get swallowed, the unlock word
- `src/sound.rs`: the synthesized harmonica
- `src/win.rs`: hook thread, tray, lock window

## Credit

The idea is PawSense by Chris Niswander (BitBoost, 1999). catguard shares no
code with it. The rule set started from the description in
[joeyvigil/pawsense](https://github.com/joeyvigil/pawsense) (MIT).
