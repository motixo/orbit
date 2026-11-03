// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"context"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"
)

func TestRemoteOpenStopsWhenTheWindowLauncherExits(t *testing.T) {
	directory := t.TempDir()
	store, _, err := loadIdentity(directory)
	if err != nil {
		t.Fatal(err)
	}
	id, _, err := store.pair("Device", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.setAllowOpen(id, true); err != nil {
		t.Fatal(err)
	}
	opener := filepath.Join(directory, "orbit")
	if err := os.WriteFile(opener, []byte("#!/bin/sh\nexit 0\n"), 0700); err != nil {
		t.Fatal(err)
	}
	h := &hub{store: store, opener: opener}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if _, err := h.openOrbit(ctx, id); err == nil || ctx.Err() != nil || len(h.openings) != 0 {
		t.Fatal("an exited launcher kept the request pending", err)
	}
}

func TestRemoteOpenRejectionReachesTheClientWithoutRetrying(t *testing.T) {
	for reason, want := range map[string]string{
		"open-denied": "Opening Orbits is not allowed",
		"open-failed": "Could not open Orbit",
	} {
		t.Run(reason, func(t *testing.T) {
			broker, client := net.Pipe()
			defer broker.Close()
			defer client.Close()
			client.SetDeadline(time.Now().Add(5 * time.Second))
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			link := &desktopLink{ctx: ctx, cancel: cancel, local: &server{connection: broker}, hello: desktopHello{Open: true}}
			go link.message(webrtc.DataChannelMessage{IsString: true, Data: []byte(reason)})
			var notification map[string]string
			if err := json.NewDecoder(client).Decode(&notification); err != nil || notification["pairingError"] != want {
				t.Fatal("missing remote rejection", notification, err)
			}
			select {
			case <-ctx.Done():
			case <-time.After(5 * time.Second):
				t.Fatal("rejected request stayed open")
			}
			if !link.intentional.Load() || link.wasReady.Load() {
				t.Fatal("rejected request could connect or retry")
			}
		})
	}
}

func TestOpeningOrbitsRequiresPermissionAndClaimsOnlyOneWindow(t *testing.T) {
	store, _, err := loadIdentity(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	id, _, err := store.pair("iPhone", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	other, _, err := store.pair("Other", store.PWA)
	if err != nil {
		t.Fatal(err)
	}
	h := &hub{store: store, opener: "/missing/orbit"}
	for _, device := range []string{id, other, "unknown"} {
		if _, err := h.openOrbit(context.Background(), device); err == nil || !strings.Contains(err.Error(), "not allowed") {
			t.Fatal("unauthorized device reached the launcher", err)
		}
	}
	if _, err := store.setAllowOpen(id, true); err != nil {
		t.Fatal(err)
	}
	restored, _, err := loadIdentity(filepath.Dir(store.path))
	if err != nil || !restored.canOpen(id) || restored.canOpen(other) {
		t.Fatal("permission was not isolated and persistent", err)
	}
	pending := &openRequest{device: id, ready: make(chan *server, 1)}
	h.openings = map[string]*openRequest{"request": pending}
	s := &server{}
	if h.completeOpen("unknown", s) {
		t.Fatal("accepted an unrelated window")
	}
	if _, err := store.setAllowOpen(id, false); err != nil {
		t.Fatal(err)
	}
	if h.completeOpen("request", s) {
		t.Fatal("accepted a revoked request")
	}
	if _, err := store.setAllowOpen(id, true); err != nil {
		t.Fatal(err)
	}
	if !h.completeOpen("request", s) || h.completeOpen("request", s) || <-pending.ready != s {
		t.Fatal("request did not bind exactly one window")
	}
	store.path = filepath.Join(t.TempDir(), "missing", "identity.json")
	if _, err := store.setAllowOpen(id, false); err == nil || !store.canOpen(id) {
		t.Fatal("failed save changed the permission")
	}
	store.path = filepath.Join(t.TempDir(), "identity.json")
	if err := store.forget(id); err != nil || store.canOpen(id) || store.canOpen(other) {
		t.Fatal("forget left permission active", err)
	}
}

func TestPairedDeviceKeepsListenerWithoutPermissionToOpenOrbits(t *testing.T) {
	directory := t.TempDir()
	store, _, err := loadIdentity(directory)
	if err != nil {
		t.Fatal(err)
	}
	if store.hasPairings() {
		t.Fatal("unused listener should be idle")
	}
	id, _, err := store.pair("iPhone", store.PWA)
	if err != nil || !store.hasPairings() || store.canOpen(id) {
		t.Fatal("paired device needs a listener without opening permission", err)
	}
	for _, allowed := range []bool{true, false} {
		if _, err := store.setAllowOpen(id, allowed); err != nil || !store.hasPairings() {
			t.Fatal("changing opening permission stopped the listener", err)
		}
	}
	restored, _, err := loadIdentity(directory)
	if err != nil || !restored.hasPairings() || restored.canOpen(id) {
		t.Fatal("listener or permission changed after restart", err)
	}
	if err := restored.forget(id); err != nil || restored.hasPairings() {
		t.Fatal("forgetting the last device kept the listener active", err)
	}
}

func TestDesktopTrustRefreshPreservesOnlyTheSameIdentityPermissions(t *testing.T) {
	store, _, err := loadIdentity(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	id := "12345678-1234-4123-8123-123456789abc"
	fingerprint := strings.Repeat("AA", 32)
	if err := store.rememberDesktop(id, "Computer", fingerprint); err != nil {
		t.Fatal(err)
	}
	if _, err := store.setAllowOpen(id, true); err != nil {
		t.Fatal(err)
	}
	if err := store.rememberDesktop(id, "Computer", fingerprint); err != nil || !store.canOpen(id) {
		t.Fatal("trust refresh lost permissions", err)
	}
	if err := store.rememberDesktop(id, "Computer", strings.Repeat("BB", 32)); err != nil || store.canOpen(id) {
		t.Fatal("a changed identity inherited permissions", err)
	}
}
