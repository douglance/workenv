# Space Attention

This plugin gives each workspace one status-colored project icon and name. It
infers a Nerd Font glyph from pane working directories, project manifests,
bounded README content, and technology markers. An agent can override its icon
with `herdr-space-icon`, or clear the override to return to automatic inference.

Workspace, pane, worktree, and agent-state events refresh the labels. The plugin
runs on each Herdr server; the multi-machine client displays those labels under
Herdr's native machine headings. It does not add folder categories or tree
markers, reorder workspaces, or redirect focus.

Older folder-label metadata is cleared during reconciliation. The local client
layout in `config.toml` renders the colored `space_*` label tokens on one row.
