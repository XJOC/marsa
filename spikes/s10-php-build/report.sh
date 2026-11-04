#!/usr/bin/env bash
# S-10: write the job summary for one platform and one table row for the final report.
#
# Environment (set by the workflow):
#   LABEL            e.g. macos-arm64
#   BUILD_OUTCOME    success | failure | cancelled | skipped
#   SMOKE_OUTCOME    same
#   LARAVEL_OUTCOME  same
#   OUT, WORK        folders (defaults next to this script)
# Writes $OUT/row-$LABEL.md (one table row) and appends to $GITHUB_STEP_SUMMARY
# (or prints to the terminal when that variable is not set).
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${OUT:-$HERE/out}"
WORK="${WORK:-$HERE/work}"
LABEL="${LABEL:?LABEL is required}"
BR="$WORK/buildroot"
SUMMARY="${GITHUB_STEP_SUMMARY:-/dev/stdout}"
mkdir -p "$OUT"

bytes() { if [ -f "$1" ]; then wc -c < "$1" | tr -d ' '; else echo ""; fi; }
mb() { if [ -n "$1" ]; then awk -v b="$1" 'BEGIN { printf "%.1f", b / 1048576 }'; else echo "-"; fi; }
mins() { if [ -n "$1" ]; then awk -v s="$1" 'BEGIN { printf "%.1f", s / 60 }'; else echo "-"; fi; }
ok() { case "${1:-skipped}" in success) echo "ok" ;; skipped) echo "-" ;; *) echo "FAILED" ;; esac; }

T_DOCTOR="" T_DOWNLOAD="" T_BUILD="" T_TOTAL=""
# shellcheck disable=SC1091
[ -f "$OUT/timings.txt" ] && . "$OUT/timings.txt"

php_b="$(bytes "$BR/bin/php")"
fpm_b="$(bytes "$BR/bin/php-fpm")"
xd_b="$(bytes "$BR/modules/xdebug.so")"
tgz_b="$(bytes "$(ls "$OUT"/dist/php-*.tar.gz 2> /dev/null | head -1)")"
pass_fail="$(cat "$OUT/smoke-status.txt" 2> /dev/null || echo '-')"
phpver="$(head -1 "$OUT/php-v.txt" 2> /dev/null | awk '{print $2}')"

# One row for the combined table: label | PHP | build | smoke | laravel | php MB | fpm MB | tar.gz MB | xdebug.so MB | doctor | download | build | total (minutes)
echo "| $LABEL | ${phpver:--} | $(ok "${BUILD_OUTCOME:-}") | $(ok "${SMOKE_OUTCOME:-}") ($pass_fail) | $(ok "${LARAVEL_OUTCOME:-}") | $(mb "$php_b") | $(mb "$fpm_b") | $(mb "$tgz_b") | $(mb "$xd_b") | $(mins "$T_DOCTOR") | $(mins "$T_DOWNLOAD") | $(mins "$T_BUILD") | $(mins "$T_TOTAL") |" > "$OUT/row-$LABEL.md"

{
  echo "## $LABEL"
  echo
  echo "| step | outcome |"
  echo "| --- | --- |"
  echo "| build | $(ok "${BUILD_OUTCOME:-}") |"
  echo "| smoke tests | $(ok "${SMOKE_OUTCOME:-}") ($pass_fail) |"
  echo "| Laravel create-project, migrate, test | $(ok "${LARAVEL_OUTCOME:-}") |"
  echo
  echo "| size | MB (bytes) |"
  echo "| --- | --- |"
  echo "| php (cli) | $(mb "$php_b") ($php_b) |"
  echo "| php-fpm | $(mb "$fpm_b") ($fpm_b) |"
  echo "| tar.gz of php + php-fpm | $(mb "$tgz_b") ($tgz_b) |"
  echo "| xdebug.so | $(mb "$xd_b") ($xd_b) |"
  echo
  echo "| time (minutes) | |"
  echo "| --- | --- |"
  echo "| spc doctor | $(mins "$T_DOCTOR") |"
  echo "| spc download | $(mins "$T_DOWNLOAD") |"
  echo "| spc build | $(mins "$T_BUILD") |"
  echo "| total | $(mins "$T_TOTAL") |"
  echo
  if [ -s "$OUT/modules-missing.txt" ]; then echo "Modules missing from php -m: \`$(tr '\n' ' ' < "$OUT/modules-missing.txt")\`"; echo; fi
  if [ -s "$OUT/modules-extra.txt" ]; then echo "Modules in php -m but not in the list: \`$(tr '\n' ' ' < "$OUT/modules-extra.txt")\`"; echo; fi
  if [ -f "$OUT/php-v.txt" ]; then echo '```'; cat "$OUT/php-v.txt"; echo '```'; echo; fi
  if [ -f "$OUT/smoke.md" ]; then
    echo "<details><summary>Smoke test lines</summary>"
    echo
    cat "$OUT/smoke.md"
    echo
    echo "</details>"
    echo
  fi
  if [ -f "$OUT/distros.md" ]; then
    echo "Other Linux distributions (docker):"
    echo
    cat "$OUT/distros.md"
    echo
  fi
  if [ -f "$OUT/laravel.md" ]; then
    echo "Laravel check:"
    echo
    sed 's/^/- /' "$OUT/laravel.md"
    echo
  fi
} >> "$SUMMARY"
