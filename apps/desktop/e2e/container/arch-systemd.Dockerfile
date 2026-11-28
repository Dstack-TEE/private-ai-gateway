FROM archlinux:base@sha256:bddb91b7a73f31ed6be307fc8ec75e5dd59ff8ccf6c5a1bbb8205f14702c6b4a
RUN pacman -Syu --noconfirm --needed systemd dbus && pacman -Scc --noconfirm
STOPSIGNAL SIGRTMIN+3
CMD ["/usr/lib/systemd/systemd"]
