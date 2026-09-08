import json
import subprocess
import sys
from pathlib import Path

from remote import health


ROOT = Path(__file__).resolve().parents[1]


def runner(fixtures):
    def run(argv, timeout):
        del timeout
        value = fixtures.get(tuple(argv))
        if value is None:
            return health.CommandResult(returncode=127, stdout="", stderr="", missing=True)
        if isinstance(value, Exception):
            raise value
        return value

    return run


def test_ready_requires_codex_chatgpt_and_claude_first_party_subscription_auth():
    report = health.evaluate_health(
        environ={},
        command_runner=runner(
            {
                ("codex", "login", "status"): health.CommandResult(
                    returncode=0,
                    stdout="",
                    stderr="Logged in using ChatGPT\n",
                ),
                ("claude", "auth", "status"): health.CommandResult(
                    returncode=0,
                    stdout=json.dumps(
                        {
                            "loggedIn": True,
                            "authMethod": "claude.ai",
                            "apiProvider": "firstParty",
                            "email": "person@example.com",
                            "orgId": "secret-org",
                            "subscriptionType": "max",
                        }
                    ),
                    stderr="",
                ),
            }
        ),
    )

    assert report["status"] == "ready"
    assert report["ready"] is True
    assert report["auth"]["codex"]["chatgpt_subscription"] is True
    assert report["auth"]["claude"]["subscription_auth"] is True
    rendered = json.dumps(report)
    assert "person@example.com" not in rendered
    assert "secret-org" not in rendered


def test_blocks_stock_not_logged_in_cli_statuses():
    report = health.evaluate_health(
        environ={},
        command_runner=runner(
            {
                ("codex", "login", "status"): health.CommandResult(
                    returncode=1,
                    stdout="",
                    stderr="Not logged in\n",
                ),
                ("claude", "auth", "status"): health.CommandResult(
                    returncode=0,
                    stdout=json.dumps({"loggedIn": False}),
                    stderr="",
                ),
            }
        ),
    )

    assert report["status"] == "auth_required"
    assert report["ok"] is False
    assert "codex_chatgpt_auth_required" in report["blockers"]
    assert "claude_subscription_auth_required" in report["blockers"]


def test_claude_unknown_auth_method_fails_closed():
    report = health.evaluate_health(
        environ={},
        command_runner=runner(
            {
                ("codex", "login", "status"): health.CommandResult(
                    returncode=0,
                    stdout="Logged in using ChatGPT\n",
                    stderr="",
                ),
                ("claude", "auth", "status"): health.CommandResult(
                    returncode=0,
                    stdout=json.dumps(
                        {
                            "loggedIn": True,
                            "authMethod": "console-api-key",
                            "apiProvider": "firstParty",
                        }
                    ),
                    stderr="",
                ),
            }
        ),
    )

    assert report["auth"]["claude"]["logged_in"] is True
    assert report["auth"]["claude"]["allowed_auth_method"] is False
    assert report["auth"]["claude"]["subscription_auth"] is False
    assert "claude_subscription_auth_required" in report["blockers"]


def test_metered_api_override_presence_is_boolean_only_and_blocks_ready():
    report = health.evaluate_health(
        environ={
            "OPENAI_API_KEY": "sk-secret",
            "ANTHROPIC_API_KEY": "",
            "ANTHROPIC_AUTH_TOKEN": "anthropic-secret",
            "OPENAI_BASE_URL": "",
            "ANTHROPIC_BASE_URL": "https://example.invalid",
            "CLAUDE_CODE_OAUTH_TOKEN": "oauth-secret",
        },
        command_runner=runner(
            {
                ("codex", "login", "status"): health.CommandResult(
                    returncode=0,
                    stdout="Logged in using ChatGPT\n",
                    stderr="",
                ),
                ("claude", "auth", "status"): health.CommandResult(
                    returncode=0,
                    stdout=json.dumps(
                        {
                            "loggedIn": True,
                            "authMethod": "oauth",
                            "apiProvider": "firstParty",
                        }
                    ),
                    stderr="",
                ),
            }
        ),
    )

    metered = report["environment"]["metered"]
    assert metered["OPENAI_API_KEY"] is True
    assert metered["ANTHROPIC_API_KEY"] is False
    assert metered["ANTHROPIC_AUTH_TOKEN"] is True
    assert metered["OPENAI_BASE_URL"] is False
    assert metered["ANTHROPIC_BASE_URL"] is True
    assert report["environment"]["subscription_tokens"]["CLAUDE_CODE_OAUTH_TOKEN"] is True
    assert report["automatic_api_fallback_allowed"] is False
    assert report["status"] == "auth_required"
    assert "unexpected_api_overrides_present" in report["blockers"]
    rendered = json.dumps(report)
    assert "sk-secret" not in rendered
    assert "anthropic-secret" not in rendered
    assert "oauth-secret" not in rendered
    assert "example.invalid" not in rendered


def test_timeout_and_invalid_json_are_blocked_states():
    report = health.evaluate_health(
        environ={},
        command_runner=runner(
            {
                ("codex", "login", "status"): health.CommandResult(
                    returncode=124,
                    stdout="",
                    stderr="",
                    timed_out=True,
                ),
                ("claude", "auth", "status"): health.CommandResult(
                    returncode=0,
                    stdout="not json",
                    stderr="",
                ),
            }
        ),
    )

    assert report["auth"]["codex"]["status"] == "timeout"
    assert report["auth"]["claude"]["status"] == "invalid_json"
    assert report["status"] == "auth_required"


def test_cli_exits_zero_for_structured_auth_required_state():
    completed = subprocess.run(
        [sys.executable, "-m", "remote.health", "--timeout", "0.01"],
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    report = json.loads(completed.stdout)
    assert report["status"] in {"ready", "auth_required"}
    assert "auth" in report
