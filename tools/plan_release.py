"""Select release work without allocating build runners for unchanged nightlies."""

import json
import os
import subprocess


def github_api(path):
    return json.loads(subprocess.check_output(["gh", "api", path], text=True))


def records(api, path, key):
    page = 1
    seen = 0
    while True:
        response = api(f"{path}&page={page}")
        batch = response[key]
        yield from batch
        seen += len(batch)
        if not batch or seen >= response["total_count"]:
            return
        page += 1


def previous_nightly(repository, run_id, api):
    runs = records(
        api,
        f"repos/{repository}/actions/workflows/release.yml/runs"
        "?branch=main&status=success&per_page=100",
        "workflow_runs",
    )
    for run in runs:
        if (run["id"] == run_id or run["conclusion"] != "success"
                or run["head_branch"] != "main"):
            continue
        if run["event"] == "schedule":
            # A successful scheduled no-op records that this head was released.
            return run
        if run["event"] in ("push", "workflow_dispatch"):
            jobs = records(
                api,
                f"repos/{repository}/actions/runs/{run['id']}/jobs?per_page=100",
                "jobs",
            )
            if any(job["name"] == "publish-nightly" and job["conclusion"] == "success"
                   for job in jobs):
                return run
    return None


def plan_release(event, ref, head, repository, run_id, publish_nightly=False, api=github_api):
    result = {"build": True, "publish": False, "rolling": False, "reason": "Manual verification build"}
    if event == "workflow_dispatch":
        if publish_nightly:
            if ref != "refs/heads/main":
                raise ValueError("Manual nightly publication requires main")
            result.update(publish=True, rolling=True, reason="Manual nightly publication")
        return result
    if event == "push" and ref.startswith("refs/tags/v"):
        result.update(publish=True, reason="Version tag release")
        return result
    if event != "schedule" or ref != "refs/heads/main":
        result.update(build=False, reason="This event does not request a release")
        return result
    previous = previous_nightly(repository, run_id, api)
    if previous is not None and previous["head_sha"] == head:
        result.update(build=False, reason=f"Head unchanged since successful nightly run {previous['id']}")
        return result
    result.update(publish=True, rolling=True, reason="New head requires a nightly release")
    return result


def main(environ=None, api=github_api):
    environ = os.environ if environ is None else environ
    result = plan_release(
        environ["GITHUB_EVENT_NAME"], environ["GITHUB_REF"], environ["GITHUB_SHA"],
        environ["GITHUB_REPOSITORY"], int(environ["GITHUB_RUN_ID"]),
        environ.get("MANUAL_PUBLISH", "false") == "true", api,
    )
    for key in ("build", "publish", "rolling"):
        print(f"{key}={str(result[key]).lower()}")
    print(f"reason={result['reason']}")


if __name__ == "__main__":
    main()
