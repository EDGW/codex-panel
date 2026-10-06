#!/usr/bin/env python3
"""Exercise the piped installer offline, using isolated homes and release assets.

Pass a real release executable to also verify installing that executable in CI.
"""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = (ROOT / "install.sh").read_text()
MARKER = "# >>> codex-panel PATH >>>"
BASE_ENV = {key: value for key, value in os.environ.items()
            if not key.startswith("CC_PANEL_")}


def executable(path, content):
    path.write_text(content)
    path.chmod(0o755)


with tempfile.TemporaryDirectory(prefix="codex-panel-installer-") as directory:
    sandbox = Path(directory)
    mocks = sandbox / "mocks"
    mocks.mkdir()
    executable(mocks / "mock", """#!/usr/bin/env python3
import os, pathlib, shutil, sys
command = pathlib.Path(sys.argv[0]).name
if command == 'uname':
    print(os.environ.get('TEST_OS', 'Linux') if sys.argv[1] == '-s'
          else os.environ.get('TEST_ARCH', 'x86_64'))
elif command == 'getconf':
    print(os.environ.get('TEST_LIBC', 'glibc 2.39'))
elif command == 'curl':
    if os.environ.get('TEST_DOWNLOAD_FAILURE'):
        sys.exit(22)
    url = sys.argv[-1]
    name = url.rsplit('/', 1)[1]
    version = (os.environ['TEST_LATEST'] if '/latest/download/' in url
               else url.split('/download/', 1)[1].split('/', 1)[0])
    source = pathlib.Path(os.environ['TEST_RELEASES']) / version / name
    destination = pathlib.Path(sys.argv[sys.argv.index('--output') + 1])
    shutil.copyfile(source, destination)
    if os.environ.get('TEST_BAD_CHECKSUM') and name.endswith('.sha256'):
        destination.write_text('0' * 64 + '  ' + name[:-7] + '\\n')
else:
    sys.exit(1)
""")
    for command in ("curl", "uname", "getconf"):
        (mocks / command).symlink_to(mocks / "mock")

    releases = sandbox / "releases"
    latest = "v0.1.2"
    actual_binary = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None
    if actual_binary:
        latest = subprocess.check_output(
            [str(actual_binary), "--panel-version"], text=True, env=BASE_ENV
        ).strip().removeprefix("codex-panel ")

    def release(version, architecture, real_binary=None):
        output = releases / version
        output.mkdir(parents=True, exist_ok=True)
        package = f"codex-panel-{architecture}-unknown-linux-gnu"
        payload = sandbox / "payload" / version / package
        payload.mkdir(parents=True, exist_ok=True)
        binary = payload / "codex-panel"
        if real_binary:
            shutil.copyfile(real_binary, binary)
            binary.chmod(0o755)
        else:
            executable(binary, f"#!/bin/sh\nprintf 'codex-panel {version}\\n'\n")
        shutil.copyfile(ROOT / "destinations.toml", payload / "destinations.toml")
        archive = output / f"{package}.tar.gz"
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(payload, arcname=package)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        archive.with_name(archive.name + ".sha256").write_text(
            f"{digest}  {archive.name}\n"
        )

    # Both architectures are covered offline. CI additionally installs its native binary.
    for architecture in ("x86_64", "aarch64"):
        release("v0.0.1", architecture)
        release(latest, architecture, actual_binary)

    def environment(name, *, in_path=False, custom_dirs=False):
        user_dir = sandbox / name
        user_dir.mkdir()
        (user_dir / ".codex-panel").mkdir()
        (user_dir / ".codex-panel/destinations.toml").write_text("personal config\n")
        (user_dir / ".codex").mkdir()
        (user_dir / ".codex/auth.json").write_text("private credentials\n")
        for rc in (".bashrc", ".profile", ".bash_profile", ".zshrc"):
            (user_dir / rc).write_text("# existing settings\n")
        env = BASE_ENV.copy()
        for key in ("XDG_DATA_HOME", "ZDOTDIR", "BASH_ENV"):
            env.pop(key, None)
        env.update(HOME=str(user_dir), PATH=f"{mocks}:/usr/bin:/bin",
                   TEST_RELEASES=str(releases), TEST_LATEST=latest)
        env["TMPDIR"] = str(user_dir / "tmp")
        Path(env["TMPDIR"]).mkdir()
        if in_path:
            env["PATH"] = f"{user_dir}/.local/bin:" + env["PATH"]
        if custom_dirs:
            env["XDG_DATA_HOME"] = str(user_dir / "custom data's $literal")
            env["ZDOTDIR"] = str(user_dir / "zsh settings")
        return env

    def install(env, *args, succeeds=True):
        # stdin is a pipe, matching curl ... | bash.
        result = subprocess.run(["bash", "-s", "--", *args], input=SCRIPT,
                                text=True, capture_output=True, env=env)
        assert (result.returncode == 0) == succeeds, result.stdout + result.stderr
        assert not list(Path(env["TMPDIR"]).iterdir()), "Temporary download leaked"
        return result

    def run(env, *args):
        return subprocess.check_output(args, text=True, env=env)

    def verify_removal(env):
        user_dir = Path(env["HOME"])
        data = Path(env.get("XDG_DATA_HOME", user_dir / ".local/share"))
        payload = data / "codex-panel/installation"
        unrelated = data / "codex-panel/keep.txt"
        unrelated.write_text("unrelated data\n")
        # A later shell configuration change must survive removal.
        with (user_dir / ".bashrc").open("a") as rc:
            rc.write("# later settings\n")
        command = user_dir / ".local/bin/codex-panel-remove"
        run(env, str(command), "--help")
        assert command.exists() and payload.exists()
        run(env, str(command))
        assert not payload.exists()
        for name in ("codex-panel", "codex-panel-remove"):
            assert not (user_dir / ".local/bin" / name).is_symlink()
        rc_files = [user_dir / rc for rc in (".bashrc", ".profile", ".bash_profile", ".zshrc")]
        if "ZDOTDIR" in env:
            rc_files.append(Path(env["ZDOTDIR"]) / ".zshrc")
        for rc in rc_files:
            assert MARKER not in rc.read_text()
        assert "# later settings\n" in (user_dir / ".bashrc").read_text()
        assert unrelated.read_text() == "unrelated data\n"
        assert (user_dir / ".codex-panel/destinations.toml").read_text() == "personal config\n"
        assert (user_dir / ".codex/auth.json").read_text() == "private credentials\n"

    env = environment("user space's $literal", custom_dirs=True)
    install(env, "--version", "v0.0.1")
    binary = Path(env["HOME"]) / ".local/bin/codex-panel"
    assert run(env, str(binary), "--panel-version").strip() == "codex-panel v0.0.1"
    old_bytes = binary.read_bytes()
    install(dict(env, TEST_BAD_CHECKSUM="1"), succeeds=False)
    assert binary.read_bytes() == old_bytes, "Failed upgrade replaced the installed binary"
    install(env)
    assert run(env, str(binary), "--panel-version").strip() == f"codex-panel {latest}"
    # Installation may be launched inside a running panel's Codex process.
    install(dict(env, CC_PANEL_PROCESS_KIND="codex"))
    for rc in (".bashrc", ".profile", ".bash_profile"):
        assert (Path(env["HOME"]) / rc).read_text().count(MARKER) == 1
    rc = Path(env["HOME"]) / ".bashrc"
    for shell in ("bash", "sh", "zsh"):
        if shutil.which(shell):
            found = run(env, shell, "-c", '. "$1"; command -v codex-panel-remove', "test", str(rc)).strip()
            assert found == str(binary.with_name("codex-panel-remove"))
    # Reinstall after the configured PATH is active: uninstall must still remove its block.
    env["PATH"] = f"{binary.parent}:" + env["PATH"]
    install(env)
    verify_removal(env)

    env = environment("arm-user", in_path=True)
    env["TEST_ARCH"] = "aarch64"
    install(env)
    for rc in (".bashrc", ".profile", ".zshrc"):
        assert (Path(env["HOME"]) / rc).read_text() == "# existing settings\n"
    verify_removal(env)

    for name, overrides in (
        ("bad-checksum", {"TEST_BAD_CHECKSUM": "1"}),
        ("download-failure", {"TEST_DOWNLOAD_FAILURE": "1"}),
        ("old-glibc", {"TEST_LIBC": "glibc 2.38"}),
        ("unsupported-arch", {"TEST_ARCH": "riscv64"}),
        ("unsupported-os", {"TEST_OS": "Darwin"}),
        ("relative-xdg", {"XDG_DATA_HOME": "relative/path"}),
    ):
        env = environment(name)
        env.update(overrides)
        install(env, succeeds=False)
        assert not (Path(env["HOME"]) / ".local/bin").exists()
        assert (Path(env["HOME"]) / ".bashrc").read_text() == "# existing settings\n"

    env = environment("existing-command")
    binary = Path(env["HOME"]) / ".local/bin/codex-panel"
    binary.parent.mkdir(parents=True)
    binary.write_text("existing installation\n")
    install(env, succeeds=False)
    assert binary.read_text() == "existing installation\n"
    install(env, "--help")
    install(env, "--version", succeeds=False)
    install(env, "--version", "../../invalid", succeeds=False)
    install(env, "--unknown", succeeds=False)

print("Installer checks passed: install, upgrade, checksum failures, PATH, removal, and preserved user data.")
