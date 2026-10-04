#!/bin/sh
# One-time setup for dchat-host on Linux. It lets whoever is logged in at this computer
# create the virtual keyboard, mouse and game controllers dchat-host uses (/dev/uinput),
# so dchat-host itself never needs to run as root.
#   ./install.sh               set up (asks for your password once)
#   ./install.sh --uninstall   undo
# DESTDIR=/some/dir installs the files there instead and skips system commands (testing).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
rules=/etc/udev/rules.d/60-dchat-host.rules
modules=/etc/modules-load.d/dchat-host.conf
dest=${DESTDIR:-}

if [ -z "$dest" ] && [ "$(id -u)" -ne 0 ]; then
    echo "Setting up dchat-host needs administrator rights once: sudo will ask for your password."
    exec sudo sh "$here/install.sh" "$@"
fi

apply_rules() {
    if [ -z "$dest" ]; then
        udevadm control --reload-rules
        udevadm trigger --sysname-match=uinput
    fi
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$dest$rules" "$dest$modules"
    apply_rules
    echo "Removed: dchat-host can no longer create input devices on this computer."
    exit 0
fi

install -D -m 0644 "$here/60-dchat-host.rules" "$dest$rules"
install -D -m 0644 "$here/uinput.conf" "$dest$modules"
if [ -z "$dest" ]; then
    modprobe uinput
fi
apply_rules
echo "Done. Now start dchat-host:  ./dchat-host"
echo "(If it still says it has no permission for /dev/uinput, log out and back in once.)"
