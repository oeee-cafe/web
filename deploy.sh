#!/bin/zsh

# Deploy what is on origin/main: build its image on this machine, load it on
# the server, and have the server switch to it blue/green.
#
#   ./deploy.sh
#
#   fetch origin/main into a clean build checkout -> build oeee-cafe:<commit>
#   against a throwaway Postgres -> ship the image over ssh -> run
#   deploy-server.sh <commit> on the server
#
# The build used to run on the server, where it pegged the CPU the site and its
# neighbours share, and left a 1.4GB image and gigabytes of build cache behind
# every time. Both machines are arm64, so an image built here runs there as is.
#
# The server is reached by the ssh alias in DEPLOY_HOST, so its address lives
# in ~/.ssh/config and not in this public repository:
#
#   Host oeee-cafe-deploy
#       HostName <the server's address>
#
# Docker here has to be running. OrbStack is started if it is not.

set -euo pipefail

DEPLOY_HOST=${DEPLOY_HOST:-oeee-cafe-deploy}
# Expanded by the server's shell, not this one.
REMOTE_DIR=${REMOTE_DIR:-'~/Git/oeee-cafe'}
# A checkout of its own rather than the one this script was run from: that one
# holds node_modules, dist/ and the rest of a working tree, any of which the
# Dockerfile's COPYs would carry into the image, and it may not be what was
# pushed. This one is cleaned to exactly the commit being deployed.
BUILD_DIR=${DEPLOY_BUILD_DIR:-$HOME/.cache/oeee-cafe-deploy}
LOCK_DIR=$BUILD_DIR.lock

REPO_URL="$(git -C "$(dirname "$0")" remote get-url origin)"

mkdir -p "$(dirname "$BUILD_DIR")"
# One deploy at a time: two would share the build checkout and the build
# database's port.
if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    echo "ERROR: another deploy is in progress (remove $LOCK_DIR if it is not)"
    exit 1
fi

cleanup() {
    docker rm -fv oeee-cafe-build-db >/dev/null 2>&1 || true
    rmdir "$LOCK_DIR" 2>/dev/null || true
}
trap cleanup EXIT

if ! docker info >/dev/null 2>&1; then
    if command -v orb >/dev/null; then
        echo "==> Starting OrbStack..."
        orb start
    fi
    if ! docker info >/dev/null 2>&1; then
        echo "ERROR: Docker is not running on this machine"
        exit 1
    fi
fi

echo "==> Fetching origin/main..."
if [[ ! -d $BUILD_DIR/.git ]]; then
    git clone --quiet "$REPO_URL" "$BUILD_DIR"
fi
git -C "$BUILD_DIR" fetch --quiet origin main
COMMIT="$(git -C "$BUILD_DIR" rev-parse FETCH_HEAD)"
git -C "$BUILD_DIR" checkout --quiet --detach "$COMMIT"
git -C "$BUILD_DIR" submodule --quiet update --init --recursive
git -C "$BUILD_DIR" clean -qffdx
git -C "$BUILD_DIR" submodule --quiet foreach --recursive git clean -qffdx
IMAGE=oeee-cafe:$COMMIT
echo "==> Deploying $(git -C "$BUILD_DIR" log -1 --format='%h %s')"

# Only what has been pushed goes out, because the server checks out the same
# commit for its compose file and proxy config.
if [[ "$(git -C "$(dirname "$0")" rev-parse HEAD)" != "$COMMIT" ]]; then
    echo "    (not this checkout's HEAD, $(git -C "$(dirname "$0")" rev-parse --short HEAD); push first to deploy that)"
fi

remote() {
    ssh "$DEPLOY_HOST" "zsh -l -c '$1'"
}

# A deploy that failed after shipping can be retried without rebuilding or
# sending the image again.
if remote "docker image inspect $IMAGE" >/dev/null 2>&1; then
    echo "==> The server already has $IMAGE"
else
    if docker image inspect "$IMAGE" >/dev/null 2>&1; then
        echo "==> $IMAGE is already built"
    else
        # sqlx checks every query against a real schema at compile time, and
        # the Dockerfile's DATABASE_URL points at this, on the host.
        echo "==> Starting temporary PostgreSQL container for build..."
        docker rm -fv oeee-cafe-build-db >/dev/null 2>&1 || true
        docker run -d --quiet \
            --name oeee-cafe-build-db \
            -p 5433:5432 \
            -e POSTGRES_PASSWORD=postgres \
            -e POSTGRES_DB=oeee_cafe \
            postgres:18 >/dev/null

        echo "==> Waiting for PostgreSQL to be ready..."
        for i in {1..30}; do
            if docker exec oeee-cafe-build-db pg_isready -U postgres >/dev/null 2>&1; then
                break
            fi
            if [ $i -eq 30 ]; then
                echo "ERROR: PostgreSQL did not become ready in time"
                exit 1
            fi
            sleep 1
        done

        echo "==> Running migrations..."
        MIGRATION_ATTEMPTS=0
        until DATABASE_URL=postgresql://postgres:postgres@localhost:5433/oeee_cafe \
            sqlx migrate run --source "$BUILD_DIR/migrations"; do
            MIGRATION_ATTEMPTS=$((MIGRATION_ATTEMPTS + 1))
            if [ $MIGRATION_ATTEMPTS -ge 5 ]; then
                echo "ERROR: migrations failed after $MIGRATION_ATTEMPTS attempts"
                exit 1
            fi
            echo "Migration attempt $MIGRATION_ATTEMPTS failed, retrying in 2 seconds..."
            sleep 2
        done

        echo "==> Building $IMAGE..."
        # GIT_COMMIT versions the static asset URLs the server hands out, so
        # each deploy invalidates browser and CDN caches exactly once.
        DOCKER_BUILDKIT=1 docker build \
            --platform linux/arm64 \
            --build-arg GIT_COMMIT="$COMMIT" \
            --tag "$IMAGE" \
            "$BUILD_DIR"
        docker rm -fv oeee-cafe-build-db >/dev/null 2>&1 || true
    fi

    echo "==> Shipping $IMAGE to $DEPLOY_HOST..."
    docker save "$IMAGE" | zstd -T0 -3 -q | remote "zstd -dcq | docker load --quiet"
fi

echo "==> Switching the server to $IMAGE..."
remote "$REMOTE_DIR/deploy-server.sh $COMMIT"

# The server has the image now, and this copy only existed to be sent there.
# Keeping the one just deployed makes a retry cheap; the build cache that makes
# the next build fast is separate and stays.
for tag in $(docker image ls oeee-cafe --format '{{.Tag}}'); do
    if [[ "$tag" != "$COMMIT" ]]; then
        docker rmi "oeee-cafe:$tag" >/dev/null 2>&1 || true
    fi
done
