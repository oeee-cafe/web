"""Deploy what is on origin/main, blue/green, or roll back to the release
before it.

    mise run deploy      (python deploy.py deploy)
    mise run rollback    (python deploy.py rollback)
    mise run cli -- ...  (python deploy.py cli ..., the admin CLI on the server)

    fetch origin/main into a clean build checkout -> build oeee-cafe:<commit>
    against a throwaway Postgres -> ship the image over ssh -> copy the compose
    file, proxy config and the config directory -> start the idle
    colour -> wait for it to answer /health -> point the proxy at it -> drain
    -> stop the colour that was serving -> remove images no container can come
    back to

The build used to run on the server, where it pegged the CPU the site and its
neighbours share, and left a 1.4GB image and gigabytes of build cache behind
every time. Both machines are arm64, so an image built here runs there as is.

The server holds no checkout and runs no script of its own. REMOTE_DIR there
has only what this copies in, plus proxy/upstream.caddy, which the switch
writes. Every step on the server is a command sent from here over ssh, one at a
time, so a failure on the server is an exit status this sees and reports.

The published port belongs to the proxy and is never rebound, so no request
arrives at a closed port. Drawing sessions on the outgoing container get a
proper close frame when it stops, and their clients reconnect through the proxy
onto the new colour and resume from their last canonical sequence -- no strokes
lost, no waiting for a container to boot first.

The colour a deploy stops is kept, stopped, with its image and its config
until the next deploy recreates it, and that is what `rollback` goes back to:
it starts that container as it is -- `start`, never `up`, which would recreate
it from oeee-cafe:current, the release being rolled back from -- and hands the
proxy over to it the same way a deploy does. Rolling back twice is back where
you started.

The server is reached by the ssh alias in DEPLOY_HOST, so its address lives in
~/.ssh/config and not in this public repository:

    Host oeee-cafe-deploy
        HostName <the server's address>

The production config (config.toml and the key files it names) lives on this
machine in DEPLOY_CONFIG_DIR, and each deploy copies it over. It is kept out of
the repository, which is public, so edit it there, not on the server: the next
deploy replaces whatever the server has.

Docker here has to be running. OrbStack is started if it is not.

The image's binary has no debug info; it is uploaded to Sentry instead, so
sentry-cli has to be logged in (`sentry-cli login`). Each deploy is also a
release there, named by its commit -- the name the server reports its errors
under (main.rs) -- with the deploy, or rollback, recorded against it, so an
error that started with a release says which. The organization and
project are named here rather than left to ~/.sentryclirc's defaults, which
belong to whichever project that machine set up last. Without the upload, a
release goes out whose stack traces in Sentry have no file or line, so the
deploy stops rather than find that out from the first error. SENTRY_UPLOAD=skip
deploys anyway.
"""

from __future__ import annotations

import io
import os
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent
DEPLOY_HOST = os.environ.get("DEPLOY_HOST", "oeee-cafe-deploy")
# Relative to the home directory on the server, which is where ssh starts.
REMOTE_DIR = os.environ.get("REMOTE_DIR", "oeee-cafe-data")
DEPLOY_CONFIG_DIR = Path(
    os.environ.get("DEPLOY_CONFIG_DIR", Path.home() / ".config/oeee-cafe/production")
)
# A checkout of its own rather than the one this was run from: that one holds
# node_modules, dist/ and the rest of a working tree, any of which the
# Dockerfile's COPYs would carry into the image, and it may not be what was
# pushed. This one is cleaned to exactly the commit being deployed.
BUILD_DIR = Path(
    os.environ.get("DEPLOY_BUILD_DIR", Path.home() / ".cache/oeee-cafe-deploy")
)
LOCK_DIR = BUILD_DIR.with_name(BUILD_DIR.name + ".lock")
DEBUG_DIR = BUILD_DIR.with_name(BUILD_DIR.name + ".debug")
# How long the outgoing colour keeps running after the proxy stops sending it
# new requests, so in-flight ones finish where they started.
DRAIN_SECONDS = int(os.environ.get("DRAIN_SECONDS", "10"))
# Cold start is a database connection, migrations, and binding a port; the rest
# of this is slack for a busy machine.
HEALTH_TIMEOUT_SECONDS = int(os.environ.get("HEALTH_TIMEOUT_SECONDS", "180"))
SENTRY_UPLOAD = os.environ.get("SENTRY_UPLOAD", "")
SENTRY_ENV = {
    "SENTRY_ORG": os.environ.get("SENTRY_ORG", "limeburst"),
    "SENTRY_PROJECT": os.environ.get("SENTRY_PROJECT", "oeee-cafe"),
}
# The repository as Sentry's GitHub integration names it.
SENTRY_REPOSITORY = "oeee-cafe/web"
BUILD_DB = "oeee-cafe-build-db"
UPSTREAM_FILE = "proxy/upstream.caddy"
# What the server needs from the repository; everything else is in the image.
RUNTIME_FILES = {"docker-compose.yml": 0o644, "proxy/Caddyfile": 0o644}


class DeployError(Exception):
    pass


def step(message: str) -> None:
    print(f"==> {message}", flush=True)


def run(
    *args: str | Path, check: bool = True, quiet: bool = False, **kwargs
) -> subprocess.CompletedProcess:
    if quiet:
        kwargs.setdefault("stdout", subprocess.DEVNULL)
        kwargs.setdefault("stderr", subprocess.DEVNULL)
    kwargs.setdefault("stdin", subprocess.DEVNULL)
    result = subprocess.run([str(a) for a in args], check=False, **kwargs)
    if check and result.returncode != 0:
        raise DeployError(
            f"`{shlex.join(str(a) for a in args)}` exited with {result.returncode}"
        )
    return result


def succeeds(*args: str | Path) -> bool:
    return run(*args, check=False, quiet=True).returncode == 0


def output(*args: str | Path) -> str:
    return run(*args, stdout=subprocess.PIPE).stdout.decode().strip()


class Server:
    """Commands on the server, each its own ssh invocation.

    One connection is opened and shared (ControlMaster), so the health check
    polling once a second costs a round trip, not a handshake. Commands go
    through a login shell, since that is where docker's PATH is set up, and
    run from REMOTE_DIR.
    """

    def __init__(self, host: str):
        self.host = host
        # In /tmp, not $TMPDIR: a socket path is capped at 104 bytes, and
        # macOS's per-user temporary directory spends most of that on its own.
        self._control_dir = tempfile.mkdtemp(prefix="oeee-deploy-", dir="/tmp")
        control = ["-o", f"ControlPath={self._control_dir}/ssh"]
        self._ssh = ["ssh", *control, "-o", "ControlMaster=no", host]
        self._control = ["ssh", *control, host]
        # Opened up front and on its own, with nothing of ours on its stdio:
        # a master that went into the background holding a pipe some later
        # command reads to EOF would hang that command. Every command after
        # this only ever joins it.
        run(
            "ssh",
            *control,
            "-o",
            "ControlMaster=yes",
            "-o",
            "ControlPersist=yes",
            "-fN",
            host,
            stderr=None,
            stdout=subprocess.DEVNULL,
        )

    def close(self) -> None:
        subprocess.run(
            [*self._control[:-1], "-O", "exit", self.host],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
        )
        shutil.rmtree(self._control_dir, ignore_errors=True)

    def _command(self, script: str, cwd: bool) -> list[str]:
        if cwd:
            script = f"cd {shlex.quote(REMOTE_DIR)} && {script}"
        return [*self._ssh, f"zsh -l -c {shlex.quote(script)}"]

    def run(
        self,
        *args: str,
        check: bool = True,
        quiet: bool = False,
        cwd: bool = True,
        **kwargs,
    ) -> subprocess.CompletedProcess:
        return run(
            *self._command(shlex.join(args), cwd), check=check, quiet=quiet, **kwargs
        )

    def shell(
        self, script: str, cwd: bool = True, **kwargs
    ) -> subprocess.CompletedProcess:
        """For the few steps that need a pipe or a redirect on the server."""
        return run(*self._command(script, cwd), **kwargs)

    def succeeds(self, *args: str, cwd: bool = True) -> bool:
        return self.run(*args, check=False, quiet=True, cwd=cwd).returncode == 0

    def output(self, *args: str, check: bool = True) -> str:
        return (
            self.run(*args, check=check, stdout=subprocess.PIPE).stdout.decode().strip()
        )

    def put(self, data: bytes, script: str) -> None:
        self.shell(script, stdin=None, input=data)

    def interactive(self, *args: str) -> int:
        """A command with this terminal attached -- its input, its output, and
        a tty on the server when there is one here -- and its exit status."""
        command = self._command(shlex.join(args), cwd=True)
        if sys.stdin.isatty():
            command.insert(1, "-t")
        return subprocess.run(command, check=False).returncode


def poll(seconds: int, attempt) -> int | None:
    """Tries once a second; the seconds it took, or None if it never did."""
    for i in range(1, seconds + 1):
        if attempt():
            return i
        time.sleep(1)
    return None


def check_config() -> None:
    """The server reads config.toml and the key files it names at boot, and a
    missing one stops it booting, so the new colour would fail its health
    check and the deploy would stop there anyway. This finds it before the
    build rather than after it, and says which file."""
    config_toml = DEPLOY_CONFIG_DIR / "config.toml"
    if not config_toml.is_file():
        raise DeployError(
            f"no config.toml in {DEPLOY_CONFIG_DIR} (set DEPLOY_CONFIG_DIR)"
        )
    try:
        config = tomllib.loads(config_toml.read_text())
    except tomllib.TOMLDecodeError as error:
        raise DeployError(f"{config_toml} does not parse: {error}") from None

    def paths(table: dict):
        for key, value in table.items():
            if isinstance(value, dict):
                yield from paths(value)
            elif key.endswith("_path") and isinstance(value, str) and value:
                yield key, value

    for key, value in paths(config):
        # The container sees DEPLOY_CONFIG_DIR as /app/config, and runs from /app.
        if not value.startswith("config/"):
            raise DeployError(
                f"config.toml sets {key} = {value!r}, which is not under config/ and so is not shipped"
            )
        if not (DEPLOY_CONFIG_DIR / value.removeprefix("config/")).is_file():
            raise DeployError(
                f"config.toml sets {key} = {value!r}, which is not in {DEPLOY_CONFIG_DIR}"
            )


def tar_of(files: dict[str, tuple[Path, int]], dir_mode: int = 0o755) -> bytes:
    """A tar of {name in the archive: (file here, mode)}, with modes set here
    rather than inherited from whatever this machine's copy has."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as tar:
        dirs = sorted(
            {str(p) for name in files for p in Path(name).parents if str(p) != "."}
        )
        for name in dirs:
            info = tarfile.TarInfo(name)
            info.type = tarfile.DIRTYPE
            info.mode = dir_mode
            info.mtime = int(time.time())
            tar.addfile(info)
        for name, (source, mode) in sorted(files.items()):
            info = tar.gettarinfo(str(source), arcname=name)
            info.mode = mode
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            with open(source, "rb") as f:
                tar.addfile(info, f)
    return buffer.getvalue()


def ensure_docker() -> None:
    if succeeds("docker", "info"):
        return
    if shutil.which("orb"):
        step("Starting OrbStack...")
        run("orb", "start")
    if not succeeds("docker", "info"):
        raise DeployError("Docker is not running on this machine")


def fetch_main() -> str:
    step("Fetching origin/main...")
    if not (BUILD_DIR / ".git").is_dir():
        repo_url = output("git", "-C", REPO, "remote", "get-url", "origin")
        run("git", "clone", "--quiet", repo_url, BUILD_DIR)
    git = ("git", "-C", BUILD_DIR)
    run(*git, "fetch", "--quiet", "origin", "main")
    commit = output(*git, "rev-parse", "FETCH_HEAD")
    run(*git, "checkout", "--quiet", "--detach", commit)
    run(*git, "submodule", "--quiet", "update", "--init", "--recursive")
    run(*git, "clean", "-qffdx")
    run(*git, "submodule", "--quiet", "foreach", "--recursive", "git clean -qffdx")
    step(f"Deploying {output(*git, 'log', '-1', '--format=%h %s')}")

    # Only what has been pushed goes out: the image, the compose file and the
    # proxy config all come from the build checkout, so they agree.
    # Uncommitted work here cannot reach the server by any of them, so a dirty
    # working copy is not refused. What does come from here is this script,
    # and the release it drives is main's: one edited or unpushed would be
    # running steps the files it ships were never deployed with.
    head = output("git", "-C", REPO, "rev-parse", "HEAD")
    if head != commit:
        print(f"    (not this checkout's HEAD, {head[:8]}; push first to deploy that)")
    if Path(__file__).read_bytes() != (BUILD_DIR / "deploy.py").read_bytes():
        raise DeployError(
            f"this deploy.py is not the one at {commit[:12]}; commit and push it, or run\n"
            f"       the one on origin/main"
        )
    return commit


def build_and_ship(server: Server, commit: str) -> None:
    image = f"oeee-cafe:{commit}"
    # A deploy that failed after shipping can be retried without rebuilding or
    # sending the image again.
    if server.succeeds("docker", "image", "inspect", image, cwd=False):
        step(f"The server already has {image}")
        return

    # Built even when the image is already here from a deploy that failed
    # later on: the debug info exists only in the build cache, and a build that
    # finds everything cached takes seconds.
    #
    # sqlx checks every query against a real schema at compile time, and the
    # Dockerfile's DATABASE_URL points at this, on the host.
    step("Starting temporary PostgreSQL container for build...")
    run("docker", "rm", "-fv", BUILD_DB, check=False, quiet=True)
    run(
        "docker",
        "run",
        "-d",
        "--quiet",
        "--name",
        BUILD_DB,
        "-p",
        "5433:5432",
        "-e",
        "POSTGRES_PASSWORD=postgres",
        "-e",
        "POSTGRES_DB=oeee_cafe",
        "postgres:18",
        stdout=subprocess.DEVNULL,
    )
    step("Waiting for PostgreSQL to be ready...")
    if (
        poll(
            30,
            lambda: succeeds(
                "docker", "exec", BUILD_DB, "pg_isready", "-U", "postgres"
            ),
        )
        is None
    ):
        raise DeployError("PostgreSQL did not become ready in time")

    step("Running migrations...")
    migrate_env = {
        **os.environ,
        "DATABASE_URL": "postgresql://postgres:postgres@localhost:5433/oeee_cafe",
    }
    for attempt in range(1, 6):
        if (
            run(
                "sqlx",
                "migrate",
                "run",
                "--source",
                BUILD_DIR / "migrations",
                check=False,
                env=migrate_env,
            ).returncode
            == 0
        ):
            break
        if attempt == 5:
            raise DeployError(f"migrations failed after {attempt} attempts")
        print(f"Migration attempt {attempt} failed, retrying in 2 seconds...")
        time.sleep(2)

    step(f"Building {image}...")
    build_env = {**os.environ, "DOCKER_BUILDKIT": "1"}
    # GIT_COMMIT versions the static asset URLs the server hands out, so each
    # deploy invalidates browser and CDN caches exactly once.
    run(
        "docker",
        "build",
        "--platform",
        "linux/arm64",
        "--build-arg",
        f"GIT_COMMIT={commit}",
        "--tag",
        image,
        BUILD_DIR,
        env=build_env,
    )
    shutil.rmtree(DEBUG_DIR, ignore_errors=True)
    run(
        "docker",
        "build",
        "--quiet",
        "--platform",
        "linux/arm64",
        "--target",
        "debug-files",
        "--output",
        f"type=local,dest={DEBUG_DIR}",
        BUILD_DIR,
        env=build_env,
    )
    run("docker", "rm", "-fv", BUILD_DB, check=False, quiet=True)

    # Before the image goes anywhere, so the first error the new release
    # reports is already symbolicated. Matched to the binary by build id, so an
    # upload of a file Sentry already has is a no-op.
    if SENTRY_UPLOAD == "skip":
        step("Not uploading debug info to Sentry (SENTRY_UPLOAD=skip)")
    else:
        step("Uploading debug info to Sentry...")
        run(
            "sentry-cli",
            "debug-files",
            "upload",
            "--no-zips",
            DEBUG_DIR,
            env={**os.environ, **SENTRY_ENV},
        )

    step(f"Shipping {image} to {server.host}...")
    save = subprocess.Popen(
        ["docker", "save", image], stdout=subprocess.PIPE, stdin=subprocess.DEVNULL
    )
    compress = subprocess.Popen(
        ["zstd", "-T0", "-3", "-q"], stdin=save.stdout, stdout=subprocess.PIPE
    )
    save.stdout.close()
    load = server.shell(
        "zstd -dcq | docker load --quiet", cwd=False, check=False, stdin=compress.stdout
    )
    compress.stdout.close()
    if save.wait() != 0 or compress.wait() != 0 or load.returncode != 0:
        raise DeployError(f"could not ship {image} to {server.host}")


def copy_files(server: Server, target: str) -> None:
    """Before anything starts, and only into the idle colour's directory: the
    one serving keeps reading its own. The directory is replaced whole, so it
    is exactly what is here: a key file removed here is gone from the next
    release rather than lingering."""
    step("Copying the compose file and proxy config...")
    runtime = tar_of(
        {name: (BUILD_DIR / name, mode) for name, mode in RUNTIME_FILES.items()}
    )
    server.put(runtime, "tar -xf -")

    config_dir = f"config-{target.removeprefix('oeee-cafe-')}"
    step(f"Copying {DEPLOY_CONFIG_DIR} to {config_dir}...")
    config_files = {
        str(path.relative_to(DEPLOY_CONFIG_DIR)): (path, 0o600)
        for path in DEPLOY_CONFIG_DIR.rglob("*")
        if path.is_file() and path.name != ".DS_Store"
    }
    staging = shlex.quote(f"{config_dir}.new")
    server.put(
        tar_of(config_files, dir_mode=0o700),
        f"umask 077 && rm -rf {staging} && mkdir {staging} && tar -xf - -C {staging}"
        f" && rm -rf {shlex.quote(config_dir)} && mv {staging} {shlex.quote(config_dir)}",
    )


def colours(server: Server) -> tuple[str, str]:
    """(the colour serving, the other one).

    The proxy's own config is the only honest answer to "what is serving
    right now", so read the colour back off it rather than tracking it
    separately. Absent reads as blue, so a fresh server brings up green first
    and the steps after skip what does not exist."""
    upstream = server.run(
        "cat",
        UPSTREAM_FILE,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if "oeee-cafe-green" in upstream.stdout.decode():
        return "oeee-cafe-green", "oeee-cafe-blue"
    return "oeee-cafe-blue", "oeee-cafe-green"


def hand_over(server: Server, active: str, target: str) -> None:
    """From a target that has just been started to one serving alone: wait for
    it to be healthy, point the proxy at it, drain and stop the active one. A
    target that never answers is stopped again and the active one left
    serving."""
    step(f"Waiting for {target} to answer /health...")
    took = poll(
        HEALTH_TIMEOUT_SECONDS,
        lambda: server.succeeds(
            "docker",
            "compose",
            "exec",
            "-T",
            target,
            "curl",
            "-fsS",
            "-o",
            "/dev/null",
            "http://localhost:3000/health",
        ),
    )
    if took is None:
        server.run("docker", "compose", "logs", "--tail", "50", target, check=False)
        server.run("docker", "compose", "stop", target, check=False)
        raise DeployError(f"{target} never became healthy; leaving {active} serving")
    print(f"{target} is healthy after {took}s")

    step(f"Pointing the proxy at {target}...")
    server.put(f"reverse_proxy {target}:3000\n".encode(), f"cat > {UPSTREAM_FILE}")
    if (
        server.run("docker", "compose", "up", "-d", "proxy", check=False).returncode
        != 0
    ):
        raise DeployError("failed to start the proxy")
    # A proxy that had to start just now already read the new upstream, and
    # Caddy treats a reload to an identical config as a no-op, so this is only
    # doing work in the usual case where it was already running. Retried
    # because a proxy that did just start may not have its admin endpoint up
    # yet.
    reload = (
        "docker",
        "compose",
        "exec",
        "-T",
        "proxy",
        "caddy",
        "reload",
        "--config",
        "/etc/caddy/Caddyfile",
    )
    if poll(30, lambda: server.succeeds(*reload)) is None:
        server.run(*reload, check=False)
        raise DeployError(f"could not reload the proxy onto {target}")

    step("Verifying the published port...")
    if (
        poll(
            30,
            lambda: server.succeeds(
                "curl", "-fsS", "-o", "/dev/null", "http://localhost:30000/health"
            ),
        )
        is None
    ):
        server.run("docker", "compose", "logs", "--tail", "50", "proxy", check=False)
        raise DeployError(f"the published port is not serving {target}")

    if server.succeeds("docker", "container", "inspect", active):
        step(f"Draining {active} for {DRAIN_SECONDS}s...")
        time.sleep(DRAIN_SECONDS)
        step(f"Stopping {active}...")
        # SIGTERM here is what tells its drawing sessions to say goodbye; their
        # clients reconnect through the proxy onto the target, already up.
        server.run("docker", "compose", "stop", active)


def switch(server: Server, commit: str) -> None:
    image = f"oeee-cafe:{commit}"
    active, target = colours(server)
    step(f"{active} is serving; deploying to {target}")

    copy_files(server, target)

    # Both colours are declared with oeee-cafe:current. Moving that tag does
    # not touch the colour that is serving: a container holds the image it was
    # created from, not the name.
    server.run("docker", "tag", image, "oeee-cafe:current")

    step(f"Starting {target}...")
    if (
        server.run(
            "docker", "compose", "up", "-d", "--force-recreate", target, check=False
        ).returncode
        != 0
    ):
        raise DeployError(f"failed to start {target}; {active} is still serving")
    hand_over(server, active, target)

    # Every release is a 1.4GB image, and nothing else removes them. Keep the
    # one just started and whatever the stopped colour was created from, which
    # is the rollback; docker refuses to remove an image a container still
    # uses, so the latter needs no special case. Untagged ones are what deploys
    # before oeee-cafe:<commit> tags left behind -- recognised by their
    # command, since they have no name left to go by.
    step("Removing old release images...")
    for tag in server.output(
        "docker", "image", "ls", "oeee-cafe", "--format", "{{.Tag}}"
    ).split():
        if tag not in ("current", commit):
            server.run("docker", "rmi", f"oeee-cafe:{tag}", check=False, quiet=True)
    for image_id in server.output(
        "docker", "image", "ls", "--quiet", "--filter", "dangling=true"
    ).split():
        cmd = server.output(
            "docker",
            "image",
            "inspect",
            "--format",
            "{{index .Config.Cmd 0}}",
            image_id,
            check=False,
        )
        if cmd == "./oeee-cafe":
            server.run("docker", "rmi", image_id, check=False, quiet=True)

    step("Deployment successful!")
    server.run("docker", "compose", "ps")


def release_of(server: Server, container: str) -> str | None:
    """The commit a container's image was built from, as the Dockerfile set it
    in GIT_COMMIT: the container's own record, which outlasts any tag."""
    env = server.output(
        "docker",
        "container",
        "inspect",
        "--format",
        "{{range .Config.Env}}{{println .}}{{end}}",
        container,
        check=False,
    )
    for line in env.splitlines():
        if line.startswith("GIT_COMMIT="):
            return line.removeprefix("GIT_COMMIT=") or None
    return None


def cli(args: list[str]) -> int:
    """The admin CLI, `./oeee-cafe cli ...`, in whichever colour is serving,
    found the way a deploy finds it, so nobody has to know which colour the
    last deploy landed on."""
    server = Server(DEPLOY_HOST)
    try:
        active, _ = colours(server)
        flags = ["-it"] if sys.stdin.isatty() else ["-i"]
        return server.interactive(
            "docker",
            "exec",
            *flags,
            active,
            "./oeee-cafe",
            "cli",
            "-c",
            "config/config.toml",
            *args,
        )
    finally:
        server.close()


def rollback() -> None:
    server = Server(DEPLOY_HOST)
    try:
        active, target = colours(server)
        if not server.succeeds("docker", "container", "inspect", target):
            raise DeployError(f"there is no {target} to roll back to")
        serving, going_back_to = release_of(server, active), release_of(server, target)
        step(
            f"Rolling back from {active} ({(serving or 'unknown')[:12]}) to {target} ({(going_back_to or 'unknown')[:12]})"
        )

        step(f"Starting {target} as it was...")
        if (
            server.run("docker", "compose", "start", target, check=False).returncode
            != 0
        ):
            raise DeployError(f"failed to start {target}; {active} is still serving")
        hand_over(server, active, target)
        step("Rollback successful!")
        server.run("docker", "compose", "ps")
    finally:
        server.close()
    if going_back_to:
        record_deploy(going_back_to, name="rollback")


def remove_local_images(commit: str) -> None:
    """The server has the image now, and this copy only existed to be sent
    there. Keeping the one just deployed makes a retry cheap; the build cache
    that makes the next build fast is separate and stays."""
    for tag in output(
        "docker", "image", "ls", "oeee-cafe", "--format", "{{.Tag}}"
    ).split():
        if tag != commit:
            run("docker", "rmi", f"oeee-cafe:{tag}", check=False, quiet=True)


def sentry(*args: str) -> bool:
    """A sentry-cli command against this project, reported rather than fatal:
    the release is already out by the time most of these run, and Sentry being
    unreachable should not decide whether it stays."""
    if SENTRY_UPLOAD == "skip":
        return False
    if (
        run(
            "sentry-cli", *args, check=False, env={**os.environ, **SENTRY_ENV}
        ).returncode
        != 0
    ):
        print(f"    (warning: `sentry-cli {shlex.join(args)}` failed; carrying on)")
        return False
    return True


def environment() -> str:
    """The environment the server reports its errors under, from the same
    config it boots with, so the deploy is recorded against the same one."""
    config = tomllib.loads((DEPLOY_CONFIG_DIR / "config.toml").read_text())
    return str(config.get("env") or "production")


def create_release(commit: str) -> None:
    """Before the release goes out, so its first error has somewhere to go.
    set-commits needs the repository connected to Sentry; without that, the
    release still exists and only its commit list is empty.

    The commit is named rather than left to --auto, which reads HEAD of
    whatever checkout this runs in: that is often ahead of origin/main, and
    the release would claim commits that are not in it."""
    step(f"Creating release {commit[:12]} in Sentry...")
    if sentry("releases", "new", commit):
        sentry(
            "releases",
            "set-commits",
            commit,
            "--commit",
            f"{SENTRY_REPOSITORY}@{commit}",
            "--ignore-missing",
        )


def record_deploy(
    commit: str, name: str | None = None, started: int | None = None
) -> None:
    step(f"Recording the {name or 'deploy'} of {commit[:12]} in Sentry...")
    args = ["deploys", "new", "--release", commit, "--env", environment()]
    if name:
        args += ["--name", name]
    if started:
        args += ["--started", str(started), "--finished", str(int(time.time()))]
    sentry(*args)


def deploy() -> None:
    check_config()
    ensure_docker()
    # Checked before the build rather than after it, which is when it would fail.
    if SENTRY_UPLOAD != "skip" and not succeeds(
        "env", *(f"{k}={v}" for k, v in SENTRY_ENV.items()), "sentry-cli", "info"
    ):
        raise DeployError(
            f"sentry-cli cannot reach {SENTRY_ENV['SENTRY_ORG']}/{SENTRY_ENV['SENTRY_PROJECT']}: run\n"
            "       `sentry-cli login`, or deploy without debug info in Sentry\n"
            "       with SENTRY_UPLOAD=skip"
        )
    started = int(time.time())
    commit = fetch_main()
    server = Server(DEPLOY_HOST)
    try:
        build_and_ship(server, commit)
        create_release(commit)
        switch(server, commit)
    finally:
        server.close()
    sentry("releases", "finalize", commit)
    record_deploy(commit, started=started)
    remove_local_images(commit)


COMMANDS = {"deploy": deploy, "rollback": rollback}


def main() -> int:
    # Not under the lock: the CLI changes nothing a deploy does, and is often
    # what is wanted while one runs.
    if sys.argv[1:2] == ["cli"]:
        try:
            return cli(sys.argv[2:])
        except DeployError as error:
            print(f"ERROR: {error}", file=sys.stderr)
            return 1
    if len(sys.argv) != 2 or sys.argv[1] not in COMMANDS:
        print(f"usage: {sys.argv[0]} {{{'|'.join(COMMANDS)}|cli ...}}", file=sys.stderr)
        return 2
    command = COMMANDS[sys.argv[1]]
    BUILD_DIR.parent.mkdir(parents=True, exist_ok=True)
    # One at a time: two deploys would share the build checkout and the build
    # database's port, and a deploy and a rollback would fight over which
    # colour is live.
    try:
        LOCK_DIR.mkdir()
    except FileExistsError:
        print(
            f"ERROR: another deploy is in progress (remove {LOCK_DIR} if it is not)",
            file=sys.stderr,
        )
        return 1
    try:
        command()
        return 0
    except DeployError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("ERROR: interrupted", file=sys.stderr)
        return 130
    finally:
        subprocess.run(
            ["docker", "rm", "-fv", BUILD_DB],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
        )
        LOCK_DIR.rmdir()


if __name__ == "__main__":
    sys.exit(main())
