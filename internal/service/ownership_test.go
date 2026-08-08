package service

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func testArtifact(data string) artifactSpec {
	return artifactSpec{Name: "airlock.service", Data: []byte(data), Mode: 0o644}
}

func testMetadata() lifecycleMetadata {
	return lifecycleMetadata{
		Source:      "test-source",
		Version:     "test-version",
		InstalledAt: "2026-08-05T00:00:00Z",
	}
}

func TestInstallArtifactsWritesAtomicOwnershipManifest(t *testing.T) {
	root := t.TempDir()
	if err := installArtifacts(root, []artifactSpec{testArtifact("new")}, testMetadata(), nil, nil); err != nil {
		t.Fatalf("installArtifacts() error = %v", err)
	}

	manifest, err := readLifecycleManifest(filepath.Join(root, lifecycleManifestName))
	if err != nil {
		t.Fatalf("readLifecycleManifest() error = %v", err)
	}
	if manifest.Format != lifecycleManifestFormat || manifest.Source != "test-source" || manifest.Version != "test-version" {
		t.Fatalf("manifest metadata = %#v", manifest)
	}
	if manifest.InstalledAt != testMetadata().InstalledAt {
		t.Fatalf("manifest installed_at = %q", manifest.InstalledAt)
	}
	if len(manifest.Files) != 1 || manifest.Files[0].Name != "airlock.service" {
		t.Fatalf("manifest files = %#v", manifest.Files)
	}
	if manifest.Files[0].SHA256 == "" || manifest.Files[0].Inode == 0 {
		t.Fatalf("manifest does not record hash/inode: %#v", manifest.Files[0])
	}
	if err := verifyLifecycleOwnership(root, []string{"airlock.service"}); err != nil {
		t.Fatalf("verifyLifecycleOwnership() error = %v", err)
	}
	if entries, err := os.ReadDir(root); err != nil {
		t.Fatal(err)
	} else if len(entries) != 2 {
		t.Fatalf("root entries = %d, want artifact plus manifest", len(entries))
	}
}

func TestInstallArtifactsRollsBackOnActivationFailure(t *testing.T) {
	root := t.TempDir()
	old := []byte("old")
	if err := installArtifacts(root, []artifactSpec{testArtifact(string(old))}, testMetadata(), nil, nil); err != nil {
		t.Fatal(err)
	}
	beforeManifest, err := os.ReadFile(filepath.Join(root, lifecycleManifestName))
	if err != nil {
		t.Fatal(err)
	}

	wantErr := errors.New("activation failed")
	err = installArtifacts(root, []artifactSpec{testArtifact("new")}, lifecycleMetadata{
		Source:      "next-source",
		Version:     "next-version",
		InstalledAt: "2026-08-05T00:01:00Z",
	}, func() error { return wantErr }, nil)
	if !errors.Is(err, wantErr) {
		t.Fatalf("installArtifacts() error = %v, want %v", err, wantErr)
	}
	got, err := os.ReadFile(filepath.Join(root, "airlock.service"))
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != string(old) {
		t.Fatalf("artifact after rollback = %q, want %q", got, old)
	}
	afterManifest, err := os.ReadFile(filepath.Join(root, lifecycleManifestName))
	if err != nil {
		t.Fatal(err)
	}
	if string(afterManifest) != string(beforeManifest) {
		t.Fatal("manifest changed after failed activation")
	}
}

func TestInstallArtifactsRejectsUnmanagedAndTamperedFiles(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "airlock.service"), []byte("unmanaged"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := installArtifacts(root, []artifactSpec{testArtifact("new")}, testMetadata(), nil, nil); err == nil || !strings.Contains(err.Error(), "unmanaged") {
		t.Fatalf("unmanaged install error = %v", err)
	}

	root = t.TempDir()
	if err := installArtifacts(root, []artifactSpec{testArtifact("original")}, testMetadata(), nil, nil); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "airlock.service"), []byte("tampered"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := installArtifacts(root, []artifactSpec{testArtifact("replacement")}, testMetadata(), nil, nil); err == nil || !strings.Contains(err.Error(), "changed") {
		t.Fatalf("tampered install error = %v", err)
	}
}

func TestUninstallArtifactsVerifiesOwnershipBeforeRemoval(t *testing.T) {
	root := t.TempDir()
	if err := installArtifacts(root, []artifactSpec{testArtifact("managed")}, testMetadata(), nil, nil); err != nil {
		t.Fatal(err)
	}
	if err := uninstallArtifacts(root, []string{"airlock.service"}, nil); err != nil {
		t.Fatalf("uninstallArtifacts() error = %v", err)
	}
	for _, name := range []string{"airlock.service", lifecycleManifestName} {
		if _, err := os.Stat(filepath.Join(root, name)); !os.IsNotExist(err) {
			t.Fatalf("%s still exists or stat failed: %v", name, err)
		}
	}

	root = t.TempDir()
	if err := installArtifacts(root, []artifactSpec{testArtifact("managed")}, testMetadata(), nil, nil); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "airlock.service"), []byte("tampered"), 0o644); err != nil {
		t.Fatal(err)
	}
	deactivated := false
	err := uninstallArtifacts(root, []string{"airlock.service"}, func() error {
		deactivated = true
		return nil
	})
	if err == nil || !strings.Contains(err.Error(), "changed") {
		t.Fatalf("tampered uninstall error = %v", err)
	}
	if deactivated {
		t.Fatal("deactivation ran before ownership verification")
	}
	if _, err := os.Stat(filepath.Join(root, "airlock.service")); err != nil {
		t.Fatalf("tampered artifact was removed: %v", err)
	}
}

func TestUninstallArtifactsMissingStateIsIdempotent(t *testing.T) {
	root := t.TempDir()
	called := false
	if err := uninstallArtifacts(root, []string{"airlock.service"}, func() error {
		called = true
		return nil
	}); err != nil {
		t.Fatalf("uninstallArtifacts() error = %v", err)
	}
	if called {
		t.Fatal("deactivation ran for missing managed state")
	}
}

func TestUninstallArtifactsRefusesMissingManagedArtifact(t *testing.T) {
	root := t.TempDir()
	if err := installArtifacts(root, []artifactSpec{testArtifact("managed")}, testMetadata(), nil, nil); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(filepath.Join(root, "airlock.service")); err != nil {
		t.Fatal(err)
	}
	if err := uninstallArtifacts(root, []string{"airlock.service"}, nil); err == nil || !strings.Contains(err.Error(), "missing") {
		t.Fatalf("missing artifact uninstall error = %v", err)
	}
}

func TestInstallArtifactsRejectsMalformedExistingArtifact(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "airlock.plist"), []byte("[]"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := installArtifacts(root, []artifactSpec{{Name: "airlock.plist", Data: []byte("xml"), Mode: 0o644}}, testMetadata(), nil, nil, nil); err == nil || !strings.Contains(err.Error(), "unmanaged") {
		t.Fatalf("error = %v", err)
	}
}

func TestInstallArtifactsRejectsUnsafeDuplicateAndIncompleteSpecs(t *testing.T) {
	root := t.TempDir()
	bad := []artifactSpec{{Name: "../escape", Data: []byte("x"), Mode: 0o644}}
	if err := installArtifacts(root, bad, testMetadata(), nil, nil, nil); err == nil || !strings.Contains(err.Error(), "invalid artifact name") {
		t.Fatalf("traversal error = %v", err)
	}
	dup := []artifactSpec{{Name: "a", Data: []byte("x"), Mode: 0o644}, {Name: "a", Data: []byte("y"), Mode: 0o644}}
	if err := installArtifacts(root, dup, testMetadata(), nil, nil, nil); err == nil || !strings.Contains(err.Error(), "duplicate") {
		t.Fatalf("duplicate error = %v", err)
	}
}

func TestVerifyLifecycleOwnershipChecksModeAndManifestCompleteness(t *testing.T) {
	root := t.TempDir()
	if err := installArtifacts(root, []artifactSpec{{Name: "a", Data: []byte("x"), Mode: 0o644}, {Name: "b", Data: []byte("y"), Mode: 0o600}}, testMetadata(), nil, nil, nil); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(filepath.Join(root, "a"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := verifyLifecycleOwnership(root, []string{"a", "b"}); err == nil || !strings.Contains(err.Error(), "changed") {
		t.Fatalf("mode error = %v", err)
	}
}

func TestInstallArtifactsRestoresRuntimeAfterActivationFailure(t *testing.T) {
	root := t.TempDir()
	if err := installArtifacts(root, []artifactSpec{testArtifact("old")}, testMetadata(), nil, nil, nil); err != nil {
		t.Fatal(err)
	}
	deactivated, restored := false, false
	want := errors.New("activation failed")
	err := installArtifacts(root, []artifactSpec{testArtifact("new")}, testMetadata(), func() error { return want }, func() error { deactivated = true; return nil }, func() error { restored = true; return nil })
	if !errors.Is(err, want) || !deactivated || !restored {
		t.Fatalf("err=%v deactivated=%v restored=%v", err, deactivated, restored)
	}
}
