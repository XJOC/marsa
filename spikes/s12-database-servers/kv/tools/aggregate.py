"""Print one markdown report from the downloaded artifacts of every cell.

    python3 -I aggregate.py ARTIFACTS_DIR

ARTIFACTS_DIR holds one folder per cell (kv-<label>), each with facts.txt and
laravel-<label>.jsonl.
"""
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
KEYS = [
    "source", "sha256_match_pinned", "sha256_match_api", "sha256_match_winget_manifest", "sha256_match_noble",
    "sha256_match_jammy", "sha256_source_match", "archive_bytes", "size_archive_server_cli_bytes",
    "unpacked_bytes", "size_unpacked_server_cli_bytes", "build_seconds", "start_to_ping_ms",
    "rss_kb_ps_empty", "working_set_kb_empty", "private_bytes_kb_empty", "listening", "stop_cli_ms",
    "stop_cli_exit_code", "stop_sigterm_ms", "stop_shutdown_ms", "orphan_server_alive", "orphan_port_held",
    "orphan_second_start", "persist_recovered_value", "ldd", "otool_L", "xattr",
]
print("# S-12b key-value spike: all cells\n")
rows = []
for d in sorted(p for p in root.iterdir() if p.is_dir()):
    facts = {}
    for f in d.rglob("facts.txt"):
        for line in f.read_text(errors="replace").splitlines():
            if "=" in line:
                k, v = line.split("=", 1)
                facts[k] = v
    results = []
    for j in d.rglob("laravel-*.jsonl"):
        for line in j.read_text(errors="replace").splitlines():
            try:
                results.append(json.loads(line))
            except ValueError:
                pass
    ok = sum(1 for r in results if r["ok"])
    bad = [r for r in results if not r["ok"]]
    print(f"## {d.name}\n")
    print(f"Laravel checks: {ok} passed, {len(bad)} failed ({len(results)} total)\n")
    for r in bad:
        print(f"- FAIL `{r['stage']}/{r['name']}`: {r['detail'][:200]}")
    print()
    for k in KEYS:
        if k in facts:
            print(f"- {k}: {facts[k][:300]}")
    print()
