# Debian 12 (systemd 252) with systemd as PID 1, for the container job of
# linux-e2e.yml (task M4-18f): the systemd versions 250 to 252, where the
# unit adds the local route for 127.0.0.1. Run it with --privileged on a
# runner only.
FROM debian:12

# systemd-resolved's postinst links /etc/resolv.conf, which Docker mounts
# while the image is built; the link is made again at run time.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        systemd systemd-sysv dbus iproute2 python3 openssl ca-certificates \
        procps network-manager \
    && (DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        systemd-resolved || dpkg --configure -a || true) \
    && rm -rf /var/lib/apt/lists/*

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
