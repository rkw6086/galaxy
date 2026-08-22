# Upstream provenance

- Source: https://github.com/amazonbedrockmodels/amazonbedrockmodels.github.io
- Data directory: https://github.com/amazonbedrockmodels/amazonbedrockmodels.github.io/tree/main/data
- Snapshot commit: recorded in `data/UPSTREAM_COMMIT`
- Upstream license declaration: MIT, as stated in the upstream README

The upstream project is an unofficial, community-maintained catalog refreshed
from AWS Bedrock APIs. Its data is useful for discovery and enrichment, but AWS
documentation and live account/region responses remain authoritative for model
availability and access.

The JSON files in `data/` are preserved verbatim from upstream. Refresh them
with `./script/update_bedrock_model_catalog` rather than editing them manually.
