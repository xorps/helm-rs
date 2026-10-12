# helm-rs

## Testing against real Helm

helm-rs is diff-tested against the real Helm, pinned to **v4.3.0**, in two
forms: the Go oracle (`oracle/`, Helm linked as a library, JSON-lines protocol)
and the official `helm` release binary.

### Setup

```sh
ASDF_GOLANG_VERSION=1.27.1 scripts/build-oracle.sh   # -> target/helm-oracle
scripts/fetch-helm.sh                                # -> .tools/helm-v4.3.0/helm (sha256-verified)
scripts/fetch-charts.sh                              # -> testdata/charts/cache/*.tgz (sha256-verified)
```

The chart corpus (~40 popular charts: ingress-nginx, cert-manager,
kube-prometheus-stack, grafana, loki, argo-cd, cilium, istio, karpenter,
Bitnami charts, ...) is listed in `testdata/charts/sources.txt` and pinned in
`testdata/charts/charts.lock` (version, sha256, whether `helm template`
renders it). Charts that need values to render use
`testdata/charts/values/<name>.yaml`. `scripts/update-charts.sh` re-resolves
the latest versions and rewrites the lock (`--pinned` keeps the versions and
only refreshes the render status, `CHARTS=a,b` limits the update).

Tests skip with a message when the oracle, the binary or the charts are
missing; set `HELM_ORACLE_REQUIRED=1` / `HELM_RS_CHARTS_REQUIRED=1` to make
that an error (CI).

### Test suites

| Test | What |
| --- | --- |
| `cargo test --test charts_oracle` | Validates the references: lock/cache checksums; the oracle's `helm template` reproduction is byte-identical to the binary (output and errors); the oracle's per-template output is consistent with the binary's. |
| `cargo test --test charts_diff` | `Chart::from_tgz` + `chart::render` vs the oracle per template, and `chart::template` vs the `helm template` binary, for every corpus chart. Errors must match (message and innermost `file:line:col`). Prints a unified diff of the first differing file and the values used. |
| `cargo test --test charts_fuzz` | Values fuzzing: mutated value sets (bool flips, feature toggles, tricky strings/numbers, YAML 1.1 scalars, deletions/nulls, unknown keys, type swaps, `image.tag`, `ingress`, `persistence`, `autoscaling`, `serviceMonitor`, `global.*`, ...), rendered by both; failures are minimized. |
| `cargo test --test template_fuzz` | Template fuzzing: grammar-generated Go templates (all actions, control structures, trim markers, literals, Sprig/Helm calls with random arity/types) and mutated corpus templates; same output or same error and location. |
| `cargo test --test harness_selftest` | Unit tests for the diff/location/RNG helpers. |

Knobs (all optional):

- `HELM_RS_CHARTS=ingress-nginx,bitnami-redis` — chart subset.
- `HELM_RS_FUZZ_SEED=<u64>` — reproduce a fuzz run (the seed is always printed).
- `HELM_RS_FUZZ_ITERS=<n>` — cases per chart (values fuzz, default 16) or per
  source (template fuzz, default 400). E.g. `HELM_RS_FUZZ_ITERS=500` for a deep run.
- `HELM_RS_FUZZ_THREADS=<n>` — parallel oracle processes.
- `HELM_RS_LAX_ERRORS=1` — when both fail, don't compare messages/locations.
- `HELM_ORACLE=<path>`, `HELM_BIN=<path>` — alternative oracle / helm binary.

Lines that differ between two Helm renders of the same input (random
passwords, generated certificates) are excluded from comparisons; `now` is
fixed to the Unix epoch in the oracle, as in `DeterministicEnv`.

### Coverage-guided fuzzing (cargo-fuzz)

```sh
cargo install cargo-fuzz
cargo +nightly fuzz build
cargo +nightly fuzz run template_render      # parse + render arbitrary templates
cargo +nightly fuzz run yaml_from_str        # yaml::from_str on arbitrary bytes
mkdir -p fuzz/corpus/chart_from_tgz && cp testdata/charts/cache/*.tgz fuzz/corpus/chart_from_tgz/
cargo +nightly fuzz run chart_from_tgz       # Chart::from_tgz (+ render) on arbitrary bytes
```

These targets check that helm-rs never panics.
