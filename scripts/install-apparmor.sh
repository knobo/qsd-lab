#!/bin/sh
# Install (or remove with --uninstall) the qsd-lab rules for fusermount3.
# Only needed on systems where AppArmor confines fusermount3 (Ubuntu 24.10+).
set -eu

LOCAL=/etc/apparmor.d/local/fusermount3
PROFILE=/etc/apparmor.d/fusermount3
RULES="$(dirname "$0")/../packaging/apparmor/fusermount3-qsd-lab"
BEGIN='# >>> qsd-lab >>>'
END='# <<< qsd-lab <<<'

if [ ! -f "$PROFILE" ]; then
    echo "No $PROFILE: fusermount3 is not confined by AppArmor here. Nothing to do."
    exit 0
fi

if [ "$(id -u)" -ne 0 ]; then
    exec sudo "$0" "$@"
fi

# Remove an earlier qsd-lab block, keep everything else in the local file.
touch "$LOCAL"
sed -i "/^$BEGIN\$/,/^$END\$/d" "$LOCAL"

if [ "${1:-}" != "--uninstall" ]; then
    { echo "$BEGIN"; cat "$RULES"; echo "$END"; } >> "$LOCAL"
fi

apparmor_parser -r "$PROFILE"
echo "Reloaded $PROFILE (${1:-installed})."
