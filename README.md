# helm-rs

Render Helm charts in Rust, without Helm. `helm-rs` is a `#![no_std]` (+
`alloc`) implementation of Helm's template engine:

- **Go `text/template`**, ported from Go 1.27, including Go's reflection
  semantics (nil vs missing values, typed arguments, comparison rules).
- **All of Sprig v3.3.0** (strings, lists, dicts, regex with Go's RE2 syntax,
  semver, dates, crypto, key and certificate generation) and **Helm's own
  functions**: `include`, `tpl`, `required`, `fail`, `lookup`, `toYaml`,
  `fromYaml`, `toJson`, `toToml`, the duration helpers, ...
- **Charts**: loading from files or `.tgz`, `.helmignore`, subcharts,
  values coalescing, dependency conditions/tags/aliases/`import-values`,
  `values.schema.json` validation, `.Files`, `.Capabilities`, and output
  byte-identical to `helm template`.

It is **diff-tested against real Helm v4.3.0** (the latest release): 38
popular charts render identically, and randomized values and template fuzzing
compares every output and error message against Helm. See
[Testing against real Helm](#testing-against-real-helm).

## Errors that point at the problem

Every error carries the file, line, column and span of the failing
construct, the `include`/`template`/`tpl` call stack, and a hint. Helm
reports only `file:line` for syntax errors and a node offset for runtime
errors. For example (`cargo run --example diagnostics`):

```text
nil pointer evaluating interface {}.major
  --> mychart/templates/_helpers.tpl:2:52
   |
 2 | {{ .Values.image.repository }}:{{ .Values.image.tag.major }}
   |                                                    ^^^^^^
   = help: `.Values.image.tag` is nil or missing
   = note: from `include "mychart.image"` at mychart/templates/deployment.yaml:8:21
```

The fields are available programmatically ([`Error`]: `kind`, `message`,
`location`, `frames`, `help`), and `Error::helm_message()` returns the exact
message Helm would print, for tools that need compatibility.

## Usage

```toml
[dependencies]
helm-rs = { git = "https://github.com/xorps/helm-rs" }
```

Render a chart archive like `helm template` (from
[`examples/template.rs`](examples/template.rs)):

```rust
use helm_rs::chart::{self, Chart, RenderOptions};
use helm_rs::{DeterministicEnv, yaml};

let chart = Chart::from_tgz(&std::fs::read("mychart-1.0.0.tgz")?)?;

// User values, parsed exactly like `helm template -f values.yaml`.
let values = yaml::unmarshal(&std::fs::read_to_string("values.yaml")?, yaml::Target::Map("common.Values"))?;

// Defaults match `helm template`: release "release-name" in "default",
// Kubernetes v1.37.0, Helm v4.3.0 capabilities.
let opts = RenderOptions::default();

let manifest: String = chart::template(&chart, &values, &opts, &mut DeterministicEnv::default())?;
```

- `chart::render` returns the per-template output instead (`BTreeMap` of
  `mychart/templates/x.yaml` → rendered text), like Helm's `engine.Render`.
- `Chart::from_files` loads a chart from in-memory files (`Chart.yaml`,
  `templates/...`, `charts/...`), applying `.helmignore` like Helm's
  directory loader. There is no filesystem in `no_std`; the host supplies the
  bytes.
- `RenderOptions` sets the release name/namespace, upgrade vs install,
  capabilities (`kube_version`, `api_versions`), `strict` (missing keys are
  errors), `include_crds`, `skip_tests` and `skip_schema_validation`.

Render a single template string:

```rust
use helm_rs::template::Engine;
use helm_rs::{DeterministicEnv, map};

let data = map! { "Values" => map! { "name" => "world" } };
let out = Engine::new().render_str("greeting", "hello {{ .Values.name | upper }}", &data, &mut DeterministicEnv::default())?;
assert_eq!(out, "hello WORLD");
```

### The environment

`no_std` has no clock, randomness or network, so those come from an
[`Env`] you provide. `DeterministicEnv` (fixed clock at the Unix epoch, seeded
randomness) makes rendering reproducible. Implement `Env` to supply the real
time (`now`, `date`), randomness (`randAlphaNum`, `uuidv4`, `genCA`, ...),
cluster lookups (`lookup` returns an empty map by default, as in
`helm template`) and DNS (`getHostByName`, off by default as in Helm).

### Custom functions

```rust
use helm_rs::{Sig, Ty, Value};
use helm_rs::template::Engine;

let mut engine = Engine::new();
// Signatures are Go signatures: arguments are checked and converted the way
// Go's text/template does.
engine.funcs.add("shout", Sig::new(&[Ty::String]), |args| {
    Ok(Value::string(args[0].as_str().unwrap_or("").to_uppercase() + "!"))
});
```

### Features

| Feature | Default | |
| --- | --- | --- |
| `archive` | yes | `Chart::from_tgz` (gzip + tar, pure Rust) |
| `certs` | yes | RSA/ECDSA/Ed25519 key and x509 certificate generation for Sprig's `genPrivateKey`, `genCA`, `genSignedCert`, ... Without it those functions return an error. |

The crate builds for bare-metal targets (`cargo build --target
aarch64-unknown-none`). Minimum Rust version: 1.88. Development and CI use the
toolchain pinned in `rust-toolchain.toml` (1.99.0, with the bare-metal
targets); `cargo-fuzz` uses the nightly pinned in `.github/workflows/fuzz.yml`.

### Known differences from Helm

- Recursion is capped at `Engine::max_depth` (default 128) nested
  `include`/`template`/`tpl` calls instead of Helm's ~1000, to stay within the
  native stack. Exceeding it is a clean error.
- Named time zones other than `UTC` and `Local` fall back to UTC (there is
  no time zone database in `no_std`); `Local` is the `Env`'s offset.
- `index` on a string yields an `int` where Go yields `uint8` (only visible
  in error messages).
- Where Helm itself is nondeterministic (Go map iteration order, pointer
  addresses in printed values), results match one of Helm's possible outputs.

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
cargo install cargo-fuzz --version 0.13.2 --locked
rustup toolchain install nightly-2026-10-11 --profile minimal   # pinned in CI
cargo +nightly-2026-10-11 fuzz build
cargo +nightly-2026-10-11 fuzz run template_render      # parse + render arbitrary templates
cargo +nightly-2026-10-11 fuzz run yaml_from_str        # yaml::from_str on arbitrary bytes
mkdir -p fuzz/corpus/chart_from_tgz && cp testdata/charts/cache/*.tgz fuzz/corpus/chart_from_tgz/
cargo +nightly-2026-10-11 fuzz run chart_from_tgz       # Chart::from_tgz (+ render) on arbitrary bytes
```

These targets check that helm-rs never panics.
