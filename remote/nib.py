"""Launch Nib with its runtime credential outside the Nix store."""

from __future__ import annotations

import os
import stat
import sys
from pathlib import Path


def credential_environment(path: Path, inherited: dict[str, str]) -> dict[str, str]:
    environment = dict(inherited)
    if environment.get("NIB_AUTH_TOKEN", "").strip():
        return environment
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return environment
    with os.fdopen(fd, "r") as handle:
        metadata = os.fstat(handle.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
            raise ValueError("Nib credential must be an owner-only regular file")
        token = handle.read(16385).strip()
    if not token or len(token) > 16384 or any(character.isspace() for character in token):
        raise ValueError("Nib credential file has an invalid format")
    environment["NIB_AUTH_TOKEN"] = token
    return environment


def main() -> None:
    if len(sys.argv) < 2:
        raise ValueError("a real Nib executable path is required")
    executable = sys.argv[1]
    environment = credential_environment(Path.home() / ".config/workenv/nib-token", dict(os.environ))
    os.execve(executable, [executable, *sys.argv[2:]], environment)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        print(f"nib: {error}", file=sys.stderr)
        raise SystemExit(1)
