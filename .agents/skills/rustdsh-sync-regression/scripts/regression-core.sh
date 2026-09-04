#!/usr/bin/env bash
# 完整回归第 1 层：构建 + 单测 + 真实语料互通。零 UI、可随时跑。
# 用法: scripts/regression-core.sh
set -u
cd "$(dirname "$0")/../../.." || exit 1
ROOT=$(pwd -W 2>/dev/null || pwd)
FAIL=0

echo "===== 1. cargo test --workspace ====="
cargo test --workspace 2>&1 | grep -E "test result" | awk '{s+=$4; f+=$6} END {print "passed="s" failed="f; if (f>0) exit 1}' || FAIL=1

echo
echo "===== 2. 真实 ~/.dsh 语料：会话加载 ====="
cargo run -q --example load_web -p dsh-persist 2>&1 | head -4 || FAIL=1

echo
echo "===== 3. 真实 ~/.dsh 语料：projcache 标题互通 ====="
cargo run -q --example share_check -p dsh-persist 2>&1 | tail -4 || FAIL=1

echo
if [ "$FAIL" -eq 0 ]; then echo "REGRESSION-CORE: ALL GREEN"; else echo "REGRESSION-CORE: FAILED"; exit 1; fi
