#!/usr/bin/env python3
"""Reconcile Space attention markers, agent icons, and update timestamps."""

from __future__ import annotations

from collections import Counter
from datetime import datetime, tzinfo
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
from typing import Any
import unicodedata

SOURCE = "operator.space-attention"
STATE_VERSION = 1
STATE_FILENAME = "last-updated.json"
UPDATED_TOKEN = "space_updated"
AGENT_ICON_TOKEN = "agent_icon"
LEGACY_TOKEN = "attention"
FOLDER_ICON = ""

SEMANTIC_ICON_RULES = (
    ("", ("train game", "railway switchyard", "train", "railway", "switchyard")),
    ("", ("drum", "music", "audio", "guitar")),
    ("", ("shooter", "shooting", "crosshair", "firearm", "gun range")),
    ("", ("screenshot annotation", "annotation tool", "annotat", "drawing")),
    ("", ("scanner", "scanning", "photogrammetry", "spatial scan")),
    ("", ("studio", "creative workspace", "media workspace")),
    ("", ("freight", "logistics", "trucking", "dock management", "yard management")),
    (
        "",
        (
            "scheduling platform",
            "scheduling appointments",
            "appointment scheduling",
            "meeting availability",
            "approved friends can meet",
            "schedule",
            "scheduling",
            "calendar",
            "appointment",
        ),
    ),
    ("", ("pull request", "code review", "review workflow", "worktree review")),
    ("", ("database", "devsql", "sqlite", "postgres", "query engine", "sql")),
    ("", ("lint", "linter", "static analysis", "code quality")),
    ("", ("visualize", "visualization", "diagram", "code wall", "dependency graph")),
    ("", ("process control", "process manager", "command orchestration", "durable execution")),
    ("", ("window manager", "visionpty", "vmux")),
    ("", ("terminal", "multiplexer", "pty", "tmux", "zellij")),
    (
        "",
        (
            "agent first",
            "ai agent",
            "ai provider",
            "llm agent",
            "coding agent",
            "slack",
            "chat",
        ),
    ),
    ("", ("system", "plugin framework", "configuration manager")),
)
PROJECT_ROOT_MARKERS = (
    ".git",
    "Cargo.toml",
    "Gemfile",
    "Package.swift",
    "config.toml",
    "go.mod",
    "package.json",
    "project.yml",
    "pyproject.toml",
)
PROJECT_TEXT_FILES = (
    "README.md",
    "README",
    "SOUL.md",
    "Cargo.toml",
    "Gemfile",
    "Package.swift",
    "config.toml",
    "go.mod",
    "package.json",
    "project.yml",
    "pyproject.toml",
    "wrangler.jsonc",
    "wrangler.toml",
)
MAX_PROJECT_TEXT_BYTES = 16_384
MARKERS = {
    "blocked": ("space_blocked", "?"),
    "done": ("space_done", "●"),
    "working": ("space_working", "●"),
    "idle": ("space_idle", "○"),
    "unknown": ("space_unknown", "·"),
}
MARKER_TOKENS = tuple(marker[0] for marker in MARKERS.values())
OLD_MARKER_TOKENS = (
    "state_blocked",
    "state_done",
    "state_working",
    "state_idle",
    "state_unknown",
)



def new_tracking_state() -> dict[str, Any]:
    return {"version": STATE_VERSION, "workspaces": {}}


def load_tracking_state(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text())
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        return new_tracking_state()
    if not isinstance(payload, dict) or payload.get("version") != STATE_VERSION:
        return new_tracking_state()
    workspaces = payload.get("workspaces")
    if not isinstance(workspaces, dict):
        return new_tracking_state()

    valid_workspaces: dict[str, dict[str, int | None]] = {}
    for workspace_id, record in workspaces.items():
        if not isinstance(workspace_id, str) or not isinstance(record, dict):
            continue
        observed_at_ms = record.get("observed_at_ms")
        updated_at_ms = record.get("updated_at_ms")
        if not isinstance(observed_at_ms, int):
            continue
        if updated_at_ms is not None and not isinstance(updated_at_ms, int):
            continue
        valid_workspaces[workspace_id] = {
            "observed_at_ms": observed_at_ms,
            "updated_at_ms": updated_at_ms,
        }
    return {"version": STATE_VERSION, "workspaces": valid_workspaces}


def save_tracking_state(path: Path, state: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f"{path.name}.tmp")
    temporary.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
    os.replace(temporary, path)


def timestamp_workspace_ids(
    event_name: str | None, event: dict[str, Any]
) -> set[str]:
    data = event.get("data")
    if not isinstance(data, dict):
        return set()

    if event_name in ("pane.agent_status_changed", "pane.agent_detected"):
        workspace_id = data.get("workspace_id")
        return {workspace_id} if isinstance(workspace_id, str) else set()

    if event_name != "pane.moved":
        return set()
    pane = data.get("pane")
    if not isinstance(pane, dict) or not pane.get("agent"):
        return set()
    workspace_ids = {
        data.get("previous_workspace_id"),
        pane.get("workspace_id"),
    }
    return {
        workspace_id
        for workspace_id in workspace_ids
        if isinstance(workspace_id, str) and workspace_id
    }


def sync_tracking_state(
    state: dict[str, Any],
    workspaces: list[dict[str, Any]],
    event_name: str | None,
    event: dict[str, Any],
    now_ms: int,
) -> bool:
    records = state["workspaces"]
    current_ids = {
        workspace_id
        for workspace in workspaces
        if isinstance((workspace_id := workspace.get("workspace_id")), str)
        and workspace_id
    }
    changed = False

    for workspace_id in set(records) - current_ids:
        del records[workspace_id]
        changed = True
    for workspace_id in current_ids:
        if workspace_id not in records:
            records[workspace_id] = {
                "observed_at_ms": now_ms,
                "updated_at_ms": None,
            }
            changed = True

    for workspace_id in timestamp_workspace_ids(event_name, event) & current_ids:
        if records[workspace_id]["updated_at_ms"] != now_ms:
            records[workspace_id]["updated_at_ms"] = now_ms
            changed = True
    return changed


def local_datetime(epoch_ms: int, local_tz: tzinfo | None) -> datetime:
    if local_tz is None:
        return datetime.fromtimestamp(epoch_ms / 1000)
    return datetime.fromtimestamp(epoch_ms / 1000, local_tz)


def clock_time(value: datetime) -> str:
    return value.strftime("%I:%M %p").lstrip("0")


def format_timestamp(
    updated_at_ms: int,
    now_ms: int,
    local_tz: tzinfo | None = None,
) -> str:
    updated = local_datetime(updated_at_ms, local_tz)
    now = local_datetime(now_ms, local_tz)
    if updated.date() == now.date():
        return clock_time(updated)
    return f"{updated.strftime('%b')} {updated.day} {clock_time(updated)}"


def format_tracking(
    observed_at_ms: int,
    local_tz: tzinfo | None = None,
) -> str:
    return clock_time(local_datetime(observed_at_ms, local_tz))


def valid_agent_icon(value: Any) -> str | None:
    """Return one printable glyph, or ignore malformed metadata."""
    if not isinstance(value, str) or len(value) != 1:
        return None
    category = unicodedata.category(value)
    if value.isspace() or category in {"Cc", "Cf", "Cs", "Cn"}:
        return None
    return value


def normalized_text(value: str) -> str:
    return " ".join(re.sub(r"[^a-z0-9]+", " ", value.lower()).split())


def keyword_present(text: str, keyword: str) -> bool:
    keyword = normalized_text(keyword)
    if len(keyword) <= 3:
        return re.search(rf"(?:^| )({re.escape(keyword)})(?: |$)", text) is not None
    return keyword in text


def find_project_root(cwd: str | None) -> Path | None:
    if not isinstance(cwd, str) or not cwd:
        return None
    path = Path(cwd).expanduser()
    try:
        path = path.resolve()
    except OSError:
        return None
    if not path.is_dir():
        return None

    home = Path.home().resolve()
    for candidate in (path, *path.parents):
        if any((candidate / marker).exists() for marker in PROJECT_ROOT_MARKERS):
            return candidate
        if all((candidate / marker).exists() for marker in ("HEAD", "objects", "refs")):
            return candidate
        if candidate == home:
            break
    return path


def workspace_project_roots(
    workspaces: list[dict[str, Any]], panes: list[dict[str, Any]]
) -> dict[str, Path]:
    candidates: dict[str, list[Path]] = {}
    for pane in panes:
        workspace_id = pane.get("workspace_id")
        if not isinstance(workspace_id, str):
            continue
        root = find_project_root(pane.get("cwd"))
        if root is not None:
            candidates.setdefault(workspace_id, []).append(root)

    roots: dict[str, Path] = {}
    for workspace in workspaces:
        workspace_id = workspace.get("workspace_id")
        if not isinstance(workspace_id, str) or workspace_id not in candidates:
            continue
        label = normalized_text(str(workspace.get("label") or ""))
        counts = Counter(candidates[workspace_id])
        matching = [
            root
            for root in counts
            if label and normalized_text(root.name) == label
        ]
        roots[workspace_id] = matching[0] if matching else counts.most_common(1)[0][0]
    return roots


def project_signal(label: str, root: Path | None) -> tuple[str, set[str]]:
    parts = [label]
    names: set[str] = set()
    if root is None:
        return normalized_text(" ".join(parts)), names

    parts.append(root.name)
    try:
        entries = list(root.iterdir())[:256]
    except OSError:
        entries = []
    names = {entry.name.lower() for entry in entries}
    parts.extend(names)

    for filename in PROJECT_TEXT_FILES:
        path = root / filename
        try:
            if path.is_file():
                parts.append(
                    path.read_bytes()[:MAX_PROJECT_TEXT_BYTES].decode(
                        "utf-8", errors="ignore"
                    )
                )
        except OSError:
            continue
    return normalized_text(" ".join(parts)), names


def infer_project_icon(label: str, root: Path | None) -> str:
    signal, names = project_signal(label, root)
    label_signal = normalized_text(label)
    root_signal = normalized_text(root.name) if root is not None else ""
    best_icon: str | None = None
    best_score = 0
    for icon, keywords in SEMANTIC_ICON_RULES:
        score = 0
        for keyword in keywords:
            if not keyword_present(signal, keyword):
                continue
            word_count = len(normalized_text(keyword).split())
            score += word_count * 3 if word_count > 1 else 1
            if keyword_present(root_signal, keyword):
                score += 24
            if keyword_present(label_signal, keyword):
                score += 48
        if score > best_score:
            best_icon = icon
            best_score = score
    if best_icon is not None and best_score >= 5:
        return best_icon

    if any(name.startswith("wrangler.") for name in names):
        return ""
    if "package.swift" in names or any(name.endswith(".xcodeproj") for name in names):
        return ""
    if "gemfile" in names:
        return ""
    if "cargo.toml" in names:
        return ""
    if "go.mod" in names:
        return ""
    if "pyproject.toml" in names or "setup.py" in names:
        return ""
    if "package.json" in names:
        return ""
    if ".git" in names or {"head", "objects", "refs"}.issubset(names):
        return ""
    return FOLDER_ICON


def space_value(marker: str, label: str, icon: str | None) -> str:
    return f"{icon or marker}  {label}"


def clear_grouping_tokens(herdr: str, workspaces: list[dict[str, Any]]) -> None:
    for workspace in workspaces:
        tokens = workspace.get("tokens") or {}
        obsolete = [key for key in ("space_section", "space_kind") if key in tokens]
        if not obsolete:
            continue
        command = [herdr, "workspace", "report-metadata", workspace["workspace_id"],
                   "--source", "operator.space-section"]
        for key in obsolete:
            command.extend(["--clear-token", key])
        subprocess.run(command, check=True, capture_output=True, text=True)


def plan_updates(
    workspaces: list[dict[str, Any]],
    tracking: dict[str, dict[str, int | None]],
    now_ms: int,
    local_tz: tzinfo | None = None,
    project_roots: dict[str, Path] | None = None,
) -> list[tuple[str, str, str, str]]:
    if project_roots is None:
        project_roots = {}
    updates: list[tuple[str, str, str, str]] = []
    for workspace in workspaces:
        workspace_id = workspace.get("workspace_id")
        if not isinstance(workspace_id, str) or not workspace_id:
            continue
        tokens = workspace.get("tokens") or {}
        if not isinstance(tokens, dict):
            tokens = {}
        status = workspace.get("agent_status", "unknown")
        if status not in MARKERS:
            status = "unknown"
        desired_token, desired_marker = MARKERS[status]
        label = workspace.get("label")
        if not isinstance(label, str) or not label:
            label = workspace_id
        reported_icon = valid_agent_icon(tokens.get(AGENT_ICON_TOKEN))
        desired_value = space_value(
            desired_marker,
            label,
            reported_icon
            or infer_project_icon(label, project_roots.get(workspace_id)),
        )

        record = tracking[workspace_id]
        updated_at_ms = record["updated_at_ms"]
        if isinstance(updated_at_ms, int):
            desired_updated = format_timestamp(updated_at_ms, now_ms, local_tz)
        else:
            desired_updated = format_tracking(record["observed_at_ms"], local_tz)

        marker_matches = tokens.get(desired_token) == desired_value
        updated_matches = tokens.get(UPDATED_TOKEN) == desired_updated
        other_markers_clear = all(
            token == desired_token or token not in tokens for token in MARKER_TOKENS
        )
        old_markers_clear = all(token not in tokens for token in OLD_MARKER_TOKENS)
        if (
            not marker_matches
            or not updated_matches
            or not other_markers_clear
            or not old_markers_clear
            or LEGACY_TOKEN in tokens
        ):
            updates.append(
                (workspace_id, desired_token, desired_value, desired_updated)
            )
    return updates


def run_json(herdr: str, *args: str) -> dict[str, Any]:
    completed = subprocess.run(
        [herdr, *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(completed.stdout)


def reconcile(
    herdr: str,
    state_dir: Path,
    event_name: str | None,
    event: dict[str, Any],
    now_ms: int,
) -> None:
    snapshot = run_json(herdr, "api", "snapshot")["result"]["snapshot"]
    workspaces = snapshot["workspaces"]
    panes = snapshot["panes"]
    clear_grouping_tokens(herdr, workspaces)
    project_roots = workspace_project_roots(workspaces, panes)
    state_path = state_dir / STATE_FILENAME
    state = load_tracking_state(state_path)
    if sync_tracking_state(state, workspaces, event_name, event, now_ms):
        save_tracking_state(state_path, state)

    for workspace_id, desired_token, desired_marker, desired_updated in plan_updates(
        workspaces,
        state["workspaces"],
        now_ms,
        project_roots=project_roots,
    ):
        command = [
            herdr,
            "workspace",
            "report-metadata",
            workspace_id,
            "--source",
            SOURCE,
        ]
        for token in (LEGACY_TOKEN, *OLD_MARKER_TOKENS, *MARKER_TOKENS):
            if token != desired_token:
                command.extend(["--clear-token", token])
        command.extend(
            [
                "--token",
                f"{desired_token}={desired_marker}",
                "--token",
                f"{UPDATED_TOKEN}={desired_updated}",
            ]
        )
        subprocess.run(command, check=True, capture_output=True, text=True)


def event_from_environment() -> dict[str, Any]:
    raw_event = os.environ.get("HERDR_PLUGIN_EVENT_JSON")
    if not raw_event:
        return {}
    try:
        event = json.loads(raw_event)
    except json.JSONDecodeError:
        return {}
    return event if isinstance(event, dict) else {}


def main() -> None:
    herdr = os.environ.get("HERDR_BIN_PATH") or shutil.which("herdr")
    if not herdr:
        raise SystemExit("herdr executable not found")
    state_dir = Path(
        os.environ.get("HERDR_PLUGIN_STATE_DIR", "/tmp/herdr-space-attention")
    )
    state_dir.mkdir(parents=True, exist_ok=True)
    with (state_dir / "reconcile.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        reconcile(
            herdr,
            state_dir,
            os.environ.get("HERDR_PLUGIN_EVENT"),
            event_from_environment(),
            time.time_ns() // 1_000_000,
        )


if __name__ == "__main__":
    main()
