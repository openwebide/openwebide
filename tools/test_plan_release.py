"""Exercise release event routing and successful-head detection."""

from contextlib import redirect_stdout
import io
import unittest

from plan_release import main, plan_release


HEAD = "a" * 40
OLD = "b" * 40


def run(identifier, head=HEAD, event="schedule", conclusion="success", branch="main"):
    return {"id": identifier, "head_sha": head, "event": event,
            "conclusion": conclusion, "head_branch": branch}


class ReleasePlanTests(unittest.TestCase):
    def plan(self, runs=(), jobs=(), **overrides):
        def api(path):
            if "/jobs?" in path:
                return {"jobs": list(jobs), "total_count": len(jobs)}
            return {"workflow_runs": list(runs), "total_count": len(runs)}
        arguments = dict(event="schedule", ref="refs/heads/main", head=HEAD,
                         repository="example/project", run_id=100, api=api)
        arguments.update(overrides)
        return plan_release(**arguments)

    def test_first_nightly_builds_and_publishes(self):
        decision = self.plan()
        self.assertTrue(decision["build"] and decision["publish"] and decision["rolling"])

    def test_unchanged_successful_head_skips_all_release_work(self):
        decision = self.plan([run(90)])
        self.assertFalse(any(decision[key] for key in ("build", "publish", "rolling")))

    def test_changed_head_builds_a_new_nightly(self):
        self.assertTrue(self.plan([run(90, head=OLD)])["publish"])

    def test_a_failed_attempt_does_not_suppress_retrying_an_unreleased_head(self):
        decision = self.plan([run(95, conclusion="failure"), run(90, head=OLD)])
        self.assertTrue(decision["build"] and decision["publish"])

    def test_current_run_cannot_prove_its_own_success(self):
        self.assertTrue(self.plan([run(100), run(90, head=OLD)])["build"])

    def test_a_successful_scheduled_noop_keeps_the_head_released(self):
        self.assertFalse(self.plan([run(95), run(90)])["build"])

    def test_latest_success_is_used_when_a_head_is_reverted(self):
        self.assertTrue(self.plan([run(95, head=OLD), run(90)])["build"])

    def test_other_branches_do_not_suppress_main_publication(self):
        self.assertTrue(self.plan([run(95, branch="feature"), run(90, head=OLD)])["build"])

    def test_old_main_push_publications_remain_valid(self):
        decision = self.plan([run(90, event="push")],
                             jobs=[{"name": "publish-nightly", "conclusion": "success"}])
        self.assertFalse(decision["build"])

    def test_other_release_channels_do_not_suppress_a_nightly(self):
        decision = self.plan([run(90, event="push")],
                             jobs=[{"name": "publish", "conclusion": "success"}])
        self.assertTrue(decision["build"])

    def test_manual_verification_does_not_suppress_a_nightly(self):
        decision = self.plan([run(90, event="workflow_dispatch")],
                             jobs=[{"name": "publish-nightly", "conclusion": "skipped"}])
        self.assertTrue(decision["publish"])

    def test_successful_manual_publication_counts_as_released(self):
        decision = self.plan([run(90, event="workflow_dispatch")],
                             jobs=[{"name": "publish-nightly", "conclusion": "success"}])
        self.assertFalse(decision["build"])

    def test_manual_build_and_manual_publication_can_force_unchanged_heads(self):
        def no_history(_):
            self.fail("Manual runs must not query history")
        build = self.plan(event="workflow_dispatch", api=no_history)
        self.assertTrue(build["build"])
        self.assertFalse(build["publish"] or build["rolling"])
        publish = self.plan(event="workflow_dispatch", publish_nightly=True, api=no_history)
        self.assertTrue(publish["build"] and publish["publish"] and publish["rolling"])

    def test_manual_nightly_cannot_publish_another_branch(self):
        with self.assertRaisesRegex(ValueError, "requires main"):
            self.plan(event="workflow_dispatch", ref="refs/heads/feature", publish_nightly=True)

    def test_version_tags_publish_without_rolling_the_nightly(self):
        decision = self.plan(event="push", ref="refs/tags/v1.0.0")
        self.assertTrue(decision["build"] and decision["publish"])
        self.assertFalse(decision["rolling"])

    def test_ordinary_main_pushes_do_not_request_releases(self):
        decision = self.plan(event="push")
        self.assertFalse(any(decision[key] for key in ("build", "publish", "rolling")))

    def test_pagination_can_find_publication_beyond_verification_only_runs(self):
        def api(path):
            if "/jobs?" in path:
                return {"jobs": [], "total_count": 0}
            page = path.rsplit("page=", 1)[1]
            rows = [run(95, event="workflow_dispatch")] if page == "1" else [run(90)]
            return {"workflow_runs": rows, "total_count": 2}
        self.assertFalse(self.plan(api=api)["build"])

    def test_history_errors_fail_instead_of_falsely_skipping_work(self):
        def unavailable(_):
            raise RuntimeError("History unavailable")
        with self.assertRaisesRegex(RuntimeError, "History unavailable"):
            self.plan(api=unavailable)

    def test_ci_outputs_follow_the_decision(self):
        environ = {"GITHUB_EVENT_NAME": "schedule", "GITHUB_REF": "refs/heads/main",
                   "GITHUB_SHA": HEAD, "GITHUB_REPOSITORY": "example/project", "GITHUB_RUN_ID": "100"}
        stream = io.StringIO()
        with redirect_stdout(stream):
            main(environ, api=lambda _: {"workflow_runs": [run(90)], "total_count": 1})
        outputs = dict(line.split("=", 1) for line in stream.getvalue().splitlines())
        self.assertEqual([outputs[key] for key in ("build", "publish", "rolling")], ["false"] * 3)


if __name__ == "__main__":
    unittest.main()
