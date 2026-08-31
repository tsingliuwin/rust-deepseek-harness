#!/usr/bin/env bash
# Build and restart the gpui host (dsh-gpui.exe).
#
# The "redeploy" step of the use-check-fix-use loop: called by the agent
# after harness changes, no manual close/build/open for the user. Three
# phases:
#   1. safety: wait while a session file was written within the last 15s
#      (a live conversation is streaming — do not cut it);
#   2. force-stop the running host (the exe is locked by its process);
#   3. rebuild, relaunch, verify the process is up.
# Usage: scripts/restart-host.sh
set -u
cd "$(dirname "$0")/.."

EXE=target/debug/dsh-gpui.exe
WINPWD=$(pwd -W 2>/dev/null || pwd)

# 1) session quiesce check (up to 45s)
for i in 1 2 3; do
  recent=$(find "$HOME/.dsh/sessions" -name 'session.jsonl.zstd' -newermt '-15 seconds' 2>/dev/null | head -1)
  if [ -z "$recent" ]; then break; fi
  echo "session still streaming ($recent), waiting 15s..."
  sleep 15
done

# 2) stop the old host (ignore if not running)
if tasklist 2>/dev/null | grep -qi dsh-gpui; then
  echo "stopping running host..."
  taskkill //F //IM dsh-gpui.exe >/dev/null 2>&1 || taskkill /F /IM dsh-gpui.exe >/dev/null 2>&1
  sleep 1
fi

# 3) build + launch
echo "building..."
cargo build -q -p dsh-gpui || { echo "build failed"; exit 1; }
echo "launching..."
# exe 是 windows 子系统（无控制台窗口）；不可加 -WindowStyle Hidden——
# gpui 首窗口用 SW_SHOWDEFAULT，会继承 SW_HIDE 把 GUI 也藏掉
powershell -NoProfile -Command "Start-Process -FilePath '$WINPWD\\target\\debug\\dsh-gpui.exe' -WorkingDirectory '$WINPWD'" || exit 1
sleep 3
if tasklist 2>/dev/null | grep -qi dsh-gpui; then
  echo "OK: host running ($(ls -la $EXE | awk '{print $6, $7, $8}'))"
else
  echo "launch failed: process not found"; exit 1
fi
