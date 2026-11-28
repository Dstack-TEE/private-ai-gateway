FROM fedora:41@sha256:68bb1ba893be0c05991b2df55bc6571862bab7526fd6053b1ebacd53a2a75366
# The host's AppArmor unix-chkpwd profile attaches to this container's helper
# and denies CAP_DAC_OVERRIDE, which Fedora's mode-0000 /etc/shadow requires.
# Owner-readable (as on Arch) needs no capability.
RUN dnf -y install systemd systemd-container systemd-pam dbus dbus-broker && \
    chmod 0400 /etc/shadow && \
    systemctl enable systemd-logind.service && \
    systemctl --global enable dbus-broker.service dbus.socket && \
    dnf clean all
STOPSIGNAL SIGRTMIN+3
CMD ["/usr/lib/systemd/systemd"]
