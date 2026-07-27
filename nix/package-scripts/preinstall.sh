#!/bin/sh
# The udev rule and the unit both name this group, and the unit's dynamic user
# is put in it to reach the cooler without any privileges of its own.
getent group flydigi >/dev/null || groupadd --system flydigi
