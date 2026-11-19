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
#        dev/release-notes.sh --prompt     # the exact prompt, to hand to any model
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
#
# With no key at all -- which is this repository's setup -- the notes are
# written by whatever model the release is cut with: `--prompt` prints exactly
# what this script would have sent, the reply goes to a file, and that file is
# the `notes` input of the release workflow.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# The fork's own version series. Upstream's v0.7.x tags are on older history
# and are not what "since the last release" means here.
TAG_MATCH='v0.[0-9]*'

RANGE=""
NO_MODEL=false
PROMPT=false
SELF_TEST=false
while (($#)); do
  case "$1" in
    --range) RANGE="$2"; shift 2 ;;
    --no-model) NO_MODEL=true; shift ;;
    --prompt) PROMPT=true; shift ;;
    --self-test) SELF_TEST=true; shift ;;
    -h|--help) sed -n '2,27p' "$0"; exit 0 ;;
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
      # 其它 holds several kinds of change, so it says which one. A `test` or
      # `ci` commit's scope names a part of the app it did not change, so those
      # are labelled by their kind; the rest by the part of the app they did.
      label="$scope"
      if [[ $section == other ]]; then
        case "$type" in
          test | ci) label="$type" ;;
          *) [[ -n $label ]] || label="$type" ;;
        esac
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

# The commits in `$RANGE` as the model reads them: each line a
# `type(scope): summary`, with the commit's body indented under it. A subject is
# the one line its author had room for; the body is where the change actually
# says what it did.
records() {
  local record subject body
  git log --no-merges --format='%s%x1f%b%x1e' "$RANGE" |
    while IFS= read -r -d $'\x1e' record; do
      # `git log --format` follows each record's `%x1e` with a newline of its
      # own, so every record after the first arrives with one in front of it.
      while [[ $record == $'\n'* ]]; do
        record="${record#$'\n'}"
      done
      subject="${record%%$'\x1f'*}"
      body="${record#*$'\x1f'}"
      [[ -n $subject ]] || continue
      # A version bump says nothing a user reads.
      if [[ $subject =~ $CONVENTIONAL ]] &&
        [[ ${BASH_REMATCH[1]} == chore && ${BASH_REMATCH[3]} == release ]]; then
        continue
      fi
      printf '%s\n' "$subject"
      if [[ -n ${body//[$'\n\t ']/} ]]; then
        printf '%s\n' "$body" | sed -e '/^[[:space:]]*$/d' -e 's/^/    /'
      fi
      printf '\n'
    done
}

SYSTEM='你是 dbdelve 的发布说明撰写者。dbdelve 是一个桌面数据库客户端，支持 Postgres、MySQL、SQLite、SQL Server、MongoDB、Redis 等。'
INSTRUCTIONS='下面是从上次发布到现在的全部提交。每条以一行 `类型(范围): 摘要` 开始，紧跟其后缩进的行是这条提交的正文，可据此写得更具体。
把它们改写成简体中文的发布说明：
- 分节只允许 `### 新功能`、`### 问题修复`、`### 性能`、`### 其它`，没有内容的小节不要输出
- 每条一行，以 `- ` 开头；摘要前能看出功能范围时，先写 `**范围**` 再空一格
- 写用户能感知到的“做了什么、怎么更好了”，不要逐字直译，也不要添加提交里没有的事实
- `test` / `ci` / `chore` 这类只影响开发与测试的提交归到 `其它`，一条概括即可，不要逐条罗列
- 范围是 `release`（打包与发布脚本）的提交也归到 `其它`，不要写成新功能或问题修复
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
  list="$(records)"
  [[ -n ${list//[$'\n\t ']/} ]] || {
    echo "release-notes: nothing to summarise in $RANGE" >&2
    return 1
  }
  payload="$(jq -n --arg model "$name" --arg system "$SYSTEM" --arg instructions "$INSTRUCTIONS" --arg user "$list" \
    '{model: $model, temperature: 0.2, messages: [
       {role: "system", content: $system},
       {role: "user", content: ($instructions + "\n\n" + $user)}]}')"
  # The answer and the status apart: `curl -f` says only that it failed, and a
  # fallback nobody can explain is a fallback nobody fixes. What is logged is
  # the endpoint's own words, which are GitHub's or the model host's, and the
  # secret is in a header rather than in this body.
  local answer error status
  answer="$(mktemp)"
  error="$(mktemp)"
  status="$(curl -sS --max-time 180 -o "$answer" -w '%{http_code}' "$base/chat/completions" \
    -H "Authorization: Bearer $key" -H 'Content-Type: application/json' \
    -d "$payload" 2>"$error" || true)"
  if [[ $status != 200 ]]; then
    echo "release-notes: the model endpoint answered ${status:-nothing}: $(head -c 400 "$error" 2>/dev/null)$(head -c 400 "$answer" 2>/dev/null)" >&2
    rm -f "$answer" "$error"
    return 1
  fi
  body="$(jq -r '.choices[0].message.content // empty' <"$answer" 2>/dev/null)"
  # A fence, or a title the instructions forbade, would land on the release page.
  body="$(printf '%s\n' "$body" | sed -e '/^```/d' -e '/^# /d' -e '/^## /d' | sed -e '/./,$!d' -e :a -e '/^\n*$/{$d;N;ba' -e '}')"
  if [[ -z ${body//[$'\n\t ']/} ]]; then
    # Not JSON, or JSON with no content under it: a proxy or a filter answering
    # in the endpoint's place looks exactly like this, and the words it said are
    # the only way to tell. models.github.ai answers a bare `OK` to every path
    # from some networks, `GITHUB_TOKEN` or not, which is why the key remains
    # the route to trust.
    echo "release-notes: the model returned nothing usable, and answered: $(head -c 300 "$answer" 2>/dev/null)" >&2
    echo "release-notes: set the NOTES_API_KEY secret to take the OpenAI-compatible route instead" >&2
    rm -f "$answer" "$error"
    return 1
  fi
  rm -f "$answer" "$error"
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
- **test** cover the thing
- note without a type
EOF
)"
  actual="$(printf '%s\n' \
    'feat(grid): mark primary key columns' \
    'fix(ui): brighten the version badge' \
    'chore(release): bump version to 0.2.4' \
    'docs: explain the thing' \
    'test(grid): cover the thing' \
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

# The whole request, so a model reached some other way -- a chat, an agent, a
# CLI -- is handed exactly what the HTTP call would have sent.
if [[ $PROMPT == true ]]; then
  printf '%s\n\n%s\n\n%s\n' "$SYSTEM" "$INSTRUCTIONS" "$(records)"
  exit 0
fi

if [[ $NO_MODEL == false ]] && model; then
  exit 0
fi
git log --no-merges --format='%s' "$RANGE" | render
