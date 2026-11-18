#!/usr/bin/env python3
"""Merge the <id>.tsv files of one job into results.json and results.md.

  summarize.py <out-dir>

Prints the markdown too, so a workflow can append it to $GITHUB_STEP_SUMMARY.
"""
import json
import pathlib
import sys

KEY_ORDER = [
    "version", "status", "url", "download_ok", "archive_bytes", "checksum_kind", "checksum_ok",
    "gpg_status", "unpacked_bytes", "unpacked_files", "client_tools", "license_files",
    "server_version_output", "minos", "archs", "quarantine_xattrs", "codesign", "dylib_other_absolute",
    "ldd_missing", "glibc_needed_max", "glibcxx_needed_max",
]


def mb(value):
    try:
        return "%.1f MB" % (int(value) / 1_000_000)
    except (TypeError, ValueError):
        return value or "-"


def load(path):
    data = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if "\t" in line:
            key, value = line.split("\t", 1)
            data[key] = value
    return data


def main(out):
    out = pathlib.Path(out)
    runs = {p.stem: load(p) for p in sorted(out.glob("*.tsv"))}
    (out / "results.json").write_text(json.dumps(runs, indent=2, ensure_ascii=False), encoding="utf-8")
    lines = []
    lines.append("| candidate | version | archive | unpacked | checksum | start ok | RSS idle | loopback only | status |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for name, d in runs.items():
        rss = d.get("rss_idle_kb")
        rss = "%.0f MB" % (int(rss) / 1024) if rss and rss.isdigit() else "-"
        lines.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
            name, d.get("version", "-"), mb(d.get("archive_bytes")), mb(d.get("unpacked_bytes")),
            d.get("checksum_ok", "-"), d.get("start_first_ready_ms", "-"), rss,
            d.get("loopback_only", "-"), d.get("status", "-")))
    lines.append("")
    for name, d in runs.items():
        lines.append("<details><summary>%s (%s)</summary>\n" % (name, d.get("platform", "?")))
        lines.append("| key | value |\n| --- | --- |")
        keys = [k for k in KEY_ORDER if k in d] + [k for k in d if k not in KEY_ORDER]
        for k in keys:
            lines.append("| %s | %s |" % (k, d[k].replace("|", "\\|")))
        lines.append("\n</details>\n")
    text = "\n".join(lines)
    (out / "results.md").write_text(text, encoding="utf-8")
    print(text)


if __name__ == "__main__":
    main(sys.argv[1])
