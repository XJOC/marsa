#!/usr/bin/env python3
"""A stand-in for the DNS server of marsad (crates/app/src/daemon/dns.rs).

Same answers: 127.0.0.1 for A and ::1 for AAAA under `.test`, no record for
any other type there, and REFUSED outside it. It also writes one line per
query, so the spike can count the queries that systemd-resolved sends here
for names outside `.test`.

usage: dns-standin.py <port> <query log>
"""

import socket
import socketserver
import struct
import sys
import threading
import time

PORT = int(sys.argv[1])
LOG = sys.argv[2]
LOCK = threading.Lock()

REFUSED = 5
TYPE_A = 1
TYPE_AAAA = 28
TYPE_OPT = 41


def question_end(data):
    """The offset after the first question, and its name."""
    labels = []
    offset = 12
    while True:
        length = data[offset]
        offset += 1
        if length == 0:
            break
        labels.append(data[offset : offset + length].decode("ascii", "replace"))
        offset += length
    return offset + 4, ".".join(labels)


def answer(data, transport):
    if len(data) < 17:
        return None
    ident, flags, _qd, _an, _ns, additional = struct.unpack(">HHHHHH", data[:12])
    end, name = question_end(data)
    qtype, qclass = struct.unpack(">HH", data[end - 4 : end])
    question = data[12:end]
    lower = name.lower()
    in_test = lower == "test" or lower.endswith(".test")
    with LOCK, open(LOG, "a", encoding="ascii") as log:
        zone = "test" if in_test else "other"
        log.write(f"{time.time():.3f} {transport} {zone} type={qtype} {name}\n")
    recursion_desired = flags & 0x0100
    record = b""
    if not in_test:
        head_flags = 0x8000 | recursion_desired | REFUSED
    else:
        head_flags = 0x8400 | recursion_desired
        if qclass == 1 and qtype == TYPE_A:
            record = b"\xc0\x0c" + struct.pack(">HHIH", TYPE_A, 1, 1, 4) + bytes([127, 0, 0, 1])
        elif qclass == 1 and qtype == TYPE_AAAA:
            record = b"\xc0\x0c" + struct.pack(">HHIH", TYPE_AAAA, 1, 1, 16) + bytes(15) + b"\x01"
    # A client that sent an OPT record gets one back (RFC 6891), like marsad.
    opt = b""
    if additional:
        opt = b"\x00" + struct.pack(">HHIH", TYPE_OPT, 1232, 0, 0)
    counts = struct.pack(">HHHH", 1, 1 if record else 0, 0, 1 if opt else 0)
    return struct.pack(">HH", ident, head_flags) + counts + question + record + opt


class Udp(socketserver.BaseRequestHandler):
    def handle(self):
        data, sock = self.request
        reply = answer(data, "udp")
        if reply:
            sock.sendto(reply, self.client_address)


class Tcp(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(5)
        try:
            head = self.request.recv(2)
            if len(head) < 2:
                return
            (length,) = struct.unpack(">H", head)
            data = b""
            while len(data) < length:
                chunk = self.request.recv(length - len(data))
                if not chunk:
                    return
                data += chunk
            reply = answer(data, "tcp")
            if reply:
                self.request.sendall(struct.pack(">H", len(reply)) + reply)
        except (socket.timeout, ConnectionError):
            pass


class UdpServer(socketserver.ThreadingUDPServer):
    allow_reuse_address = True


class TcpServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True


udp = UdpServer(("127.0.0.1", PORT), Udp)
tcp = TcpServer(("127.0.0.1", PORT), Tcp)
threading.Thread(target=tcp.serve_forever, daemon=True).start()
print(f"dns-standin: 127.0.0.1:{PORT} udp+tcp", flush=True)
udp.serve_forever()
