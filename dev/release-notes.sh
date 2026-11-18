#!/usr/bin/env bash
#
# The release page's notes: what changed since the last release, in Chinese,
# grouped by what kind of change it is.
#
# The notes GitHub generates are an English list of commits ending in a compare
# link, which is a pointer rather than a summary. This reads the commits since
# the last tag, groups them (新功能 / 问题修复 / 性能 / 其它) and has a model
# say each one the way a user would read it.
#
# Usage: dev/release-notes.sh [--range <from>..<to>] [--no-model]
#        dev/release-notes.sh --self-test
#
# Model settings, an OpenAI-compatible chat completions endpoint:
#   NOTES_API_BASE  default https://api.deepseek.com
#   NOTES_MODEL     default deepseek-chat
#   NOTES_API_KEY   the key; without it, GITHUB_TOKEN is used against GitHub
#                   Models (https://models.github.ai/inference) instead
# Without either, or with --no-model, the sections are still grouped and
# ordered but each line keeps its commit's English wording, with a warning: a
# release is better than no release, and a wrong translation is worse.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# The fork's own version series. Upstream's v0.7.x tags are on older history
# and are not what "since the last release" means here.
TAG_MATCH='v0.[0-9]*'

RANGE=""
NO_MODEL=false
SELF_TEST=false
while (($#)); do
  case "$1" in
    --range) RANGE="$2"; shift 2 ;;
    --no-model) NO_MODEL=true; shift ;;
    --self-test) SELF_TEST=true; shift ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
    *) echo "release-notes: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# The section a commit type belongs to, and its heading.
heading_for() {
  case "$1" in
    feat) echo "### 新功能" ;;
    fix) echo "### 问题修复" ;;
    perf) echo "### 性能" ;;
    *) echo "### 其它" ;;
  esac
}

# `type(scope): summary` into `section|type|scope|subject`, one line each, in
# the order the commits came. A type without a section of its own lands in 其它
# and keeps its name as the label. Anything not in the convention keeps its
# subject whole, also under 其它. The pattern lives in a variable because bash
# 3.2 reads an unquoted one in `[[ =~ ]]` as syntax of its own.
CONVENTIONAL='^([a-z]+)(\(([^)]*)\))?!?:[[:space:]]+(.*)$'
parse() {
  local line type scope subject section
  while IFS= read -r line; do
    [[ -n $line ]] || continue
    if [[ $line =~ $CONVENTIONAL ]]; then
      type="${BASH_REMATCH[1]}"
      scope="${BASH_REMATCH[3]}"
      subject="${BASH_REMATCH[4]}"
      # A version bump says nothing a user reads.
      [[ $type == chore && $scope == release ]] && continue
      case "$type" in
        feat | fix | perf) section="$type" ;;
        *) section="other" ;;
      esac
    else
      type=""
      scope=""
      subject="$line"
      section="other"
    fi
    # Everything lands in exactly one section, so no commit falls between two
    # headings.
    printf '%s|%s|%s|%s\n' "$section" "$type" "$scope" "$subject"
  done
}

# The parsed commits as Markdown sections, each bullet still in its commit's
# own words. Empty sections are left out rather than shown as a heading with
# nothing under it.
render() {
  local parsed section type scope subject heading label printed
  parsed="$(mktemp)"
  parse >"$parsed"
  if [[ ! -s $parsed ]]; then
    echo "本次发布没有用户可见的改动。"
    rm -f "$parsed"
    return 0
  fi
  for section in feat fix perf other; do
    heading="$(heading_for "$section")"
    printed=false
    while IFS='|' read -r seen_section type scope subject; do
      [[ $seen_section == "$section" ]] || continue
      if [[ $printed == false ]]; then
        echo "$heading"
        printed=true
      fi
      # 其它 holds several kinds of change, so it says which one when the
      # commit names no part of the app: `- **docs** …` rather than a bare line.
      label="$scope"
      if [[ $section == other && -z $label && -n $type ]]; then
        label="$type"
      fi
      if [[ -n $label ]]; then
        printf -- '- **%s** %s\n' "$label" "$subject"
      else
        printf -- '- %s\n' "$subject"
      fi
    done <"$parsed"
    [[ $printed == false ]] || echo
  done
  rm -f "$parsed"
}

SYSTEM='你是 dbdelve 的发布说明撰写者。dbdelve 是一个桌面数据库客户端，支持 Postgres、MySQL、SQLite、SQL Server、MongoDB、Redis 等。'
INSTRUCTIONS='下面是从上次发布到现在的全部提交，一行一条，格式为 `类型(范围): 摘要`，类型是 feat / fix / perf / chore / docs / refactor 等。
把它们改写成简体中文的发布说明：
- 分节只允许 `### 新功能`、`### 问题修复`、`### 性能`、`### 其它`，没有内容的小节不要输出
- 每条一行，以 `- ` 开头；摘要前能看出功能范围时，先写 `**范围**` 再空一格
- 写用户能感知到的“做了什么、怎么更好了”，不要逐字直译，也不要添加提交里没有的事实
- 不要输出一级标题、版本号、日期、链接，不要复述提交类型
- 全角标点，行尾不加句号
只输出 Markdown 正文。'

# The model's own words, or nothing at all: a caller that cannot tell a good
# answer from a broken one should not be handed half of one.
model() {
  local list key="${NOTES_API_KEY:-}" base="${NOTES_API_BASE:-https://api.deepseek.com}"
  local name="${NOTES_MODEL:-deepseek-chat}" payload body
  if [[ -z $key && -n "${GITHUB_TOKEN:-}" ]]; then
    key="$GITHUB_TOKEN"
    base="https://models.github.ai/inference"
    name="${NOTES_MODEL:-openai/gpt-4o-mini}"
  fi
  if [[ -z $key ]]; then
    echo "release-notes: no NOTES_API_KEY or GITHUB_TOKEN; keeping the commits' own words" >&2
    return 1
  fi
  command -v jq >/dev/null || {
    echo "release-notes: jq is not installed; keeping the commits' own words" >&2
    return 1
  }
  list="$(parse | while IFS='|' read -r section type scope subject; do
    if [[ -z $type ]]; then
      printf '%s\n' "$subject"
    elif [[ -n $scope ]]; then
      printf '%s(%s): %s\n' "$type" "$scope" "$subject"
    else
      printf '%s: %s\n' "$type" "$subject"
    fi
  done)"
  payload="$(jq -n --arg model "$name" --arg system "$SYSTEM" --arg instructions "$INSTRUCTIONS" --arg user "$list" \
    '{model: $model, temperature: 0.2, messages: [
       {role: "system", content: $system},
       {role: "user", content: ($instructions + "\n\n" + $user)}]}')"
  body="$(curl -fsS --max-time 180 "$base/chat/completions" \
    -H "Authorization: Bearer $key" -H 'Content-Type: application/json' \
    -d "$payload" 2>/dev/null | jq -r '.choices[0].message.content // empty' 2>/dev/null)" || body=""
  # A fence, or a title the instructions forbade, would land on the release page.
  body="$(printf '%s\n' "$body" | sed -e '/^```/d' -e '/^# /d' -e '/^## /d' | sed -e '/./,$!d' -e :a -e '/^\n*$/{$d;N;ba' -e '}')"
  [[ -n ${body//[$'\n\t ']/} ]] || {
    echo "release-notes: the model returned nothing usable; keeping the commits' own words" >&2
    return 1
  }
  printf '%s\n' "$body"
}

self_test() {
  local expected actual
  expected="$(cat <<'EOF'
### 新功能
- **grid** mark primary key columns

### 问题修复
- **ui** brighten the version badge

### 性能
- **grid** hold rows packed

### 其它
- **docs** explain the thing
- note without a type
EOF
)"
  actual="$(printf '%s\n' \
    'feat(grid): mark primary key columns' \
    'fix(ui): brighten the version badge' \
    'chore(release): bump version to 0.2.4' \
    'docs: explain the thing' \
    'perf(grid): hold rows packed' \
    'note without a type' | render | sed -e :a -e '/^$/{$d;N;ba' -e '}')"
  if [[ $actual != "$expected" ]]; then
    echo "release-notes --self-test: grouping changed" >&2
    diff <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") >&2 || true
    return 1
  fi
  echo "release-notes --self-test: ok"
}

if [[ $SELF_TEST == true ]]; then
  self_test
  exit
fi

if [[ -z $RANGE ]]; then
  LAST="$(git describe --tags --abbrev=0 --match "$TAG_MATCH" 2>/dev/null || true)"
  RANGE="${LAST:+$LAST..}HEAD"
fi
echo "release-notes: $RANGE" >&2

if [[ $NO_MODEL == false ]] && model; then
  exit 0
fi
git log --no-merges --format='%s' "$RANGE" | render
