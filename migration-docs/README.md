# Migration Docs — Galaxy Project

This folder contains everything needed for a new Claude instance to pick up exactly where the previous session left off.

## How to Use

Give all files in this folder to the new Claude instance at the start of conversation. Say something like:

> "Read all files in `migration-docs/` — this is a handoff from a previous Claude session. Pick up where it left off."

## Contents

| File | Purpose |
|------|---------|
| `README.md` | This file — instructions for handoff |
| `conversation-transcript.md` | Full transcript of the 2026-07-08/09 session |
| `session-history-may.md` | Prior session log (May 13-14, 2026) — signing pipeline creation |
| `project-architecture.md` | Codebase architecture reference (56-crate Rust workspace) |
| `current-state.md` | Exact state of repo, uncommitted changes, pending work |
| `credentials-and-signing.md` | Apple signing credentials, certificate info, how the build pipeline works |
| `user-preferences.md` | How George works, what he expects, communication style |
