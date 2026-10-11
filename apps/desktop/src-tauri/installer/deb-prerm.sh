#!/bin/sh
# Debian Policy 6.6: when the prerm of an installed package up to 0.1.7-beta.n
# refuses an upgrade because Private AI Proxy is running, dpkg runs this new
# prerm with `failed-upgrade`; succeeding lets the upgrade continue. Remove in 0.3.
set -e
case "$1" in
  failed-upgrade) exit 0 ;;
esac
