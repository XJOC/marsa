# Fedora with systemd as PID 1, for the S-02 spike only. Run it with
# --privileged on a CI runner; see .github/workflows/spike-s02-linux.yml.
FROM fedora:latest

# nss-tools (certutil) is left out on purpose: the spike records that a
# Fedora without it needs the package.
RUN dnf -y install systemd systemd-resolved sudo polkit libcap python3 \
        ca-certificates findutils iproute procps-ng tar gzip util-linux gawk \
    && dnf clean all

RUN useradd -m runner \
    && echo 'runner ALL=(ALL) NOPASSWD:ALL' >/etc/sudoers.d/runner \
    && chmod 0440 /etc/sudoers.d/runner

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
