#!/usr/bin/env bash
# Runs as root in the Fedora container, before the spike.
#
# Docker writes its DNS server into /etc/resolv.conf. A real Fedora has
# systemd-resolved there instead (the stub on 127.0.0.53), with the network's
# DNS server on the link. This makes the container look the same, so the
# spike tests what a Fedora desktop has.
set -eu

upstream=$(awk '/^nameserver/ {print $2; exit}' /etc/resolv.conf)
link=$(ip -o route show default | awk '{print $5; exit}')
echo "upstream DNS $upstream on link $link"

systemctl enable --now systemd-resolved
resolvectl dns "$link" "$upstream"
resolvectl default-route "$link" yes
# Docker mounts this file, so it cannot become a symlink. Its content can
# change: resolved skips its own stub address when it reads the file.
printf 'nameserver 127.0.0.53\noptions edns0 trust-ad\n' >/etc/resolv.conf

# Docker sets this to 0 in a container. A real Fedora has 1024.
sysctl -w net.ipv4.ip_unprivileged_port_start=1024

resolvectl status
getent hosts example.com
