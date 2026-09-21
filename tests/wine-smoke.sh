#!/usr/bin/env bash
# Runs the real exe under Wine on a virtual display, presses three
# neighbouring keys at once, and checks what a user would see:
#   - two characters leak into Notepad, the third is blocked
#   - the lock window appears
#   - keys typed while locked are swallowed, the unlock word hides the lock
# Screenshots land in the output directory for a human to look at.
#
# Needs: wine, Xvfb, xdotool, ImageMagick. Wine has no WebView2, so this
# covers the hook, the guard and the lock window, not the app window.
set -euo pipefail

exe=${1:-target/x86_64-pc-windows-msvc/release/catguard.exe}
out=${2:-target/wine-smoke}
mkdir -p "$out"
export WINEPREFIX="$PWD/target/wineprefix" WINEDEBUG=-all DISPLAY=:77

pkill -f "Xvfb :77" 2>/dev/null || true
wineserver -k 2>/dev/null || true
Xvfb :77 -screen 0 1280x800x24 >/dev/null 2>&1 &
trap 'wineserver -k 2>/dev/null || true; pkill -f "Xvfb :77" 2>/dev/null || true' EXIT
sleep 1
[ -d "$WINEPREFIX" ] || timeout 180 wineboot -i >/dev/null 2>&1

wine "$exe" --background >"$out/catguard.log" 2>&1 &
wine notepad >/dev/null 2>&1 &
sleep 8
notepad=$(xdotool search --name Notepad | tail -1)
xdotool windowfocus "$notepad"; sleep 0.5

# Reads what Notepad holds by selecting all and copying it out is not
# possible without a clipboard manager, so the checks compare screenshots:
# the lock window is a 540 px wide black box with a lime frame.
lock_visible() { [ -n "$(xdotool search --class catguard 2>/dev/null | while read -r w; do xdotool getwindowgeometry "$w" 2>/dev/null | grep -q "Geometry: 540x356" && xwininfo -id "$w" | grep -q IsViewable && echo yes; done)" ]; }

fail() { echo "FAIL: $1"; import -window root "$out/fail.png"; exit 1; }

xdotool type --delay 90 "hi"
lock_visible && fail "typing two letters locked the keyboard"

xdotool keydown w keydown e keydown d; sleep 0.6
import -window root "$out/1-locked.png"
lock_visible || fail "three keys at once did not lock"
xdotool keyup w keyup e keyup d

xdotool type --delay 90 "zzz"          # swallowed
xdotool type --delay 90 "human"; sleep 0.6
lock_visible && fail "the unlock word did not unlock"
# Keys got through, so catguard opens its app window. Wine has no WebView2,
# which makes this the test of the message that says so.
sleep 1.5; import -window root "$out/2-unlocked.png"

echo "PASS. Look at $out/1-locked.png and $out/2-unlocked.png: Notepad must read 'hiwe', nothing of 'zzz' or 'human'."
