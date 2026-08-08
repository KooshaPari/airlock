package service

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"syscall"
)

const (
	lifecycleManifestName   = ".airlock-lifecycle.json"
	lifecycleManifestFormat = 1
)

type artifactSpec struct {
	Name string
	Data []byte
	Mode os.FileMode
}
type lifecycleMetadata struct {
	Source      string
	Version     string
	InstalledAt string
}
type lifecycleFile struct {
	Name   string `json:"name"`
	SHA256 string `json:"sha256"`
	Inode  uint64 `json:"inode"`
	Mode   uint32 `json:"mode"`
}
type lifecycleManifest struct {
	Format      int             `json:"format"`
	Source      string          `json:"source"`
	Version     string          `json:"version"`
	InstalledAt string          `json:"installed_at"`
	Files       []lifecycleFile `json:"files"`
}

func readLifecycleManifest(path string) (lifecycleManifest, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return lifecycleManifest{}, err
	}
	var m lifecycleManifest
	if err := json.Unmarshal(data, &m); err != nil {
		return m, fmt.Errorf("parse lifecycle manifest: %w", err)
	}
	if m.Format != lifecycleManifestFormat {
		return m, fmt.Errorf("unsupported lifecycle manifest format %d", m.Format)
	}
	return m, nil
}

func fileFingerprint(path string) (lifecycleFile, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return lifecycleFile{}, err
	}
	if !info.Mode().IsRegular() {
		return lifecycleFile{}, fmt.Errorf("artifact %s is not a regular file", filepath.Base(path))
	}
	b, err := os.ReadFile(path)
	if err != nil {
		return lifecycleFile{}, err
	}
	h := sha256.Sum256(b)
	return lifecycleFile{Name: filepath.Base(path), SHA256: hex.EncodeToString(h[:]), Inode: info.Sys().(*syscall.Stat_t).Ino, Mode: uint32(info.Mode().Perm())}, nil
}

func verifyLifecycleOwnership(root string, names []string) error {
	m, err := readLifecycleManifest(filepath.Join(root, lifecycleManifestName))
	if err != nil {
		return fmt.Errorf("read lifecycle manifest: %w", err)
	}
	owned := make(map[string]lifecycleFile, len(m.Files))
	for _, f := range m.Files {
		if _, duplicate := owned[f.Name]; duplicate {
			return fmt.Errorf("duplicate manifest artifact %s", f.Name)
		}
		owned[f.Name] = f
	}
	if len(owned) != len(names) {
		return fmt.Errorf("lifecycle manifest artifact set is incomplete")
	}
	for _, name := range names {
		expected, ok := owned[name]
		if !ok {
			return fmt.Errorf("artifact %s is not managed", name)
		}
		got, err := fileFingerprint(filepath.Join(root, name))
		if errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("managed artifact %s is missing", name)
		}
		if err != nil {
			return err
		}
		if got.SHA256 != expected.SHA256 || got.Inode != expected.Inode || got.Mode != expected.Mode {
			return fmt.Errorf("managed artifact %s changed", name)
		}
	}
	return nil
}

func atomicWrite(path string, data []byte, mode os.FileMode) error {
	tmp, err := os.CreateTemp(filepath.Dir(path), ".airlock-tmp-")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if err := tmp.Chmod(mode); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmpName, path)
}

func installArtifacts(root string, specs []artifactSpec, meta lifecycleMetadata, activate func() error, deactivate func() error, restore ...func() error) error {
	seen := make(map[string]struct{}, len(specs))
	for _, s := range specs {
		if s.Name == "" || filepath.Base(s.Name) != s.Name || s.Name == lifecycleManifestName {
			return fmt.Errorf("invalid artifact name %q", s.Name)
		}
		if _, ok := seen[s.Name]; ok {
			return fmt.Errorf("duplicate artifact name %q", s.Name)
		}
		seen[s.Name] = struct{}{}
	}
	manifestPath := filepath.Join(root, lifecycleManifestName)
	oldManifest, manifestErr := os.ReadFile(manifestPath)
	if manifestErr != nil && !errors.Is(manifestErr, os.ErrNotExist) {
		return manifestErr
	}
	if errors.Is(manifestErr, os.ErrNotExist) {
		// The root may be a shared LaunchAgents/systemd directory. Only an
		// existing target artifact is a takeover candidate; unrelated entries
		// must remain untouched.
	}
	var oldFiles = map[string][]byte{}
	var oldModes = map[string]os.FileMode{}
	var existed = map[string]bool{}
	for _, s := range specs {
		p := filepath.Join(root, s.Name)
		b, err := os.ReadFile(p)
		if err == nil {
			if manifestErr != nil {
				return fmt.Errorf("unmanaged artifact %s present", s.Name)
			}
			oldFiles[s.Name] = b
			info, _ := os.Stat(p)
			oldModes[s.Name] = info.Mode().Perm()
			existed[s.Name] = true
		} else if !errors.Is(err, os.ErrNotExist) {
			return err
		} else if manifestErr == nil {
			return fmt.Errorf("managed artifact %s is missing", s.Name)
		}
	}
	if manifestErr == nil {
		if err := verifyLifecycleOwnership(root, manifestNames(specs)); err != nil {
			return err
		}
	}
	if err := os.MkdirAll(root, 0o755); err != nil {
		return err
	}
	rollback := func() {
		for _, s := range specs {
			p := filepath.Join(root, s.Name)
			if existed[s.Name] {
				_ = atomicWrite(p, oldFiles[s.Name], oldModes[s.Name])
			} else {
				_ = os.Remove(p)
			}
		}
		if oldManifest != nil {
			_ = atomicWrite(manifestPath, oldManifest, 0o644)
		} else {
			_ = os.Remove(manifestPath)
		}
	}
	for _, s := range specs {
		if err := atomicWrite(filepath.Join(root, s.Name), s.Data, s.Mode); err != nil {
			rollback()
			return err
		}
	}
	files := make([]lifecycleFile, 0, len(specs))
	for _, s := range specs {
		f, err := fileFingerprint(filepath.Join(root, s.Name))
		if err != nil {
			rollback()
			return err
		}
		files = append(files, f)
	}
	sort.Slice(files, func(i, j int) bool { return files[i].Name < files[j].Name })
	m := lifecycleManifest{Format: lifecycleManifestFormat, Source: meta.Source, Version: meta.Version, InstalledAt: meta.InstalledAt, Files: files}
	data, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		rollback()
		return err
	}
	data = append(data, '\n')
	if err := atomicWrite(manifestPath, data, 0o644); err != nil {
		rollback()
		return err
	}
	if activate != nil {
		if deactivate != nil {
			if err := deactivate(); err != nil {
				rollback()
				return err
			}
		}
		if err := activate(); err != nil {
			rollback()
			if len(restore) > 0 && restore[0] != nil {
				_ = restore[0]()
			}
			return err
		}
	}
	return nil
}

func manifestNames(specs []artifactSpec) []string {
	names := make([]string, len(specs))
	for i, s := range specs {
		names[i] = s.Name
	}
	return names
}

func uninstallArtifacts(root string, names []string, deactivate func() error) error {
	manifestPath := filepath.Join(root, lifecycleManifestName)
	if _, err := os.Stat(manifestPath); errors.Is(err, os.ErrNotExist) {
		return nil
	} else if err != nil {
		return err
	}
	if err := verifyLifecycleOwnership(root, names); err != nil {
		return err
	}
	if deactivate != nil {
		if err := deactivate(); err != nil {
			return err
		}
	}
	for _, name := range names {
		if err := os.Remove(filepath.Join(root, name)); err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
	}
	return os.Remove(manifestPath)
}
