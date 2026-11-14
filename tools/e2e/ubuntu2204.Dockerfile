# Ubuntu 22.04 (systemd 249) with systemd as PID 1 and systemd-networkd
# masked, as on a desktop with NetworkManager: the one system before 250
# where the helper makes marsa0, with the local route for 127.0.0.1 (task
# M4-18k, review of #125). Run it with --privileged on a runner only.
FROM ubuntu:22.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        systemd systemd-sysv dbus iproute2 python3 openssl ca-certificates procps \
        network-manager \
    && rm -rf /var/lib/apt/lists/* \
    && systemctl mask systemd-networkd.service systemd-networkd.socket \
        systemd-networkd-wait-online.service

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
