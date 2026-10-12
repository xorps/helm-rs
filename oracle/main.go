// Command helm-oracle renders charts and ad-hoc templates with the real Helm
// engine (pinned in go.mod) so the Rust implementation can be diffed against it.
//
// Protocol: one JSON request per line on stdin, one JSON response per line on
// stdout. See Request/Response below.
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"os"
	"sort"
	"strings"
	"text/template"
	"time"

	"helm.sh/helm/v4/pkg/action"
	ci "helm.sh/helm/v4/pkg/chart"
	"helm.sh/helm/v4/pkg/chart/common"
	"helm.sh/helm/v4/pkg/chart/common/util"
	genericloader "helm.sh/helm/v4/pkg/chart/loader"
	"helm.sh/helm/v4/pkg/chart/loader/archive"
	chart "helm.sh/helm/v4/pkg/chart/v2"
	"helm.sh/helm/v4/pkg/chart/v2/loader"
	chartutil "helm.sh/helm/v4/pkg/chart/v2/util"
	"helm.sh/helm/v4/pkg/engine"
	release "helm.sh/helm/v4/pkg/release/v1"
)

type Request struct {
	// "chart": load the chart at Path (directory or .tgz).
	// "files": load a chart from the in-memory Files map (path -> content).
	// "version": report versions.
	// "template": like "chart"/"files" (Path or Files), but returns exactly
	//   what `helm template release-name CHART -f values` prints to stdout
	//   (Output), or the error the CLI prints after "Error: ".
	// "batch": runs every request in Requests, returning Results in order.
	// "values": returns the coalesced default values of the chart at Path
	//   (plus user Values) as JSON, for building fuzzed value sets. Without
	//   Path: just Values parsed by Helm (common.ReadValues), as JSON.
	Op    string            `json:"op"`
	Path  string            `json:"path,omitempty"`
	Files map[string]string `json:"files,omitempty"`
	// User supplied values (YAML), equivalent to `helm template -f`.
	Values      string   `json:"values,omitempty"`
	Release     string   `json:"release,omitempty"`
	Namespace   string   `json:"namespace,omitempty"`
	KubeVersion string   `json:"kube_version,omitempty"`
	APIVersions []string `json:"api_versions,omitempty"`
	IsUpgrade   bool     `json:"is_upgrade,omitempty"`
	Strict      bool     `json:"strict,omitempty"`
	// Skip values.schema.json validation.
	SkipSchema bool `json:"skip_schema,omitempty"`
	// "template" only: `helm template --include-crds`.
	IncludeCRDs bool `json:"include_crds,omitempty"`
	// Make `now` return the Unix epoch in UTC (helm-rs' DeterministicEnv
	// default) so outputs using it are reproducible.
	FixedTime bool `json:"fixed_time,omitempty"`
	// "batch" only.
	Requests []Request `json:"requests,omitempty"`
}

type Response struct {
	Files map[string]string `json:"files,omitempty"`
	Error *string           `json:"error,omitempty"`
	// Stage at which the error happened: load, values, dependencies, render.
	Stage string            `json:"stage,omitempty"`
	Info  map[string]string `json:"info,omitempty"`
	// "template" only: `helm template` stdout.
	Output *string `json:"output,omitempty"`
	// "batch" only.
	Results []Response `json:"results,omitempty"`
	// "values" only: the chart's default values coalesced with Values and
	// with every subchart's defaults (chartutil.CoalesceValues), as JSON.
	JSON any `json:"json,omitempty"`
}

func main() {
	slog.SetDefault(slog.New(slog.NewTextHandler(io.Discard, nil)))
	in := bufio.NewReaderSize(os.Stdin, 1<<20)
	out := bufio.NewWriter(os.Stdout)
	enc := json.NewEncoder(out)
	enc.SetEscapeHTML(false)
	for {
		line, err := in.ReadBytes('\n')
		if len(strings.TrimSpace(string(line))) > 0 {
			var req Request
			var resp Response
			if jerr := json.Unmarshal(line, &req); jerr != nil {
				resp = fail("request", jerr)
			} else {
				resp = handle(req)
			}
			_ = enc.Encode(resp)
			_ = out.Flush()
		}
		if err != nil {
			return
		}
	}
}

func fail(stage string, err error) Response {
	s := err.Error()
	return Response{Error: &s, Stage: stage}
}

func handle(req Request) (resp Response) {
	defer func() {
		if r := recover(); r != nil {
			resp = fail("panic", fmt.Errorf("%v", r))
		}
	}()
	switch req.Op {
	case "version":
		return Response{Info: map[string]string{
			"kube_version": common.DefaultCapabilities.KubeVersion.Version,
			"helm_version": common.DefaultCapabilities.HelmVersion.Version,
			"api_versions": strings.Join(common.DefaultCapabilities.APIVersions, ","),
		}}
	case "chart":
		c, err := loader.Load(req.Path)
		if err != nil {
			return fail("load", err)
		}
		return render(c, req)
	case "files":
		c, err := loadFiles(req.Files)
		if err != nil {
			return fail("load", err)
		}
		return render(c, req)
	case "template":
		return helmTemplate(req)
	case "values":
		vals, err := common.ReadValues([]byte(req.Values))
		if err != nil {
			return fail("values", err)
		}
		if req.Path == "" {
			return Response{JSON: map[string]any(vals)}
		}
		c, err := loader.Load(req.Path)
		if err != nil {
			return fail("load", err)
		}
		v, err := util.CoalesceValues(c, vals)
		if err != nil {
			return fail("values", err)
		}
		return Response{JSON: map[string]any(v)}
	case "batch":
		results := make([]Response, len(req.Requests))
		for i, r := range req.Requests {
			if r.Op == "batch" {
				results[i] = fail("request", fmt.Errorf("nested batch"))
				continue
			}
			results[i] = handle(r)
		}
		return Response{Results: results}
	default:
		return fail("request", fmt.Errorf("unknown op %q", req.Op))
	}
}

// render mirrors what `helm template` does (pkg/action/install.go) up to the
// engine call, and returns the per-template output of the engine.
func render(c *chart.Chart, req Request) Response {
	vals, err := common.ReadValues([]byte(req.Values))
	if err != nil {
		return fail("values", err)
	}
	if err := chartutil.ProcessDependencies(c, vals); err != nil {
		return fail("dependencies", err)
	}
	caps := common.DefaultCapabilities.Copy()
	if req.KubeVersion != "" {
		kv, err := common.ParseKubeVersion(req.KubeVersion)
		if err != nil {
			return fail("request", err)
		}
		caps.KubeVersion = *kv
	}
	caps.APIVersions = append(caps.APIVersions, req.APIVersions...)
	name := req.Release
	if name == "" {
		name = "release-name"
	}
	ns := req.Namespace
	if ns == "" {
		ns = "default"
	}
	opts := common.ReleaseOptions{
		Name:      name,
		Namespace: ns,
		Revision:  1,
		IsInstall: !req.IsUpgrade,
		IsUpgrade: req.IsUpgrade,
	}
	top, err := util.ToRenderValuesWithSchemaValidation(c, vals, opts, caps, req.SkipSchema)
	if err != nil {
		return fail("values", err)
	}
	if c.Metadata.KubeVersion != "" && !chartutil.IsCompatibleRange(c.Metadata.KubeVersion, caps.KubeVersion.String()) {
		return fail("render", fmt.Errorf("chart requires kubeVersion: %s which is incompatible with Kubernetes %s", c.Metadata.KubeVersion, caps.KubeVersion.Version))
	}
	e := engine.Engine{Strict: req.Strict, CustomTemplateFuncs: customFuncs(req)}
	files, err := e.Render(c, top)
	if err != nil {
		return fail("render", err)
	}
	return Response{Files: files}
}

func loadFiles(m map[string]string) (*chart.Chart, error) {
	names := make([]string, 0, len(m))
	for n := range m {
		names = append(names, n)
	}
	sort.Strings(names)
	var files []*archive.BufferedFile
	for _, n := range names {
		files = append(files, &archive.BufferedFile{Name: n, ModTime: time.Unix(0, 0), Data: []byte(m[n])})
	}
	return loader.LoadFiles(files)
}

func customFuncs(req Request) template.FuncMap {
	if !req.FixedTime {
		return nil
	}
	return template.FuncMap{"now": func() time.Time { return time.Unix(0, 0).UTC() }}
}

// helmTemplate reproduces `helm template` (pkg/cmd/template.go and runInstall
// in pkg/cmd/install.go) for a local chart, with only the flags in Request.
// The error message is what the CLI prints after "Error: ".
func helmTemplate(req Request) Response {
	var chrt ci.Charter
	var err error
	if req.Files != nil {
		chrt, err = loadFiles(req.Files)
	} else {
		chrt, err = genericloader.Load(req.Path)
	}
	if err != nil {
		return fail("load", err)
	}
	// -f values (pkg/cli/values.Options.MergeValues).
	vals := map[string]any{}
	if req.Values != "" {
		cur, err := loader.LoadValues(bytes.NewReader([]byte(req.Values)))
		if err != nil {
			return fail("values", fmt.Errorf("failed to parse values.yaml: %w", err))
		}
		vals = loader.MergeMaps(vals, cur)
	}
	ac, err := ci.NewAccessor(chrt)
	if err != nil {
		return fail("load", err)
	}
	switch t := ac.MetadataAsMap()["Type"]; t {
	case "", "application", nil:
	default:
		return fail("load", fmt.Errorf("%s charts are not installable", t))
	}
	if reqs := ac.MetaDependencies(); len(reqs) > 0 {
		if err := action.CheckDependencies(chrt, reqs); err != nil {
			return fail("dependencies", fmt.Errorf("an error occurred while checking for chart dependencies. You may need to run 'helm dependency build' to fetch missing dependencies: %w", err))
		}
	}
	cfg := action.NewConfiguration(action.ConfigurationSetLogger(slog.NewTextHandler(io.Discard, nil)))
	cfg.CustomTemplateFuncs = customFuncs(req)
	client := action.NewInstall(cfg)
	client.DryRunStrategy = action.DryRunClient
	client.ReleaseName = "release-name"
	if req.Release != "" {
		client.ReleaseName = req.Release
	}
	client.Replace = true
	client.Namespace = "default"
	if req.Namespace != "" {
		client.Namespace = req.Namespace
	}
	client.APIVersions = common.VersionSet(req.APIVersions)
	client.IncludeCRDs = req.IncludeCRDs
	client.IsUpgrade = req.IsUpgrade
	client.SkipSchemaValidation = req.SkipSchema
	if req.KubeVersion != "" {
		kv, err := common.ParseKubeVersion(req.KubeVersion)
		if err != nil {
			return fail("request", fmt.Errorf("invalid kube version '%s': %w", req.KubeVersion, err))
		}
		client.KubeVersion = kv
	}
	ri, err := client.Run(chrt, vals)
	var rel *release.Release
	switch r := ri.(type) {
	case *release.Release:
		rel = r
	case release.Release:
		rel = &r
	}
	if err != nil {
		if rel != nil {
			return fail("render", fmt.Errorf("%w\n\nUse --debug flag to render out invalid YAML", err))
		}
		return fail("render", err)
	}
	var out bytes.Buffer
	fmt.Fprintln(&out, strings.TrimSpace(rel.Manifest))
	for _, m := range rel.Hooks {
		fmt.Fprintf(&out, "---\n# Source: %s\n%s\n", m.Path, m.Manifest)
	}
	s := out.String()
	return Response{Output: &s}
}
