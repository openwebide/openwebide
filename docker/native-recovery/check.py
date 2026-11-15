"""Check real Linux bridge namespace continuity using fresh packaged containers.

Usage: python3 docker/native-recovery/check.py IMAGE LINUX_LIBTEST EVIDENCE_DIR
Build the bridge libtest from the same source as IMAGE. The runtime-state target
is sufficient for this namespace-only check. Containers and the owned volume are
retained, including original and replacement fault objects; no workspace mounts
or network are used. This does not test transaction-owner enrollment.
"""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid

image, executable, output = sys.argv[1:]
executable = Path(executable).resolve(strict=True)
output = Path(output).resolve()
output.mkdir(parents=True, exist_ok=False)
prefix = "openwebide-recovery-" + uuid.uuid4().hex[:12]
volume = prefix + "-data"
test = "native_recovery::tests::packaged_principal_uses_the_same_original_state_across_containers"
state = "/app/.spin/native-principal"
rows = []


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for data in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(data)
    return digest.hexdigest()


def run(name, command, expected=0):
    started = time.monotonic()
    result = subprocess.run(command, capture_output=True, timeout=600)
    (output / (name + ".stdout")).write_bytes(result.stdout)
    (output / (name + ".stderr")).write_bytes(result.stderr)
    rows.append({"case": name, "command": command, "returncode": result.returncode,
                 "expected_returncode": expected, "seconds": round(time.monotonic() - started, 2),
                 "stdout_sha256": hashlib.sha256(result.stdout).hexdigest(),
                 "stderr_sha256": hashlib.sha256(result.stderr).hexdigest()})
    (output / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
    assert result.returncode == expected, (name, result.returncode, expected)
    return (result.stdout + result.stderr).decode()


image = run("image", ["docker", "image", "inspect", image, "--format", "{{.Id}}" ]).strip()
assert re.fullmatch(r"sha256:[0-9a-f]{64}", image)
original = sha(executable)
run("volume", ["docker", "volume", "create", "--label", "org.openwebide.recovery-check=" + prefix, volume])
base = ["docker", "run", "--network", "none", "--read-only", "--volume", volume + ":/app/.spin"]


def check(name, profile, expected=0):
    text = run(name, base + ["--name", prefix + "-" + name,
        "--volume", str(executable) + ":/recovery-test:ro",
        "--env", "OPENWEBIDE_TEST_PACKAGED_PRINCIPAL=1",
        "--env", "HOME=/" + profile, "--env", "XDG_STATE_HOME=/" + profile + "/state",
        "--env", "XDG_CONFIG_HOME=/" + profile + "/config", "--env", "XDG_CACHE_HOME=/" + profile + "/cache",
        "--entrypoint", "/recovery-test", image, "--exact", test, "--nocapture"], expected)
    if expected:
        assert "Native recovery namespace is incomplete or replaced" in text
        assert "test result: FAILED. 0 passed; 1 failed" in text
        return None
    assert "test result: ok. 1 passed; 0 failed; 0 ignored;" in text
    return re.search(r"^packaged_namespace_digest=(\S+)$", text, re.M).group(1)


def shell(name, script):
    return run(name, base + ["--name", prefix + "-" + name, "--entrypoint", "/bin/sh", image, "-ec", script])


def observe(name):
    return shell(name, "cd " + state + "; stat -c '%d %i %W %s %a %u %g %h' . .openwebide-native-recovery .openwebide-native-recovery-owner .openwebide-native-recovery/recovery.sqlite3 .openwebide-native-recovery/recovery-head.json; sha256sum .openwebide-native-recovery-owner .openwebide-native-recovery/recovery.sqlite3 .openwebide-native-recovery/recovery-head.json")


complete = False
try:
    first = check("first", "profile-one")
    before = observe("before")
    assert check("second", "profile-two") == first
    assert observe("after-second") == before
    shell("hide-original", "cd " + state + "; mv .openwebide-native-recovery/recovery.sqlite3 retained-original.sqlite3")
    check("missing", "profile-three", 101)
    shell("copy-original", "cd " + state + "; cp -p retained-original.sqlite3 .openwebide-native-recovery/recovery.sqlite3")
    check("copied", "profile-four", 101)
    shell("restore-original", "cd " + state + "; mv .openwebide-native-recovery/recovery.sqlite3 retained-copy.sqlite3; mv retained-original.sqlite3 .openwebide-native-recovery/recovery.sqlite3")
    assert check("restored", "profile-five") == first
    shell("hide-head", "cd " + state + "; mv .openwebide-native-recovery/recovery-head.json retained-original-head.json")
    check("missing-head", "profile-six", 101)
    shell("copy-head", "cd " + state + "; cp -p retained-original-head.json .openwebide-native-recovery/recovery-head.json")
    check("copied-head", "profile-seven", 101)
    shell("restore-head", "cd " + state + "; mv .openwebide-native-recovery/recovery-head.json retained-copy-head.json; mv retained-original-head.json .openwebide-native-recovery/recovery-head.json")
    assert check("restored-head", "profile-eight") == first
    # Directory size changes when retained fault files are added. The five
    # original objects' device/inode/birth, mode, owner and link count must match;
    # size and content remain exact for the three original regular files.
    after = observe("after-restored")
    a, b = before.splitlines(), after.splitlines()
    for index in range(5):
        left, right = a[index].split(), b[index].split()
        assert left[:3] == right[:3] and left[4:] == right[4:]
        if index >= 2:
            assert left == right
    assert a[5:] == b[5:]
    assert sha(executable) == original
    complete = True
finally:
    (output / "terminal.json").write_text(json.dumps({"complete": complete, "image": image,
        "linux_libtest": str(executable), "linux_libtest_sha256": original,
        "volume": volume, "container_prefix": prefix, "retained": True,
        "namespace_metadata_only": True, "owner_enrollment_verified": False,
        "cases": rows}, indent=2) + "\n")
