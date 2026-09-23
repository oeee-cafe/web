#!/bin/zsh

# Zero-downtime deploy, blue/green.
#
#   build the image -> start the idle colour -> wait for it to answer /health
#   -> point the proxy at it -> drain -> stop the colour that was serving
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

LOCK_DIR=.deploy.lock

# One deploy at a time: two would fight over the build database's port and over
# which colour is live.
#
# Taken before the pull, because the pull is itself a change to the machine: a
# deploy refused after pulling leaves the checkout on a commit that the deploy
# already building has never seen, and the release that came out of it was
# built from one tree while the checkout claimed another. Taken before the trap
# is installed, so failing to take it cannot clean up after the deploy that
# holds it.
if [[ -z ${DEPLOY_LOCK_HELD:-} ]] && ! mkdir "$LOCK_DIR" 2>/dev/null; then
    echo "ERROR: another deploy is in progress (remove $LOCK_DIR if it is not)"
    exit 1
fi

cleanup() {
    echo "==> Cleaning up build database..."
    docker rm -fv oeee-cafe-build-db 2>/dev/null || true
    rmdir "$LOCK_DIR" 2>/dev/null || true
}
trap cleanup EXIT

# `git pull` can rewrite this file underneath the shell that is reading it, and
# zsh reads a script incrementally rather than all at once. Pull before doing
# anything else, and start over from the new version if it changed, so the rest
# of the deploy runs from one consistent script.
#
# zsh does not run an EXIT trap on exec, so the lock survives that restart;
# DEPLOY_LOCK_HELD is how the new process knows it already holds it rather than
# refusing its own deploy.
SCRIPT_BEFORE_PULL="$(shasum "$0")"
echo "==> Pulling latest code from git..."
if ! git pull; then
    echo "ERROR: git pull failed"
    exit 1
fi
if [[ "$SCRIPT_BEFORE_PULL" != "$(shasum "$0")" ]]; then
    echo "==> deploy.sh changed in that pull; restarting it..."
    export DEPLOY_LOCK_HELD=1
    exec "$0" "$@"
fi

# Enable BuildKit for Docker builds
export DOCKER_BUILDKIT=1
export COMPOSE_DOCKER_CLI_BUILD=1

# How long the outgoing colour keeps running after the proxy stops sending it
# new requests, so in-flight ones finish where they started.
DRAIN_SECONDS=${DRAIN_SECONDS:-10}
# Cold start is a database connection, migrations, and binding a port; the rest
# of this is slack for a busy machine.
HEALTH_TIMEOUT_SECONDS=${HEALTH_TIMEOUT_SECONDS:-180}

UPSTREAM_FILE=proxy/upstream.caddy

echo "==> Ensuring build network exists..."
docker network create oeee-cafe-network 2>/dev/null || true

echo "==> Starting temporary PostgreSQL container for build..."
if ! docker run -d \
    --name oeee-cafe-build-db \
    -p 5433:5432 \
    -e POSTGRES_PASSWORD=postgres \
    -e POSTGRES_DB=oeee_cafe \
    postgres:18; then
    echo "ERROR: Failed to start build database"
    exit 1
fi

echo "==> Waiting for PostgreSQL to be ready..."
for i in {1..30}; do
    if docker exec oeee-cafe-build-db pg_isready -U postgres >/dev/null 2>&1; then
        echo "PostgreSQL is ready!"
        break
    fi
    if [ $i -eq 30 ]; then
        echo "ERROR: PostgreSQL did not become ready in time"
        exit 1
    fi
    sleep 1
done

echo "==> Running migrations..."
export DATABASE_URL=postgresql://postgres:postgres@localhost:5433/oeee_cafe
MIGRATION_ATTEMPTS=0
MAX_MIGRATION_ATTEMPTS=5
until sqlx migrate run; do
    MIGRATION_ATTEMPTS=$((MIGRATION_ATTEMPTS + 1))
    if [ $MIGRATION_ATTEMPTS -ge $MAX_MIGRATION_ATTEMPTS ]; then
        echo "ERROR: migrations failed after $MAX_MIGRATION_ATTEMPTS attempts"
        exit 1
    fi
    echo "Migration attempt $MIGRATION_ATTEMPTS failed, retrying in 2 seconds..."
    sleep 2
done
echo "Migrations completed successfully!"

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

echo "==> Building the new image..."
# Versions the static asset URLs the server hands out, so each deploy
# invalidates browser and CDN caches exactly once.
export GIT_COMMIT="$(git rev-parse HEAD)"
if ! docker compose build "$TARGET"; then
    echo "ERROR: docker compose build failed"
    exit 1
fi

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

echo "==> Deployment successful!"
echo "==> Checking container status..."
docker compose ps
