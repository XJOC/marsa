#!/usr/bin/env python3
"""Two ways for a root helper to give a file `cap_net_bind_service` without
following a link that the user planted. Run with sudo.

usage: cap-probe.py proc-fd <path>   open with O_PATH|O_NOFOLLOW, check the
                                     open file, then run setcap on
                                     /proc/<pid>/fd/<n>
       cap-probe.py xattr <path>     open with O_RDONLY|O_NOFOLLOW|O_NONBLOCK,
                                     check the open file, then write the
                                     security.capability attribute through
                                     the file descriptor (fsetxattr)
"""

import os
import stat
import struct
import subprocess
import sys

SETCAP = "/usr/sbin/setcap"
CAP_NET_BIND_SERVICE = 10
# struct vfs_cap_data, revision 2, effective bit set: magic_etc, then two
# pairs of (permitted, inheritable), all little-endian u32.
VFS_CAP_REVISION_2 = 0x02000000
VFS_CAP_FLAGS_EFFECTIVE = 0x000001
VALUE = struct.pack(
    "<IIIII",
    VFS_CAP_REVISION_2 | VFS_CAP_FLAGS_EFFECTIVE,
    1 << CAP_NET_BIND_SERVICE,
    0,
    0,
    0,
)


def describe(fd):
    info = os.fstat(fd)
    regular = stat.S_ISREG(info.st_mode)
    print(f"opened: mode={oct(info.st_mode)} uid={info.st_uid} regular={regular}")
    return regular


def main():
    way, path = sys.argv[1], sys.argv[2]
    if way == "proc-fd":
        fd = os.open(path, os.O_PATH | os.O_NOFOLLOW)
        if not describe(fd):
            print("refused: not a regular file")
            return 3
        target = f"/proc/{os.getpid()}/fd/{fd}"
        done = subprocess.run(
            [SETCAP, "cap_net_bind_service=+ep", target],
            capture_output=True,
            text=True,
            check=False,
        )
        print(f"setcap {target}: exit {done.returncode} {done.stdout.strip()} {done.stderr.strip()}")
        return done.returncode
    if way == "xattr":
        try:
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        except OSError as err:
            print(f"refused at open: {err.strerror} (errno {err.errno})")
            return 3
        if not describe(fd):
            print("refused: not a regular file")
            return 3
        try:
            os.setxattr(fd, "security.capability", VALUE)
        except OSError as err:
            print(f"fsetxattr failed: {err.strerror} (errno {err.errno})")
            return 1
        print("fsetxattr security.capability: ok")
        return 0
    print(__doc__)
    return 2


sys.exit(main())
