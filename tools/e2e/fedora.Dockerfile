# Fedora with systemd as PID 1, for the container job of linux-e2e.yml
# (task M4-18f). Run it with --privileged on a runner only.
FROM fedora:latest

RUN dnf -y install systemd systemd-resolved NetworkManager python3 openssl \
        ca-certificates iproute procps-ng util-linux gawk \
    && dnf clean all

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
