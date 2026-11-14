#!/usr/bin/env python3
"""Small helpers for the S-12 SQL spike. Python standard library only.

Used by run.sh and run.ps1 so that both scripts do the same thing on every
platform:

  tool.py ms                          print the time in milliseconds
  tool.py tcp HOST PORT               exit 0 if a TCP connection works
  tool.py wait HOST PORT SECONDS      wait until a TCP connection works
  tool.py hold-port HOST PORT         listen on a port and sit there (a "taken port")
  tool.py pgquery HOST PORT USER DB SQL...
                                      PostgreSQL wire protocol 3.0, trust auth,
                                      simple query. Prints one line per row.
  tool.py pgwait HOST PORT SECONDS    wait until `select 1` works
  tool.py pghold HOST PORT USER DB    connect, then idle (an idle client session)
  tool.py freeport                    print a free TCP port on 127.0.0.1

The PostgreSQL client is here because some archives (zonky) have no psql.
"""
import socket
import struct
import sys
import time


def ms():
    print(int(time.time() * 1000))


def tcp_ok(host, port, timeout=1.0):
    try:
        with socket.create_connection((host, int(port)), timeout=timeout):
            return True
    except OSError:
        return False


def wait(host, port, seconds):
    end = time.time() + float(seconds)
    while time.time() < end:
        if tcp_ok(host, port):
            return 0
        time.sleep(0.2)
    return 1


def hold_port(host, port):
    s = socket.socket()
    s.bind((host, int(port)))
    s.listen(5)
    print("holding", host, port, flush=True)
    while True:
        time.sleep(60)


def _read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("connection closed by the server")
        buf += chunk
    return buf


def _read_msg(sock):
    kind = _read_exact(sock, 1)
    (length,) = struct.unpack("!I", _read_exact(sock, 4))
    return kind, _read_exact(sock, length - 4)


def _error_text(body):
    fields = {}
    for part in body.split(b"\0"):
        if part:
            fields[part[:1]] = part[1:].decode("utf-8", "replace")
    return "%s: %s" % (fields.get(b"S", "ERROR"), fields.get(b"M", "?"))


def pg_connect(host, port, user, db):
    sock = socket.create_connection((host, int(port)), timeout=10)
    params = b"user\0" + user.encode() + b"\0database\0" + db.encode() + b"\0\0"
    payload = struct.pack("!I", 196608) + params
    sock.sendall(struct.pack("!I", len(payload) + 4) + payload)
    while True:
        kind, body = _read_msg(sock)
        if kind == b"R":
            (code,) = struct.unpack("!I", body[:4])
            if code != 0:
                raise RuntimeError("server asks for authentication method %d" % code)
        elif kind == b"E":
            raise RuntimeError(_error_text(body))
        elif kind == b"Z":
            return sock


def pg_query(sock, sql):
    data = sql.encode("utf-8") + b"\0"
    sock.sendall(b"Q" + struct.pack("!I", len(data) + 4) + data)
    rows = []
    error = None
    while True:
        kind, body = _read_msg(sock)
        if kind == b"D":
            (ncols,) = struct.unpack("!H", body[:2])
            pos = 2
            cols = []
            for _ in range(ncols):
                (clen,) = struct.unpack("!i", body[pos : pos + 4])
                pos += 4
                if clen < 0:
                    cols.append("NULL")
                else:
                    cols.append(body[pos : pos + clen].decode("utf-8", "replace"))
                    pos += clen
            rows.append("|".join(cols))
        elif kind == b"E":
            error = _error_text(body)
        elif kind == b"Z":
            if error:
                raise RuntimeError(error)
            return rows


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd == "ms":
        ms()
    elif cmd == "tcp":
        return 0 if tcp_ok(argv[2], argv[3]) else 1
    elif cmd == "wait":
        return wait(argv[2], argv[3], argv[4])
    elif cmd == "hold-port":
        hold_port(argv[2], argv[3])
    elif cmd == "freeport":
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        print(s.getsockname()[1])
    elif cmd in ("pgquery", "pgwait", "pghold"):
        host, port = argv[2], argv[3]
        if cmd == "pgwait":
            end = time.time() + float(argv[4])
            while time.time() < end:
                try:
                    sock = pg_connect(host, port, "postgres", "postgres")
                    pg_query(sock, "select 1")
                    return 0
                except (OSError, RuntimeError, ConnectionError):
                    time.sleep(0.3)
            return 1
        user, db = argv[4], argv[5]
        sock = pg_connect(host, port, user, db)
        if cmd == "pghold":
            print("connected", flush=True)
            while True:
                time.sleep(60)
        for sql in argv[6:]:
            for row in pg_query(sock, sql):
                print(row)
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except Exception as exc:  # a spike script: print the reason, fail
        print("tool.py: %s" % exc, file=sys.stderr)
        sys.exit(1)
