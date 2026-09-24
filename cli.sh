#!/bin/zsh

# Runs the admin CLI inside whichever colour is currently serving, e.g.
#   ./cli.sh set-role <login_name> admin
# Going through this rather than `docker exec oeee-cafe-blue` means you do not
# have to know which colour the last deploy landed on.

set -euo pipefail

cd "$(dirname "$0")"

if grep -q oeee-cafe-green proxy/upstream.caddy 2>/dev/null; then
    container=oeee-cafe-green
else
    container=oeee-cafe-blue
fi

# -t only when there is a terminal to attach, so this still works over a
# non-interactive ssh.
tty_flags=(-i)
if [[ -t 0 ]]; then
    tty_flags=(-i -t)
fi

exec docker exec "${tty_flags[@]}" "$container" ./oeee-cafe cli -c config/config.toml "$@"
