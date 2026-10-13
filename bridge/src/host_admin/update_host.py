"""Execution primitives for host updates; deployment policy is supplied by shared Rust.

Invoked over authenticated SSH. The resident worker belongs to the host user manager,
so replacing the app container cannot terminate it. No registry credentials are returned.
"""
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def run(argv, timeout=120, cwd=None):
    environment = os.environ.copy()
    environment.pop("OPENWEBIDE_IMAGE", None)
    result = subprocess.run(argv, cwd=cwd, env=environment, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{argv[0]} exited with status {result.returncode}")
    if len(result.stdout) > 1024 * 1024:
        raise RuntimeError("Host response exceeds the update limit")
    return result.stdout


def job_directory(identity):
    if len(identity) != 32 or any(ch not in "0123456789abcdefABCDEF" for ch in identity):
        raise ValueError("Invalid update identity")
    root = Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state")))
    return root / "openwebide/updates" / identity


def save(directory, phase, detail):
    temporary = directory / "status.tmp"
    temporary.write_text(json.dumps({"phase": phase, "detail": detail}))
    temporary.replace(directory / "status.json")


def execute(directory, plan):
    with (directory.parent / "lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        save(directory, "running", "Preparing the published image on the host.")
        time.sleep(3)  # Let the initiating API response reach its client before restart.
        for step in plan:
            if "argv" in step:
                output = run(step["argv"], timeout=900, cwd=step.get("cwd"))
                if "keys" in step:
                    value = json.loads(output)
                    for key in step["keys"]:
                        value = value[key]
                    if value != step["expected"]:
                        raise RuntimeError("Deployment does not use the configured app image variable")
            else:
                path = Path(step["path"])
                text = path.read_text() if path.exists() else ""
                backup = directory / f"backup-{len(list(directory.glob('backup-*')))}"
                backup.write_text(text)
                prefix = step["prefix"]
                lines = text.splitlines()
                matches = [index for index, line in enumerate(lines) if line.startswith(prefix)]
                if len(matches) > 1:
                    raise RuntimeError("Deployment file contains duplicate image settings")
                if matches:
                    lines[matches[0]] = prefix + step["value"]
                elif step.get("require_existing"):
                    raise RuntimeError("Deployment file is missing its image setting")
                else:
                    lines.append(prefix + step["value"])
                temporary = path.with_name(path.name + ".openwebide-update")
                with temporary.open("x") as stream:
                    os.chmod(temporary, path.stat().st_mode & 0o777 if path.exists() else 0o600)
                    stream.write("\n".join(lines) + "\n")
                temporary.replace(path)
        save(directory, "succeeded", "Host deployment restarted. Waiting for the app to reconnect.")


def deployment_evidence():
    """Read public runtime labels and Quadlet source; never return container env."""
    evidence = []
    for runtime in ["docker", "podman"]:
        try:
            ids = run([runtime, "ps", "-q"], timeout=5).split()[:64]
            if not ids:
                continue
            template = '{{json .Id}} {{json .Config.Hostname}} {{json .Name}} {{json .Config.Labels}}'
            output = run([runtime, "inspect", "--format", template, *ids], timeout=10)
            decoder = json.JSONDecoder()
            for line in output.splitlines():
                values = []
                remaining = line
                for _ in range(4):
                    value, end = decoder.raw_decode(remaining.lstrip())
                    values.append(value)
                    remaining = remaining.lstrip()[end:]
                identity, hostname, name, labels = values
                labels = {key: value for key, value in (labels or {}).items() if key in [
                    "com.docker.compose.project.working_dir", "com.docker.compose.project.config_files",
                    "com.docker.compose.service", "PODMAN_SYSTEMD_UNIT"]}
                item = dict(runtime=runtime, id=identity, hostname=hostname, name=name, labels=labels,
                            source_path=None, source_text=None)
                unit = labels.get("PODMAN_SYSTEMD_UNIT", "")
                if unit and not unit.startswith("-") and len(unit) <= 128:
                    try:
                        source = run(["systemctl", "--user", "show", unit,
                                      "--property=SourcePath", "--value"], timeout=5).strip()
                        if source.startswith("/") and source.endswith(".container"):
                            with Path(source).open() as stream:
                                text = stream.read(65537)
                            if len(text) <= 65536:
                                section = ""
                                public = []
                                for line in text.splitlines():
                                    stripped = line.strip()
                                    if stripped.startswith("["):
                                        section = stripped
                                    if section == "[Container]" and stripped.startswith("Image="):
                                        public.append(stripped)
                                item.update(source_path=source, source_text="[Container]\n" + "\n".join(public))
                    except (OSError, RuntimeError, subprocess.TimeoutExpired):
                        pass
                evidence.append(item)
        except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired):
            continue
    return evidence


def main(action, request):
    if action == "deployment":
        print(json.dumps(deployment_evidence()))
        return
    if action == "inspect":
        value = json.loads(run(["skopeo", "inspect", "--no-tags", "docker://" + request["image"]]))
        # Return only public image metadata, never environment variables or credentials.
        print(json.dumps({key: value.get(key) for key in ["Digest", "Labels", "Architecture", "Os"]}))
        return
    directory = job_directory(request["id"])
    if action == "status":
        if not (directory / "status.json").exists():
            state = run(["systemctl", "--user", "show", "openwebide-update.service", "--property=ActiveState", "--value"]).strip()
            print(json.dumps({"phase": "unknown", "idle": state in ["inactive", "failed"], "detail": "No host receipt. Inspect the deployment before retrying."}))
        else:
            print((directory / "status.json").read_text())
        return
    if action == "start":
        directory.mkdir(mode=0o700, parents=True, exist_ok=False)
        save(directory, "starting", "Starting the host updater.")
        helper = directory / "worker.py"
        helper.write_text(request["helper"])
        try:
            run(["systemd-run", "--user", "--collect", "--unit=openwebide-update",
                 "--property=Type=exec", sys.executable, str(helper), "worker",
                 json.dumps({"id": request["id"], "plan": request["plan"]})])
        except Exception:
            save(directory, "failed", "Could not launch the host updater. Check the user systemd manager.")
            raise
        print(json.dumps({"ok": True}))
        return
    if action == "worker":
        try:
            execute(directory, request["plan"])
        except Exception as error:
            save(directory, "failed", str(error))
            raise
        return
    raise ValueError("Unknown update primitive")


if __name__ == "__main__":
    try:
        main(sys.argv[1], json.loads(sys.argv[2]))
    except Exception as error:
        print(json.dumps({"error": str(error)}))
        sys.exit(1)
