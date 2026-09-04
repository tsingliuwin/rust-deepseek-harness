#!/usr/bin/env bash
# 上游（deepseek-harness）版本差异一键分析。
# 用法:
#   scripts/check-upstream.sh                     # 上次同步点 → 上游 HEAD
#   scripts/check-upstream.sh dsh-v0.1.2-rc.1     # 指定起点
#   scripts/check-upstream.sh tagA tagB           # 指定区间
# 判读顺序见 SKILL.md「模式 A」；同步点记录在项目记忆 rustdsh-sync-*-scope。
set -u
UP="${DSH_UPSTREAM:-/e/aiproject/deepseek-harness}"
FROM="${1:-dsh-v0.1.2-alpha.5}"
TO="${2:-HEAD}"
cd "$UP" || { echo "upstream repo not found: $UP"; exit 1; }

# 同步面路径（判定表见 references/upstream-analysis.md）
FACE=(
  packages/client apps/web/src packages/client/web/src
  packages/storage packages/session packages/api packages/llm
)

echo "===== [$FROM] → [$TO] ====="
echo
echo "===== 1. 发布树差异（仅同步面；为空 = 发布对 rustdsh 零功能变更）====="
git diff "$FROM" "$TO" --name-only -- "${FACE[@]}" \
  | grep -v "package\.json$" | grep -v "/README" \
  | grep -v "\.test\.\|/tests/" || echo "(空 — 同步面无变更)"
echo
echo "===== 2. 区间提交清单（no-merges；面外工作也在此可见）====="
git log --oneline "$FROM..$TO" --no-merges | cat
echo
echo "===== 3. 同步面净变更 stat ====="
git diff "$FROM..$TO" --stat -- "${FACE[@]}" 2>/dev/null \
  | grep -v "package\.json" | grep -v " README" | tail -45
echo
echo "===== 4. 词汇表 diff（zh/en locale；唯一判定 UI 文案增删）====="
git diff "$FROM..$TO" -- \
  'packages/client/*/src/client/locale*.ts' \
  'packages/client/*/src/client/locales.ts' \
  | grep -E "^[+-]  '" | head -30
[ -t 1 ] || true
echo
echo "===== 5. 存储行为 diff（格式/读写路径；仅注释措辞变更可忽略）====="
git diff "$FROM..$TO" --name-only -- \
  packages/storage packages/session/session-projection-cache \
  | grep -v test | grep -v README || echo "(无)"
