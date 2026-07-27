#!/bin/sh
# Nothing is enabled or started here: a cooler is not a thing a package should
# start reaching for on its own. `systemctl enable --now flydigictld.socket`.
systemctl daemon-reload >/dev/null 2>&1 || true
udevadm control --reload-rules >/dev/null 2>&1 || true
udevadm trigger --subsystem-match=hidraw >/dev/null 2>&1 || true
