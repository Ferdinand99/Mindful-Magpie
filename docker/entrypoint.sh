#!/bin/sh
# Container entrypoint.
#
# Started as root (the default), it:
#   1. gives the `magpie` user the uid/gid from PUID/PGID (default 1000:1000;
#      Unraid uses 99:100),
#   2. hands the data directory to `magpie`,
# and then runs the app as `magpie`.
#
# Started with an explicit non-root user (`--user`, `user:` in compose), it runs
# the binary directly and leaves everything alone.
set -e

app=/usr/local/bin/mindful-magpie

if [ "$(id -u)" != "0" ]; then
    exec "$app" "$@"
fi

uid="${PUID:-1000}"
gid="${PGID:-1000}"
case "$uid$gid" in
    *[!0-9]*) echo "PUID and PGID must be numeric (got PUID=$uid PGID=$gid)" >&2; exit 1 ;;
esac

if [ "$uid" != "$(id -u magpie)" ] || [ "$gid" != "$(id -g magpie)" ]; then
    sed -i "s/^magpie:x:[0-9]*:[0-9]*:/magpie:x:$uid:$gid:/" /etc/passwd
    sed -i "s/^magpie:x:[0-9]*:/magpie:x:$gid:/" /etc/group
fi

data_dir="${MM_DATA_DIR:-/data}"
mkdir -p "$data_dir"
chown -R magpie:magpie "$data_dir" 2>/dev/null \
    || echo "warning: could not chown $data_dir; make it writable for uid $uid" >&2

exec su-exec magpie "$app" "$@"
