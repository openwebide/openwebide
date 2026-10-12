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


def main(action, request):
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
