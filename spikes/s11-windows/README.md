# S-11: Windows spikes for the Phase 1 gate

Throwaway code. It answers the Windows questions of `docs/architecture.md`
section 13 on a GitHub runner (`windows-2025`), because the owner has no
Windows machine. The workflow is `.github/workflows/spike-s11.yml`; every
script writes a results file that the run uploads and shows in its summary.

The spike is over. Its results are in three decisions: ADR 0016 (php-cgi
pool), ADR 0017 (Job Object and a start without inherited handles), and ADR
0018 (the helper service, the named pipe, the hosts file, and trust). The
runs they cite are 42920002335, 42923418104, and 42962161258. The workflow
now runs only by hand (`workflow_dispatch`).

| Script | Question | Changes the machine? |
| --- | --- | --- |
| `scripts/php-cgi-load.ps1` | Are 4 php-cgi processes enough under concurrent requests? Is one php-cgi with `PHP_FCGI_CHILDREN` better than several behind Caddy? What happens when php-cgi exits after its request limit? Which DLLs does the official PHP need? | No |
| `scripts/job-and-handles.ps1` | Does a kill-on-close Job Object take the whole child tree when the parent is killed? Does `CREATE_NO_WINDOW` keep console windows away? Does a detached child keep the caller's stdout pipe open? | No |
| `scripts/pipe-service-trust.ps1` | Does the pipe ACL keep another local user out? What does a LocalSystem service see? How should it write the hosts file? Can it add the CA to the machine Root store, and how is the CA removed? | **Yes, and it needs admin rights.** A local user, a service, the hosts file, a root certificate. It removes them at the end. Run it only on a throwaway machine. |

## The programs

| Program | What it does |
| --- | --- |
| `cgipool` | Stands in for the supervisor: keeps `n` php-cgi processes (or one with `n` children) running and logs every start and exit. |
| `loadgen` | A small HTTP load generator, `std` only. |
| `jobtree`, `jobtreew` | A parent with a chain of three children. `jobtreew` is built for the windows subsystem, like `marsad`. |
| `inherit` | Starts a detached child the way `spawn_marsad` does. |
| `pipesvc` | The pipe server (foreground or as a service), its client, and a squatter. |

## Run it by hand

On a Windows machine, in PowerShell 7, from this folder:

```powershell
cargo build --release
./scripts/job-and-handles.ps1 -Bin target/release -Out out
./scripts/php-cgi-load.ps1 -Bin target/release -Out out
```

`pipe-service-trust.ps1` is for a throwaway machine only (see the table).
