package main

// Chart-level operations used by the Rust `chart` module tests: loading
// (directory semantics incl. .helmignore, archives), values coalescing and
// dependency processing, `helm template` output, manifest sorting, schema
// validation, .Files.Glob and kube version parsing.

import (
	"bytes"
	"context"
	"encoding/base64"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"

	"github.com/gobwas/glob"
	"sigs.k8s.io/yaml"

	"helm.sh/helm/v4/pkg/action"
	ci "helm.sh/helm/v4/pkg/chart"
	"helm.sh/helm/v4/pkg/chart/common"
	"helm.sh/helm/v4/pkg/chart/common/util"
	chart "helm.sh/helm/v4/pkg/chart/v2"
	"helm.sh/helm/v4/pkg/chart/v2/loader"
	chartutil "helm.sh/helm/v4/pkg/chart/v2/util"
	release "helm.sh/helm/v4/pkg/release/v1"
	releaseutil "helm.sh/helm/v4/pkg/release/v1/util"
)

func handleChartOp(req Request) (Response, bool) {
	switch req.Op {
	case "load":
		c, err := loadReq(req)
		if err != nil {
			return fail("load", err), true
		}
		return jsonResp(chartTree(c, true)), true
	case "load_archive":
		data, err := base64.StdEncoding.DecodeString(req.FilesB64["archive"])
		if err != nil {
			return fail("request", err), true
		}
		c, err := loader.LoadArchive(bytes.NewReader(data))
		if err != nil {
			return fail("load", err), true
		}
		return jsonResp(chartTree(c, true)), true
	case "coalesce":
		c, err := loadReq(req)
		if err != nil {
			return fail("load", err), true
		}
		vals, err := common.ReadValues([]byte(req.Values))
		if err != nil {
			return fail("values", err), true
		}
		out, err := util.CoalesceValues(c, vals)
		if err != nil {
			return fail("values", err), true
		}
		return jsonResp(out), true
	case "chart_values":
		c, err := loadReq(req)
		if err != nil {
			return fail("load", err), true
		}
		vals, err := common.ReadValues([]byte(req.Values))
		if err != nil {
			return fail("values", err), true
		}
		if err := chartutil.ProcessDependencies(c, vals); err != nil {
			return fail("dependencies", err), true
		}
		top, err := util.ToRenderValuesWithSchemaValidation(c, vals, common.ReleaseOptions{Name: "release-name", Namespace: "default", Revision: 1, IsInstall: true}, nil, req.SkipSchema)
		if err != nil {
			return fail("values", err), true
		}
		return jsonResp(map[string]any{"values": top["Values"], "chart": top["Chart"], "tree": chartTree(c, false)}), true
	case "chart_template":
		out, err := runTemplate(req)
		resp := Response{Output: &out}
		if err != nil {
			s := err.Error()
			resp.Error = &s
			resp.Stage = "template"
		}
		return resp, true
	case "sort_manifests":
		out, err := sortManifests(req.Rendered)
		resp := Response{Output: &out}
		if err != nil {
			s := err.Error()
			resp.Error = &s
			resp.Stage = "sort"
		}
		return resp, true
	case "schema":
		vals, err := common.ReadValues([]byte(req.Values))
		if err != nil {
			return fail("values", err), true
		}
		if err := util.ValidateAgainstSingleSchema(vals, []byte(req.Schema)); err != nil {
			return fail("schema", err), true
		}
		return Response{}, true
	case "glob":
		g, err := glob.Compile(req.Pattern, '/')
		if err != nil {
			g, _ = glob.Compile("**")
		}
		var out []string
		for _, n := range req.Names {
			if g.Match(n) {
				out = append(out, n)
			}
		}
		return jsonResp(out), true
	case "kube_version":
		kv, err := common.ParseKubeVersion(req.KubeVersion)
		if err != nil {
			return fail("request", err), true
		}
		return jsonResp(map[string]string{"version": kv.Version, "string": kv.String(), "major": kv.Major, "minor": kv.Minor, "git": kv.GitVersion()}), true
	}
	return Response{}, false
}

func jsonResp(v any) Response {
	return Response{JSON: v}
}

// writeDir materializes the request files into a temporary chart directory.
func writeDir(req Request) (string, error) {
	dir, err := os.MkdirTemp("", "helm-oracle-chart-")
	if err != nil {
		return "", err
	}
	write := func(name string, data []byte) error {
		p := filepath.Join(dir, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			return err
		}
		return os.WriteFile(p, data, 0o644)
	}
	for n, d := range req.Files {
		if err := write(n, []byte(d)); err != nil {
			return dir, err
		}
	}
	for n, d := range req.FilesB64 {
		b, err := base64.StdEncoding.DecodeString(d)
		if err != nil {
			return dir, err
		}
		if err := write(n, b); err != nil {
			return dir, err
		}
	}
	return dir, nil
}

func loadReq(req Request) (*chart.Chart, error) {
	if req.Path != "" {
		return loader.Load(req.Path)
	}
	dir, err := writeDir(req)
	defer os.RemoveAll(dir)
	if err != nil {
		return nil, err
	}
	return loader.Load(dir)
}

func fileNames(fs []*common.File) []string {
	out := []string{}
	for _, f := range fs {
		out = append(out, f.Name)
	}
	return out
}

func chartTree(c *chart.Chart, sortDeps bool) map[string]any {
	acc, _ := ci.NewAccessor(c)
	var schema any
	if c.Schema != nil {
		schema = string(c.Schema)
	}
	var lock any
	if c.Lock != nil {
		lock = map[string]any{"digest": c.Lock.Digest, "dependencies": c.Lock.Dependencies}
	}
	files := map[string]string{}
	for _, f := range c.Files {
		files[f.Name] = string(f.Data)
	}
	templates := map[string]string{}
	for _, f := range c.Templates {
		templates[f.Name] = string(f.Data)
	}
	deps := []any{}
	children := slices.Clone(c.Dependencies())
	if sortDeps {
		sort.SliceStable(children, func(i, j int) bool { return children[i].Name() < children[j].Name() })
	}
	for _, d := range children {
		deps = append(deps, chartTree(d, sortDeps))
	}
	crds := []string{}
	for _, crd := range c.CRDObjects() {
		crds = append(crds, crd.Filename)
	}
	return map[string]any{
		"name":           c.Name(),
		"full_path":      c.ChartFullPath(),
		"metadata":       acc.MetadataAsMap(),
		"values":         c.Values,
		"templates":      fileNames(c.Templates),
		"template_data":  templates,
		"files":          fileNames(c.Files),
		"file_data":      files,
		"raw":            fileNames(c.Raw),
		"schema":         schema,
		"lock":           lock,
		"deps":           deps,
		"crds":           crds,
		"is_library":     acc.IsLibraryChart(),
		"metadata_nil_deps": c.Metadata.Dependencies == nil,
	}
}

// runTemplate mirrors `helm template release-name CHART -f values.yaml`
// (pkg/cmd/template.go + runInstall), printing what helm prints to stdout.
func runTemplate(req Request) (string, error) {
	var c *chart.Chart
	var err error
	if req.Path != "" {
		c, err = loader.Load(req.Path)
	} else {
		var dir string
		dir, err = writeDir(req)
		defer os.RemoveAll(dir)
		if err == nil {
			c, err = loader.Load(dir)
		}
	}
	if err != nil {
		return "", err
	}
	// valueOpts.MergeValues for a single -f file.
	vals := map[string]any{}
	if req.Values != "" {
		cur := map[string]any{}
		if err := yaml.Unmarshal([]byte(req.Values), &cur); err != nil {
			return "", fmt.Errorf("failed to parse values.yaml: %w", err)
		}
		vals = loader.MergeMaps(vals, cur)
	}
	ac, _ := ci.NewAccessor(c)
	meta := ac.MetadataAsMap()
	switch meta["Type"] {
	case "", "application":
	default:
		return "", fmt.Errorf("%s charts are not installable", meta["Type"])
	}
	if reqs := ac.MetaDependencies(); len(reqs) > 0 {
		if err := action.CheckDependencies(c, reqs); err != nil {
			return "", fmt.Errorf("an error occurred while checking for chart dependencies. You may need to run 'helm dependency build' to fetch missing dependencies: %w", err)
		}
	}
	client := action.NewInstall(action.NewConfiguration())
	if req.KubeVersion != "" {
		kv, err := common.ParseKubeVersion(req.KubeVersion)
		if err != nil {
			return "", fmt.Errorf("invalid kube version '%s': %w", req.KubeVersion, err)
		}
		client.KubeVersion = kv
	}
	client.DryRunStrategy = action.DryRunClient
	client.ReleaseName = req.Release
	if client.ReleaseName == "" {
		client.ReleaseName = "release-name"
	}
	client.Namespace = req.Namespace
	if client.Namespace == "" {
		client.Namespace = "default"
	}
	client.Replace = true
	client.APIVersions = common.VersionSet(req.APIVersions)
	client.IncludeCRDs = req.IncludeCRDs
	client.IsUpgrade = req.IsUpgrade
	client.SkipSchemaValidation = req.SkipSchema
	ri, err := client.RunWithContext(context.Background(), c, vals)
	var rel *release.Release
	if ri != nil {
		rel, _ = ri.(*release.Release)
	}
	if err != nil {
		if rel != nil {
			return "", fmt.Errorf("%w\n\nUse --debug flag to render out invalid YAML", err)
		}
		return "", err
	}
	var manifests bytes.Buffer
	fmt.Fprintln(&manifests, strings.TrimSpace(rel.Manifest))
	for _, m := range rel.Hooks {
		if req.SkipTests && slices.Contains(m.Events, release.HookTest) {
			continue
		}
		fmt.Fprintf(&manifests, "---\n# Source: %s\n%s\n", m.Path, m.Manifest)
	}
	return manifests.String(), nil
}

// sortManifests mirrors the part of renderResources after rendering
// (NOTES removal excluded) plus the printing in `helm template`.
func sortManifests(files map[string]string) (string, error) {
	hs, manifests, err := releaseutil.SortManifests(files, nil, releaseutil.InstallOrder)
	if err != nil {
		return "", err
	}
	var b bytes.Buffer
	for _, m := range manifests {
		fmt.Fprintf(&b, "---\n# Source: %s\n%s\n", m.Name, m.Content)
	}
	var out bytes.Buffer
	fmt.Fprintln(&out, strings.TrimSpace(b.String()))
	for _, h := range hs {
		fmt.Fprintf(&out, "---\n# Source: %s\n%s\n", h.Path, h.Manifest)
	}
	return out.String(), nil
}
