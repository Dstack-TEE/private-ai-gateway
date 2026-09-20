# ACI Session Storage Measurement Scripts

Scripts behind `docs/aci-session-storage-study.md` — every number in that
report is reproducible with these. They were run against a corpus fetched
from the production deployment (`api.redpill.ai` `/v1/aci/sessions`); the
corpus itself is not committed (12MB).

Corpus layout expected by the scripts (re-fetch to reproduce):

```
corpus/
  sessions-list.json                 # GET /v1/aci/sessions (abbreviated list)
  series-phala225/<id>.json          # consecutive full records, one upstream
  series-near/<id>.json              # consecutive full records, near-ai
  cross/<upstream>.json              # one full record per upstream
```

Scripts:

| script | what it measures |
| --- | --- |
| `measure.py` | production churn rates, per-round increments, compression baselines |
| `deep_diff.py` | byte-range diffs inside quote / nvidia evidence / certs (L2/L3 churn) |
| `cross_diff.py` | per-field uniqueness across upstreams (field lifecycle classes) |
| `deep_cross.py` | app_compose / app_cert / event_log diffs across same-model nodes |
| `near_series.py` | instance rotation pattern; per-field zstd patch sizes |
| `project.py` | 90-day projections per storage layout |
| `store_sim3.py` | merkle-CAS simulation with byte-exact roundtrip assertions (1-day retention comparison) |

Note: `store_sim3.py` simulates the shipped layout; the implementation
is `src/aggregator/session_cas.rs` + `src/aggregator/session_store.rs`, whose
unit tests (`tests/fixtures/sessions/`, real production documents) assert the
same byte-exact roundtrip property.
