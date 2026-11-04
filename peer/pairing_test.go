// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"golang.org/x/sys/unix"
)

func TestPairingPersistsRejectsReplayAndRevokesOnlyOneDevice(t *testing.T) {
	directory := t.TempDir()
	store, certificate, err := loadIdentity(directory)
	if err != nil {
		t.Fatal(err)
	}
	id, token, err := store.pair("iPhone", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	other, _, err := store.pair("Pixel", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	key := sha256.Sum256([]byte(token))
	mac := hmac.New(sha256.New, key[:])
	mac.Write([]byte("first challenge"))
	proof := base64.RawURLEncoding.EncodeToString(mac.Sum(nil))
	restored, restoredCertificate, err := loadIdentity(directory)
	if err != nil {
		t.Fatal(err)
	}
	if !certificate.Equals(*restoredCertificate) || restored.Host != store.Host ||
		!restored.verify(id, "first challenge", proof) {
		t.Fatal("pairing identity did not survive restart")
	}
	if restored.verify(id, "new challenge", proof) || restored.verify(other, "first challenge", proof) {
		t.Fatal("accepted a replay or another device's proof")
	}
	if err := restored.forget(id); err != nil {
		t.Fatal(err)
	}
	if restored.verify(id, "first challenge", proof) || !restored.contains(other) {
		t.Fatal("revocation did not isolate the forgotten device")
	}
	devices := restored.devices()
	if len(devices) != 1 || devices[0].Hash != "" {
		t.Fatal("pairing list exposed authentication material")
	}
	info, err := os.Stat(restored.path)
	if err != nil || info.Mode().Perm() != 0600 {
		t.Fatal("identity is not private")
	}
	data, err := os.ReadFile(restored.path)
	if err != nil || strings.Contains(string(data), token) {
		t.Fatal("identity stored the device's raw enrollment token")
	}
}

func TestRenamingPairedDevicesPersistsWithoutChangingCredentials(t *testing.T) {
	directory := t.TempDir()
	store, _, err := loadIdentity(directory)
	if err != nil {
		t.Fatal(err)
	}
	id, _, err := store.pair("iPhone", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	other, _, err := store.pair("Pixel", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	original, untouched := store.Devices[id], store.Devices[other]
	if err := store.rename(id, "  My device\u202e\n  "); err != nil {
		t.Fatal(err)
	}
	restored, _, err := loadIdentity(directory)
	if err != nil {
		t.Fatal(err)
	}
	renamed := restored.Devices[id]
	if renamed.Name != "My device" || renamed.ID != original.ID || renamed.Hash != original.Hash || restored.Devices[other] != untouched {
		t.Fatal("rename changed credentials, another device, or did not persist")
	}
	if restored.rename(id, " \n\u202e ") == nil || restored.rename("missing", "Device") == nil {
		t.Fatal("accepted an empty name or an unknown device")
	}
	restored.path = filepath.Join(t.TempDir(), "missing", "identity.json")
	if restored.rename(id, "Unsaved") == nil || restored.Devices[id] != renamed {
		t.Fatal("a failed save changed the device name")
	}
}

func TestBrowserNamesReplaceIPLabelsWithoutOverwritingAliases(t *testing.T) {
	store, _, err := loadIdentity(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	id, _, err := store.pair("192.168.1.2", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	original := store.Devices[id]
	if changed, err := store.rememberBrowserName(id, "Firefox on Linux"); err != nil || !changed {
		t.Fatalf("could not replace IP label: %v", err)
	}
	restored, _, err := loadIdentity(filepath.Dir(store.path))
	if err != nil {
		t.Fatal(err)
	}
	want := original
	want.Name = "Firefox on Linux"
	if restored.Devices[id] != want {
		t.Fatal("name did not persist or pairing credentials changed")
	}
	if err := store.rename(id, "My computer"); err != nil {
		t.Fatal(err)
	}
	if changed, err := store.rememberBrowserName(id, "Chrome on Linux"); err != nil || changed || store.Devices[id].Name != "My computer" {
		t.Fatal("browser overwrote a custom device name")
	}
}

func TestNativeRegistrationCannotProxyOutsideTheLocalShare(t *testing.T) {
	token := strings.Repeat("a", 64)
	valid := config{PWA: "https://share.example/", Share: "12345678-1234-4123-8123-123456789abc", Bind: "192.168.1.2", Token: token, URL: "http://127.0.0.1:12345/" + token + "/"}
	if !validConfig(valid) {
		t.Fatal("rejected local share")
	}
	for _, target := range []string{
		"http://example.com/" + token + "/",
		"http://127.0.0.1:12345/elsewhere/",
		"http://127.0.0.1:12345/" + token + "/?redirect=elsewhere",
	} {
		invalid := valid
		invalid.URL = target
		if validConfig(invalid) {
			t.Fatalf("accepted unrelated endpoint %q", target)
		}
	}
}

func TestReconnectTargetsTheOriginalShelf(t *testing.T) {
	first := &server{config: config{Share: "first"}}
	second := &server{config: config{Share: "second"}}
	h := &hub{sessions: []*server{first, second}}
	if h.session("first", true) != first || h.session("missing", true) != nil {
		t.Fatal("reconnect selected another shelf")
	}
	h.sessions = h.sessions[1:]
	if h.session("first", false) != second {
		t.Fatal("explicit connection could not select the only open shelf")
	}
	if h.session("first", true) != nil {
		t.Fatal("closed shelf redirected to another session")
	}
}

func TestChangingPWARevokesPairingsAndClosesExistingSessions(t *testing.T) {
	store, cert, err := loadIdentity(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.setPWA("https://first.example/"); err != nil {
		t.Fatal(err)
	}
	id, _, err := store.pair("Device", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	if changed, err := store.setPWA("https://first.example/"); err != nil || changed || !store.contains(id) {
		t.Fatal("unchanged URL revoked pairing")
	}
	connection, client := net.Pipe()
	defer client.Close()
	h := &hub{store: store, sessions: []*server{{connection: connection}}}
	if err := h.setPWA("https://second.example/"); err != nil {
		t.Fatal(err)
	}
	if _, _, err := store.pair("Stale device", "https://first.example/"); err == nil {
		t.Fatal("stale session could enroll a device")
	}
	if store.contains(id) || len(h.sessions) != 0 {
		t.Fatal("old trust or sessions survived URL change")
	}
	var data [1]byte
	if _, err := client.Read(data[:]); err == nil {
		t.Fatal("old control connection remains open")
	}
	restored, restoredCert, err := loadIdentity(filepath.Dir(store.path))
	if err != nil || restored.PWA != "https://second.example/" || restored.contains(id) || !cert.Equals(*restoredCert) {
		t.Fatal("URL change was not persisted or changed the desktop identity")
	}
	id, _, err = store.pair("New device", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.setPWA("http://unsafe.example/"); err == nil || !store.contains(id) {
		t.Fatal("invalid URL changed trust")
	}
	store.path = filepath.Join(t.TempDir(), "missing", "identity.json")
	if _, err := store.setPWA("https://unsaved.example/"); err == nil || !store.contains(id) || store.PWA != "https://second.example/" {
		t.Fatal("failed save changed trust")
	}
}

func TestPWAControlRevokesDormantTrustAndRejectsStaleRegistration(t *testing.T) {
	store, _, err := loadIdentity(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	first, second := "https://first.example/", "https://second.example/"
	if _, err := store.setPWA(first); err != nil {
		t.Fatal(err)
	}
	id, _, err := store.pair("Device", first)
	if err != nil {
		t.Fatal(err)
	}
	h := &hub{store: store}
	for _, value := range []string{"http://unsafe.example/", second} {
		broker, client := net.Pipe()
		client.SetDeadline(time.Now().Add(5 * time.Second))
		go h.register(broker)
		if err := json.NewEncoder(client).Encode(config{UpdatePWA: value}); err != nil {
			t.Fatal(err)
		}
		var reply struct{ OK bool }
		if err := json.NewDecoder(client).Decode(&reply); err != nil {
			t.Fatal(err)
		}
		client.Close()
		if reply.OK != (value == second) || store.contains(id) != (value != second) || len(h.sessions) != 0 {
			t.Fatal("PWA control did not revoke trust independently of a shelf")
		}
	}
	broker, client := net.Pipe()
	defer client.Close()
	client.SetDeadline(time.Now().Add(5 * time.Second))
	go h.register(broker)
	token := strings.Repeat("a", 64)
	stale := config{PWA: first, Share: "12345678-1234-4123-8123-123456789abc", Bind: "192.168.1.2", Token: token, URL: "http://127.0.0.1:12345/" + token + "/"}
	if err := json.NewEncoder(client).Encode(stale); err != nil {
		t.Fatal(err)
	}
	var notification map[string]any
	if err := json.NewDecoder(client).Decode(&notification); err == nil || !store.matchesPWA(second) {
		t.Fatal("a stale shelf restored the previous PWA")
	}
}

func TestSharePortSurvivesRestartAndCannotSilentlyChange(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	occupied, err := listenUDP(ctx, 0)
	if err != nil {
		t.Fatal(err)
	}
	port := occupied.LocalAddr().(*net.UDPAddr).Port
	replacement, err := listenUDP(ctx, port)
	occupied.Close()
	if replacement != nil {
		replacement.Close()
		t.Fatal("silently selected another port")
	}
	if !errors.Is(err, unix.EADDRINUSE) {
		t.Fatalf("expected occupied port error, got %v", err)
	}
	restarted, err := listenUDP(context.Background(), port)
	if err != nil {
		t.Fatal(err)
	}
	defer restarted.Close()
	if restarted.LocalAddr().(*net.UDPAddr).Port != port {
		t.Fatal("port changed after restart")
	}
}

func TestServiceVersionsCannotOwnTheNetworkIdentityTogether(t *testing.T) {
	path := filepath.Join(t.TempDir(), "network.lock")
	first, err := lockNetworkService(context.Background(), path)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	if second, err := lockNetworkService(ctx, path); second != nil || !errors.Is(err, context.DeadlineExceeded) {
		if second != nil {
			second.Close()
		}
		t.Fatalf("another version acquired the network identity: %v", err)
	}
	first.Close()
	second, err := lockNetworkService(context.Background(), path)
	if err != nil {
		t.Fatal(err)
	}
	second.Close()
}
