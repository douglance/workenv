#!/usr/bin/env python3

from __future__ import annotations

from datetime import datetime
import importlib.util
import json
from pathlib import Path
import tempfile
import sys
import unittest
from zoneinfo import ZoneInfo


SCRIPT = Path(__file__).parents[1] / "scripts" / "reconcile.py"
sys.path.insert(0, str(SCRIPT.parent))
SPEC = importlib.util.spec_from_file_location("space_attention_reconcile", SCRIPT)
assert SPEC and SPEC.loader
reconcile = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reconcile)

LOCAL_TIME = ZoneInfo("America/Detroit")
NOW = datetime(2026, 8, 1, 11, 42, tzinfo=LOCAL_TIME)
NOW_MS = int(NOW.timestamp() * 1000)


def tracked(updated_at_ms: int | None = NOW_MS) -> dict[str, int | None]:
    return {"observed_at_ms": NOW_MS, "updated_at_ms": updated_at_ms}


class PlanUpdatesTests(unittest.TestCase):
    def test_agent_icon_prefixes_the_existing_single_line_value(self) -> None:
        workspaces = [
            {
                "workspace_id": "w1",
                "label": "alpha",
                "agent_status": "working",
                "tokens": {"agent_icon": "\uf120"},
            }
        ]
        self.assertEqual(
            reconcile.plan_updates(
                workspaces, {"w1": tracked()}, NOW_MS, LOCAL_TIME, {}
            ),
            [("w1", "space_working", "\uf120  alpha", "11:42 AM")],
        )

    def test_invalid_agent_icon_does_not_pollute_the_row(self) -> None:
        workspaces = [
            {
                "workspace_id": "w1",
                "label": "alpha",
                "agent_status": "idle",
                "tokens": {"agent_icon": "not-an-icon"},
            }
        ]
        self.assertEqual(
            reconcile.plan_updates(
                workspaces, {"w1": tracked()}, NOW_MS, LOCAL_TIME, {}
            ),
            [("w1", "space_idle", "\uf07b  alpha", "11:42 AM")],
        )




    def test_unknown_project_uses_a_folder_instead_of_a_circle(self) -> None:
        workspaces = [
            {
                "workspace_id": "w1",
                "label": "alpha",
                "agent_status": "done",
                "tokens": None,
            }
        ]
        self.assertEqual(
            reconcile.plan_updates(
                workspaces,
                {"w1": tracked()},
                NOW_MS,
                LOCAL_TIME,
                {},
            ),
            [("w1", "space_done", "\uf07b  alpha", "11:42 AM")],
        )

    def test_agent_icon_overrides_the_project_default(self) -> None:
        workspaces = [
            {
                "workspace_id": "w1",
                "label": "alpha",
                "agent_status": "working",
                "tokens": {"agent_icon": "\uf120"},
            }
        ]
        self.assertEqual(
            reconcile.plan_updates(
                workspaces,
                {"w1": tracked()},
                NOW_MS,
                LOCAL_TIME,
                {},
            ),
            [("w1", "space_working", "\uf120  alpha", "11:42 AM")],
        )

    def test_blocked_replaces_the_normal_state_marker_and_adds_time(self) -> None:
        workspaces = [
            {
                "workspace_id": "w1",
                "label": "alpha",
                "agent_status": "blocked",
                "tokens": None,
            },
            {
                "workspace_id": "w2",
                "label": "beta",
                "agent_status": "working",
                "tokens": None,
            },
        ]
        tracking = {"w1": tracked(), "w2": tracked()}
        self.assertEqual(
            reconcile.plan_updates(workspaces, tracking, NOW_MS, LOCAL_TIME),
            [
                ("w1", "space_blocked", "\uf07b  alpha", "11:42 AM"),
                ("w2", "space_working", "\uf07b  beta", "11:42 AM"),
            ],
        )

    def test_initial_observation_uses_the_time_only(self) -> None:
        workspaces = [
            {
                "workspace_id": "w1",
                "label": "alpha",
                "agent_status": "idle",
                "tokens": None,
            }
        ]
        self.assertEqual(
            reconcile.plan_updates(
                workspaces,
                {"w1": tracked(updated_at_ms=None)},
                NOW_MS,
                LOCAL_TIME,
            ),
            [("w1", "space_idle", "\uf07b  alpha", "11:42 AM")],
        )

    def test_replaces_a_stale_attention_label_with_idle(self) -> None:
        workspaces = [
            {
                "workspace_id": "w1",
                "label": "alpha",
                "agent_status": "idle",
                "tokens": {"attention": "Waiting for answer"},
            }
        ]
        self.assertEqual(
            reconcile.plan_updates(
                workspaces, {"w1": tracked()}, NOW_MS, LOCAL_TIME
            ),
            [("w1", "space_idle", "\uf07b  alpha", "11:42 AM")],
        )

    def test_does_nothing_when_live_metadata_matches(self) -> None:
        workspaces = [
            {
                "workspace_id": "w1",
                "label": "alpha",
                "agent_status": "blocked",
                "tokens": {
                    "space_blocked": "\uf07b  alpha",
                    "space_updated": "11:42 AM",
                },
            }
        ]
        self.assertEqual(
            reconcile.plan_updates(
                workspaces, {"w1": tracked()}, NOW_MS, LOCAL_TIME
            ),
            [],
        )


class FlatSidebarTests(unittest.TestCase):
    def test_old_header_metadata_does_not_hide_a_workspace(self):
        workspace = {
            "workspace_id": "w1", "label": "alpha", "agent_status": "working",
            "tokens": {"space_kind": "header", "space_section": "projects"},
        }
        self.assertEqual(
            reconcile.plan_updates([workspace], {"w1": tracked()}, NOW_MS, LOCAL_TIME),
            [("w1", "space_working", "\uf07b  alpha", "11:42 AM")],
        )

    def test_old_tree_prefix_is_replaced_with_only_the_icon_and_name(self):
        workspace = {
            "workspace_id": "w1", "label": "alpha", "agent_status": "idle",
            "tokens": {"space_idle": "└─ \uf07b  alpha", "space_updated": "11:42 AM"},
        }
        self.assertEqual(
            reconcile.plan_updates([workspace], {"w1": tracked()}, NOW_MS, LOCAL_TIME),
            [("w1", "space_idle", "\uf07b  alpha", "11:42 AM")],
        )


class ProjectInferenceTests(unittest.TestCase):
    def test_semantic_readme_wins_over_the_technology_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[package]\nname = "dock"\n')
            (root / "README.md").write_text(
                "Freight logistics, dock management, and yard management.\n"
            )
            self.assertEqual(reconcile.infer_project_icon("dock", root), "")

    def test_rust_project_uses_the_rust_icon_without_a_semantic_match(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[package]\nname = "alpha"\n')
            self.assertEqual(reconcile.infer_project_icon("alpha", root), "")

    def test_label_can_classify_a_project_before_files_are_populated(self) -> None:
        self.assertEqual(reconcile.infer_project_icon("train-game", None), "")

    def test_inference_reacts_to_changed_project_contents_without_a_cache(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            readme = root / "README.md"
            readme.write_text("Scheduling appointments on a calendar.\n")
            self.assertEqual(reconcile.infer_project_icon("alpha", root), "")
            readme.write_text("A railway switchyard train game.\n")
            self.assertEqual(reconcile.infer_project_icon("alpha", root), "")

    def test_nearest_project_root_is_found_from_a_nested_pane_cwd(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            nested = root / "apps" / "web"
            nested.mkdir(parents=True)
            (root / "package.json").write_text('{"name":"alpha"}\n')
            self.assertEqual(reconcile.find_project_root(str(nested)), root.resolve())

    def test_workspace_root_prefers_the_candidate_matching_the_space_label(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            alpha = base / "alpha"
            beta = base / "beta"
            alpha.mkdir()
            beta.mkdir()
            (alpha / "Cargo.toml").write_text("[workspace]\n")
            (beta / "package.json").write_text("{}\n")
            roots = reconcile.workspace_project_roots(
                [{"workspace_id": "w1", "label": "alpha"}],
                [
                    {"workspace_id": "w1", "cwd": str(beta)},
                    {"workspace_id": "w1", "cwd": str(alpha)},
                ],
            )
            self.assertEqual(roots, {"w1": alpha.resolve()})


class TimestampFormattingTests(unittest.TestCase):
    def test_formats_a_same_day_update_with_the_time(self) -> None:
        self.assertEqual(
            reconcile.format_timestamp(NOW_MS, NOW_MS, LOCAL_TIME),
            "11:42 AM",
        )

    def test_formats_an_earlier_update_with_the_date_and_time(self) -> None:
        updated = datetime(2026, 7, 31, 21, 50, tzinfo=LOCAL_TIME)
        self.assertEqual(
            reconcile.format_timestamp(
                int(updated.timestamp() * 1000), NOW_MS, LOCAL_TIME
            ),
            "Jul 31 9:50 PM",
        )


class EventMappingTests(unittest.TestCase):
    def test_status_and_detection_events_update_their_space(self) -> None:
        for event_name in (
            "pane.agent_status_changed",
            "pane.agent_detected",
        ):
            with self.subTest(event_name=event_name):
                event = {"data": {"workspace_id": "w1"}}
                self.assertEqual(
                    reconcile.timestamp_workspace_ids(event_name, event), {"w1"}
                )

    def test_agent_move_updates_source_and_destination_spaces(self) -> None:
        event = {
            "data": {
                "previous_workspace_id": "w1",
                "pane": {"workspace_id": "w2", "agent": "codex"},
            }
        }
        self.assertEqual(
            reconcile.timestamp_workspace_ids("pane.moved", event),
            {"w1", "w2"},
        )

    def test_non_agent_move_and_other_events_do_not_update_time(self) -> None:
        move = {
            "data": {
                "previous_workspace_id": "w1",
                "pane": {"workspace_id": "w2", "agent": None},
            }
        }
        self.assertEqual(reconcile.timestamp_workspace_ids("pane.moved", move), set())
        self.assertEqual(
            reconcile.timestamp_workspace_ids(
                "workspace.focused", {"data": {"workspace_id": "w1"}}
            ),
            set(),
        )


class TrackingStateTests(unittest.TestCase):
    def test_initializes_current_spaces_and_prunes_closed_spaces(self) -> None:
        state = {
            "version": 1,
            "workspaces": {
                "closed": tracked(),
                "w1": tracked(),
            },
        }
        changed = reconcile.sync_tracking_state(
            state,
            [{"workspace_id": "w1"}, {"workspace_id": "w2"}],
            None,
            {},
            NOW_MS,
        )
        self.assertTrue(changed)
        self.assertEqual(set(state["workspaces"]), {"w1", "w2"})
        self.assertEqual(state["workspaces"]["w2"], tracked(updated_at_ms=None))

    def test_agent_event_records_the_update_time(self) -> None:
        state = {
            "version": 1,
            "workspaces": {"w1": tracked(updated_at_ms=None)},
        }
        changed = reconcile.sync_tracking_state(
            state,
            [{"workspace_id": "w1"}],
            "pane.agent_status_changed",
            {"data": {"workspace_id": "w1"}},
            NOW_MS,
        )
        self.assertTrue(changed)
        self.assertEqual(state["workspaces"]["w1"]["updated_at_ms"], NOW_MS)

    def test_persists_state_and_recovers_from_invalid_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "last-updated.json"
            expected = {"version": 1, "workspaces": {"w1": tracked()}}
            reconcile.save_tracking_state(path, expected)
            self.assertEqual(reconcile.load_tracking_state(path), expected)
            self.assertEqual(json.loads(path.read_text()), expected)

            path.write_text("not json")
            self.assertEqual(
                reconcile.load_tracking_state(path),
                {"version": 1, "workspaces": {}},
            )


if __name__ == "__main__":
    unittest.main()
