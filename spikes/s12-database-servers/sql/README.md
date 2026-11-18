# S-12a: MySQL and PostgreSQL as ready-made downloads

Throwaway scripts. They answer the SQL half of spike S-12 (the Valkey half is
in another folder): can Marsa download a MySQL 8.4 or PostgreSQL 17/18
server for the user from its source, check it, and run it without admin rights?
The answers are in [`RESULTS.md`](RESULTS.md).

**These scripts start database servers and kill processes. Run them only on a
CI runner (that is what the workflow does) or on a machine you can throw
away. Never run them on your own machine.**

## What runs where

`.github/workflows/spike-s12-sql.yml` runs on every push to a `task/S-12a-*`
branch that touches this folder, and by hand (`workflow_dispatch`). Matrix:

| Runner | Platform | Notes |
| --- | --- | --- |
| `macos-26`, `macos-15` | darwin-arm64 | `macos-14` is retired (the job is refused). |
| `macos-15-intel` | darwin-x64 | The only Intel runner left. |
| `ubuntu-24.04`, `ubuntu-22.04` | linux-x64 | glibc 2.39 and 2.35. 24.04 also checks other distributions in Docker. |
| `ubuntu-24.04-arm` | linux-arm64 | |
| `windows-2022` | windows-x64 | |

Each runner runs two jobs: `mysql` and `postgres`. Every job writes one
`<candidate>.tsv` (key and value per line), one `.log`, and a `.files` folder
with the server logs. A job ends with `results.json` and `results.md` (made by
`summarize.py`), shown in the job summary and uploaded as an artifact.

## Files

| File | What it does |
| --- | --- |
| `candidates.txt` | The table of downloads: id, platform, version, URL, checksum URL, signature URL. |
| `run.sh` | Linux and macOS: one candidate from start to finish. |
| `run.ps1` | Windows (PowerShell 7): the same steps. |
| `tool.py` | Python helpers both scripts use: millisecond clock, TCP checks, a free port, a port-holder, and a small PostgreSQL wire-protocol client (some archives have no `psql`). |
| `summarize.py` | Merges the `.tsv` files into `results.json` and `results.md`. |

## What each candidate goes through

1. Download with `curl` (no quarantine attribute, no Mark of the Web). Size and time.
2. Verify against what the source publishes (SHA-256, or MD5; GPG signature where there is one).
3. Unpack. Sizes. License files. Which programs are in the archive.
4. Inspect the binaries. macOS: `otool -L`, minimum OS version, code signature, quarantine. Linux: `ldd`, the highest glibc symbol, a `--version` run in Ubuntu 22.04, Debian 12, AlmaLinux 9, and Alpine containers. Windows: Authenticode, bundled runtime DLLs.
5. Init a data directory in a temp folder (`mysqld --initialize-insecure`, `initdb -A trust`).
6. Start in the foreground, as a child process, bound to `127.0.0.1` on a free high port. Check the listening sockets. Run a smoke query (with an Arabic string). Measure memory.
7. Stop it in every way a supervisor could, with one idle client connected, and start it again after each: the vendor command, `SIGTERM`, `SIGINT`, `SIGQUIT`, `SIGKILL` (Windows: `pg_ctl` modes and `TerminateProcess`).
8. Start while another program holds the port. Start on the default port (3306 or 5432).
9. Memory again with a small config (`performance_schema=OFF`, a small buffer pool; `shared_buffers=32MB`).
10. Orphan test: start the server under a parent, kill the parent with `SIGKILL`. Look at the pid file, socket, port, and what a second start does. Stop the orphan through its pid file. Then kill the parent and the server together and start again.

## Run it by hand

On a throwaway Linux or macOS machine:

```sh
bash run.sh theseus-pg17 /tmp/s12-out     # then look at /tmp/s12-out/theseus-pg17.tsv
python3 summarize.py /tmp/s12-out
```

On a throwaway Windows machine, in PowerShell 7:

```powershell
./run.ps1 -Id theseus-pg17 -Out C:\s12-out
```

Candidate ids are in the first column of `candidates.txt`. A candidate that
does not exist for the current platform writes `status: no candidate for this
platform` and stops.
