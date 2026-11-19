# Death matrix (macOS arm64, Caddy v2.11.7)

State of Caddy after the parent died: 1 s later, after one request, and 5 s later.
`alive(ppid=1)` means launchd adopted it. `dead` means it exited with its parent.

| Case | group | stdio | kill_on_drop | +1 s | after request | +5 s | notes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| D1 | inherit | file | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D1 | inherit | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D1 | inherit | pipe | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D1 | inherit | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D1 | own | file | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D1 | own | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D1 | own | pipe | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D1 | own | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D2 | inherit | file | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D2 | inherit | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D2 | inherit | pipe | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D2 | inherit | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D2 | own | file | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D2 | own | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D2 | own | pipe | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D2 | own | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D3 | inherit | file | on | dead | dead | dead | |
| D3 | inherit | file | off | dead | dead | dead | |
| D3 | inherit | pipe | on | dead | dead | dead | |
| D3 | inherit | pipe | off | dead | dead | dead | |
| D3 | own | file | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D3 | own | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D3 | own | pipe | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D3 | own | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D4 | inherit | file | on | dead | dead | dead | panic message printed; |
| D4 | inherit | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D4 | inherit | pipe | on | dead | dead | dead | panic message printed; |
| D4 | inherit | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D4 | own | file | on | dead | dead | dead | panic message printed; |
| D4 | own | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D4 | own | pipe | on | dead | dead | dead | panic message printed; |
| D4 | own | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D5 | inherit | file | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D5 | inherit | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D5 | inherit | pipe | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D5 | inherit | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D5 | own | file | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D5 | own | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D5 | own | pipe | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D5 | own | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | panic message printed; |
| D6 | inherit | file | on | dead | dead | dead | |
| D6 | inherit | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D6 | inherit | pipe | on | dead | dead | dead | |
| D6 | inherit | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D6 | own | file | on | dead | dead | dead | |
| D6 | own | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D6 | own | pipe | on | dead | dead | dead | |
| D6 | own | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D7 | inherit | file | on | dead | dead | dead | daemon: join=panic caddy_alive_right_after=false; parent after 5s: alive(ppid=14797); panic message printed; |
| D7 | inherit | file | off | alive(ppid=29564) | alive(ppid=29564) | alive(ppid=29564) | daemon: join=panic caddy_alive_right_after=true; parent after 5s: alive(ppid=14797); panic message printed; |
| D7 | inherit | pipe | on | dead | dead | dead | daemon: join=panic caddy_alive_right_after=false; parent after 5s: alive(ppid=14797); panic message printed; |
| D7 | inherit | pipe | off | alive(ppid=29904) | alive(ppid=29904) | alive(ppid=29904) | daemon: join=panic caddy_alive_right_after=true; parent after 5s: alive(ppid=14797); panic message printed; |
| D7 | own | file | on | dead | dead | dead | daemon: join=panic caddy_alive_right_after=false; parent after 5s: alive(ppid=14797); panic message printed; |
| D7 | own | file | off | alive(ppid=30198) | alive(ppid=30198) | alive(ppid=30198) | daemon: join=panic caddy_alive_right_after=true; parent after 5s: alive(ppid=14797); panic message printed; |
| D7 | own | pipe | on | dead | dead | dead | daemon: join=panic caddy_alive_right_after=false; parent after 5s: alive(ppid=14797); panic message printed; |
| D7 | own | pipe | off | alive(ppid=30532) | alive(ppid=30532) | alive(ppid=30532) | daemon: join=panic caddy_alive_right_after=true; parent after 5s: alive(ppid=14797); panic message printed; |
| D8 | inherit | file | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D8 | inherit | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D8 | inherit | pipe | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D8 | inherit | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D8 | own | file | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D8 | own | file | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D8 | own | pipe | on | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
| D8 | own | pipe | off | alive(ppid=1) | alive(ppid=1) | alive(ppid=1) | |
