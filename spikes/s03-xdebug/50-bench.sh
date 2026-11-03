#!/usr/bin/env bash
# S-03 step 7: what does loading Xdebug cost? Five states, 5 interleaved rounds each.
#   0 no Xdebug | 1 loaded, mode=off | 2 loaded, mode=develop
#   3 loaded, mode=debug, start_with_request=no | 4 loaded, mode=debug, start_with_request=yes, no listener
# Per state we report the MEDIAN of: bench.php wall ms (5 runs), bench.php CPU ms (5 runs),
# startup `php -n -r ''` ms (100 runs = 5 rounds x 20), PHP peak memory, and max RSS.
# Also writes the size table. The machine should be otherwise idle: load average is logged.
# Run:  ./50-bench.sh         Output: results/bench.md, results/bench-raw.tsv, results/sizes.txt
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

PHP="$WORK/php-cli/php"
SO="$WORK/out/xdebug-${PHP_VERSION%.*}-macos-arm64.so"
ROUNDS=5
STARTUPS=20
RAW="$RESULTS/bench-raw.tsv"
if lsof -nP -iTCP:9003 -sTCP:LISTEN >/dev/null 2>&1; then echo "port 9003 is busy; state 4 needs it free" >&2; exit 4; fi

state_args() {   # prints the php -d arguments of a state, one per line
  case "$1" in
    0) ;;
    1) printf '%s\n' -d "zend_extension=$SO" -d xdebug.mode=off ;;
    2) printf '%s\n' -d "zend_extension=$SO" -d xdebug.mode=develop ;;
    3) printf '%s\n' -d "zend_extension=$SO" -d xdebug.mode=debug -d xdebug.start_with_request=no ;;
    4) printf '%s\n' -d "zend_extension=$SO" -d xdebug.mode=debug -d xdebug.start_with_request=yes \
         -d xdebug.client_host=127.0.0.1 -d xdebug.client_port=9003 -d xdebug.log_level=0 ;;
  esac
}
ms_of() {   # ms_of CMD...: wall milliseconds of one command (no shell involved)
  perl -MTime::HiRes=time -e '$t = time; system(@ARGV) == 0 or exit 1; printf "%.2f\n", (time - $t) * 1000' -- "$@"
}

echo "load before: $(uptime | sed 's/.*load averages*: //')" > "$RESULTS/bench-load.txt"
printf 'state\tround\tkind\tvalue\n' > "$RAW"
for round in $(seq 1 $ROUNDS); do
  for s in 0 1 2 3 4; do
    args=(); while IFS= read -r a; do [ -n "$a" ] && args+=("$a"); done < <(state_args "$s")
    # bench.php under /usr/bin/time -l (max RSS goes to stderr)
    out="$(/usr/bin/time -l "$PHP" -n ${args[@]+"${args[@]}"} "$S03_DIR/bench.php" 2>"$WORK/time.err")"
    rss="$(awk '/maximum resident set size/ {print $1}' "$WORK/time.err")"
    ms="$(printf '%s' "$out" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["ms"])')"
    cpu="$(printf '%s' "$out" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["cpu_ms"])')"
    peak="$(printf '%s' "$out" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["peak_bytes"])')"
    while IFS=$'\t' read -r pn pv; do
      printf '%s\t%s\tpart_%s_ms\t%s\n' "$s" "$round" "$pn" "$pv" >> "$RAW"
    done < <(printf '%s' "$out" | python3 -I -c 'import json,sys; [print(k, v, sep="\t") for k, v in json.load(sys.stdin)["parts"].items()]')
    printf '%s\t%s\tbench_ms\t%s\n%s\t%s\tbench_cpu_ms\t%s\n%s\t%s\tpeak_bytes\t%s\n%s\t%s\tmax_rss_bytes\t%s\n' \
      "$s" "$round" "$ms" "$s" "$round" "$cpu" "$s" "$round" "$peak" "$s" "$round" "$rss" >> "$RAW"
    for _ in $(seq 1 $STARTUPS); do
      printf '%s\t%s\tstartup_ms\t%s\n' "$s" "$round" "$(ms_of "$PHP" -n ${args[@]+"${args[@]}"} -r '')" >> "$RAW"
    done
  done
done
echo "load after:  $(uptime | sed 's/.*load averages*: //')" >> "$RESULTS/bench-load.txt"

python3 -I - "$RAW" > "$RESULTS/bench.md" <<'PY'
import statistics, sys, collections
rows = [l.rstrip("\n").split("\t") for l in open(sys.argv[1])][1:]
d = collections.defaultdict(list)
for s, r, k, v in rows:
    d[(int(s), k)].append(float(v))
med = lambda s, k: statistics.median(d[(s, k)])
names = {0: "0 no Xdebug (baseline)", 1: "1 loaded, mode=off", 2: "2 loaded, mode=develop",
         3: "3 loaded, mode=debug, start_with_request=no", 4: "4 loaded, mode=debug, start_with_request=yes, no listener"}
print("| State | bench wall ms | vs 0 | bench CPU ms | vs 0 | startup ms | vs 0 (ms) | PHP peak MB | max RSS MB |")
print("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
for s in range(5):
    w, c, st = med(s, "bench_ms"), med(s, "bench_cpu_ms"), med(s, "startup_ms")
    print("| %s | %.1f | %+.1f%% | %.1f | %+.1f%% | %.1f | %+.1f | %.1f | %.1f |" % (
        names[s], w, (w / med(0, "bench_ms") - 1) * 100, c, (c / med(0, "bench_cpu_ms") - 1) * 100,
        st, st - med(0, "startup_ms"), med(s, "peak_bytes") / 1048576, med(s, "max_rss_bytes") / 1048576))
print()
print("Median ms per workload part (bench.php): loop = 12M iterations, no calls; calls = fib(30); arrays; strings.")
print()
print("| State | loop | calls | arrays | strings |")
print("| --- | ---: | ---: | ---: | ---: |")
for s in range(5):
    cells = []
    for p in ("loop", "calls", "arrays", "strings"):
        v = med(s, "part_%s_ms" % p); b = med(0, "part_%s_ms" % p)
        cells.append("%.0f (%.1fx)" % (v, v / b))
    print("| %s | %s |" % (names[s], " | ".join(cells)))
print()
print("Each cell is a median: bench = %d runs, startup = %d runs." % (len(d[(0, "bench_ms")]), len(d[(0, "startup_ms")])))
print("bench wall ms min..max per state: " + "; ".join("%d: %.0f..%.0f" % (s, min(d[(s, "bench_ms")]), max(d[(s, "bench_ms")])) for s in range(5)))
PY

{
  echo "== sizes (bytes)"
  stat -f '%z  %N' "$DL/php-${PHP_VERSION}-cli-macos-aarch64.tar.gz" "$DL/php-${PHP_VERSION}-fpm-macos-aarch64.tar.gz" "$PHP" "$WORK/php-fpm/php-fpm" "$SO"
  tar -czf "$WORK/out/xdebug.so.tar.gz" -C "$WORK/out" "$(basename "$SO")"
  stat -f '%z  %N (gzip of the .so, as it would be shipped)' "$WORK/out/xdebug.so.tar.gz"
  rm -f "$WORK/out/xdebug.so.tar.gz"
} > "$RESULTS/sizes.txt"

cat "$RESULTS/bench-load.txt" "$RESULTS/bench.md" "$RESULTS/sizes.txt"
