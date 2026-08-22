# Galaxy Bedrock Model Catalog

This crate embeds an offline snapshot of the public Amazon Bedrock Models
Catalog. It intentionally exposes the upstream JSON verbatim so Galaxy can
adopt newly added fields without changing the snapshot layer first.

Refresh the snapshot from the workspace root:

```sh
./script/update_bedrock_model_catalog
```

The updater resolves the current upstream `main` commit, downloads all catalog
documents at that immutable commit, validates their JSON shapes, and updates
`data/UPSTREAM_COMMIT`.

Normal Cargo builds never require network access. The checked-in snapshot is
embedded with `include_str!` and rebuilt whenever one of the data files changes.

See [UPSTREAM.md](UPSTREAM.md) for provenance and licensing notes.
