#!/bin/zsh

# The server half of a deploy, blue/green. deploy.sh runs this over ssh once it
# has built the image elsewhere and loaded it here as oeee-cafe:<commit>:
#
#   ./deploy-server.sh <commit>
#
#   check out <commit> -> start the idle colour from its image -> wait for it
#   to answer /health -> point the proxy at it -> drain -> stop the colour that
#   was serving -> remove images no container can come back to
#
# Nothing is compiled here. The build used to run on this machine, and it
# pegged the CPU the site and its neighbours share for the length of every
# deploy.
#
# The published port belongs to the proxy and is never rebound, so no request
# arrives at a closed port. Drawing sessions on the outgoing container get a
# proper close frame when it stops, and their clients reconnect through the
# proxy onto the new colour and resume from their last canonical sequence --
# no strokes lost, no waiting for a container to boot first.
#
# To roll back: point proxy/upstream.caddy at the other colour, `docker compose
# start` it, and reload the proxy. The previous colour's container is kept
# (stopped) with its image until the next deploy recreates it.

# Exit on error, undefined variables, and pipe failures
set -euo pipefail

cd "$(dirname "$0")"

COMMIT=${1:?usage: deploy-server.sh <commit>}
IMAGE=oeee-cafe:$COMMIT

LOCK_DIR=.deploy.lock

# One deploy at a time: two would fight over which colour is live.
#
# Taken before the checkout moves, because moving it is itself a change to the
# machine: a deploy refused after updating leaves the checkout on a commit that
# the deploy in progress has never seen. Taken before the trap is installed, so
# failing to take it cannot clean up after the deploy that holds it.
if [[ -z ${DEPLOY_LOCK_HELD:-} ]] && ! mkdir "$LOCK_DIR" 2>/dev/null; then
    echo "ERROR: another deploy is in progress (remove $LOCK_DIR if it is not)"
    exit 1
fi

cleanup() {
    rmdir "$LOCK_DIR" 2>/dev/null || true
}
trap cleanup EXIT

# The image is built from <commit>, and the compose file, Caddyfile and proxy
# config it runs under come from this checkout, so the two have to agree.
# Fast-forward to exactly that commit rather than to whatever main is now: if
# main moved while the image was building, the newer tree is not what was built.
#
# That can rewrite this file underneath the shell that is reading it, and zsh
# reads a script incrementally rather than all at once, so start over from the
# new version if it changed. zsh does not run an EXIT trap on exec, so the lock
# survives that restart; DEPLOY_LOCK_HELD is how the new process knows it
# already holds it rather than refusing its own deploy.
SCRIPT_BEFORE_UPDATE="$(shasum "$0")"
echo "==> Checking out $COMMIT..."
if ! git fetch --quiet origin || ! git merge --ff-only --quiet "$COMMIT"; then
    echo "ERROR: could not fast-forward the checkout to $COMMIT"
    exit 1
fi
if [[ "$(git rev-parse HEAD)" != "$COMMIT" ]]; then
    # merge --ff-only onto an ancestor is a no-op, not a failure.
    echo "ERROR: the checkout is at $(git rev-parse --short HEAD), which is ahead of $COMMIT;"
    echo "       refusing to put an older release live. To roll back, see the top of this file."
    exit 1
fi
if [[ "$SCRIPT_BEFORE_UPDATE" != "$(shasum "$0")" ]]; then
    echo "==> deploy-server.sh changed in that update; restarting it..."
    export DEPLOY_LOCK_HELD=1
    exec "$0" "$@"
fi

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "ERROR: $IMAGE is not loaded here; deploy.sh ships it before running this"
    exit 1
fi

# How long the outgoing colour keeps running after the proxy stops sending it
# new requests, so in-flight ones finish where they started.
DRAIN_SECONDS=${DRAIN_SECONDS:-10}
# Cold start is a database connection, migrations, and binding a port; the rest
# of this is slack for a busy machine.
HEALTH_TIMEOUT_SECONDS=${HEALTH_TIMEOUT_SECONDS:-180}

UPSTREAM_FILE=proxy/upstream.caddy

# The proxy's own config is the only honest answer to "what is serving right
# now", so read the colour back off it rather than tracking it separately.
# Absent (first blue/green deploy) reads as blue, which does not exist yet, so
# the first deploy brings up green and the steps below skip what is missing.
if grep -q oeee-cafe-green "$UPSTREAM_FILE" 2>/dev/null; then
    ACTIVE=oeee-cafe-green
    TARGET=oeee-cafe-blue
else
    ACTIVE=oeee-cafe-blue
    TARGET=oeee-cafe-green
fi
echo "==> $ACTIVE is serving; deploying to $TARGET"

# Both colours are declared with oeee-cafe:current. Moving that tag does not
# touch the colour that is serving: a container holds the image it was created
# from, not the name.
docker tag "$IMAGE" oeee-cafe:current

echo "==> Starting $TARGET..."
if ! docker compose up -d --force-recreate "$TARGET"; then
    echo "ERROR: failed to start $TARGET; $ACTIVE is still serving"
    exit 1
fi

echo "==> Waiting for $TARGET to answer /health..."
TARGET_HEALTHY=""
for i in {1..$HEALTH_TIMEOUT_SECONDS}; do
    if docker compose exec -T "$TARGET" \
        curl -fsS -o /dev/null http://localhost:3000/health 2>/dev/null; then
        echo "$TARGET is healthy after ${i}s"
        TARGET_HEALTHY=1
        break
    fi
    sleep 1
done
if [[ -z "$TARGET_HEALTHY" ]]; then
    echo "ERROR: $TARGET never became healthy; leaving $ACTIVE serving"
    docker compose logs --tail 50 "$TARGET" || true
    docker compose stop "$TARGET" || true
    exit 1
fi

# One-time, on the first blue/green deploy: the single container this replaces
# holds the published port, so the proxy cannot bind until it is gone. By now
# the new colour is already warm, so this is the shortest the gap can be.
if docker container inspect oeee-cafe >/dev/null 2>&1; then
    echo "==> Removing the pre-blue/green container so the proxy can take the port..."
    docker rm -f oeee-cafe
fi

echo "==> Pointing the proxy at $TARGET..."
print -r -- "reverse_proxy $TARGET:3000" >"$UPSTREAM_FILE"
if ! docker compose up -d proxy; then
    echo "ERROR: failed to start the proxy"
    exit 1
fi
# A proxy that had to start just now already read the new upstream, and Caddy
# treats a reload to an identical config as a no-op, so this is only doing work
# in the usual case where it was already running. Retried because a proxy that
# did just start may not have its admin endpoint up yet.
RELOADED=""
for i in {1..30}; do
    if docker compose exec -T proxy caddy reload --config /etc/caddy/Caddyfile 2>/dev/null; then
        RELOADED=1
        break
    fi
    sleep 1
done
if [[ -z "$RELOADED" ]]; then
    echo "ERROR: could not reload the proxy onto $TARGET"
    docker compose exec -T proxy caddy reload --config /etc/caddy/Caddyfile || true
    exit 1
fi

echo "==> Verifying the published port..."
PUBLISHED_OK=""
for i in {1..30}; do
    if curl -fsS -o /dev/null http://localhost:30000/health; then
        PUBLISHED_OK=1
        break
    fi
    sleep 1
done
if [[ -z "$PUBLISHED_OK" ]]; then
    echo "ERROR: the published port is not serving $TARGET"
    docker compose logs --tail 50 proxy || true
    exit 1
fi

if docker container inspect "$ACTIVE" >/dev/null 2>&1; then
    echo "==> Draining $ACTIVE for ${DRAIN_SECONDS}s..."
    sleep "$DRAIN_SECONDS"
    echo "==> Stopping $ACTIVE..."
    # SIGTERM here is what tells its drawing sessions to say goodbye; their
    # clients reconnect through the proxy onto $TARGET, which is already up.
    docker compose stop "$ACTIVE"
fi

# Every release is a 1.4GB image, and nothing else removes them. Keep the one
# just started and whatever the stopped colour was created from, which is the
# rollback; docker refuses to remove an image a container still uses, so the
# latter needs no special case. Untagged ones are what deploys before
# oeee-cafe:<commit> tags left behind -- recognised by their command, since
# they have no name left to go by.
echo "==> Removing old release images..."
for tag in $(docker image ls oeee-cafe --format '{{.Tag}}'); do
    if [[ "$tag" != current && "$tag" != "$COMMIT" ]]; then
        docker rmi "oeee-cafe:$tag" >/dev/null 2>&1 || true
    fi
done
for id in $(docker image ls --quiet --filter dangling=true); do
    if [[ "$(docker image inspect --format '{{index .Config.Cmd 0}}' "$id" 2>/dev/null)" == ./oeee-cafe ]]; then
        docker rmi "$id" >/dev/null 2>&1 || true
    fi
done

echo "==> Deployment successful!"
echo "==> Checking container status..."
docker compose ps
