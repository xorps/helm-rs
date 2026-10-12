// Command helm-oracle renders charts and ad-hoc templates with the real Helm
// engine (pinned in go.mod) so the Rust implementation can be diffed against it.
//
// Protocol: one JSON request per line on stdin, one JSON response per line on
// stdout. See Request/Response below.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"
	"time"

	"helm.sh/helm/v4/pkg/chart/common"
	"helm.sh/helm/v4/pkg/chart/common/util"
	"helm.sh/helm/v4/pkg/chart/loader/archive"
	chart "helm.sh/helm/v4/pkg/chart/v2"
	"helm.sh/helm/v4/pkg/chart/v2/loader"
	chartutil "helm.sh/helm/v4/pkg/chart/v2/util"
	"helm.sh/helm/v4/pkg/engine"
)

type Request struct {
	// "chart": load the chart at Path (directory or .tgz).
	// "files": load a chart from the in-memory Files map (path -> content).
	// "version": report versions.
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
}

type Response struct {
	Files map[string]string `json:"files,omitempty"`
	Error *string           `json:"error,omitempty"`
	// Stage at which the error happened: load, values, dependencies, render.
	Stage string            `json:"stage,omitempty"`
	Info  map[string]string `json:"info,omitempty"`
}

func main() {
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
		names := make([]string, 0, len(req.Files))
		for n := range req.Files {
			names = append(names, n)
		}
		sort.Strings(names)
		var files []*archive.BufferedFile
		for _, n := range names {
			files = append(files, &archive.BufferedFile{Name: n, ModTime: time.Unix(0, 0), Data: []byte(req.Files[n])})
		}
		c, err := loader.LoadFiles(files)
		if err != nil {
			return fail("load", err)
		}
		return render(c, req)
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
	e := engine.Engine{Strict: req.Strict}
	files, err := e.Render(c, top)
	if err != nil {
		return fail("render", err)
	}
	return Response{Files: files}
}
