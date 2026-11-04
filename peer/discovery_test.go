// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"context"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/libp2p/zeroconf/v2"
	"github.com/pion/webrtc/v4"
)

func TestDiscoveryBindFollowsNetworkChangesAndRetainsASelectedInterface(t *testing.T) {
	first, second := "192.168.1.2", "192.168.2.2"
	if selectBind(second, []string{first, second}) != second {
		t.Fatal("changed a still-connected interface")
	}
	if selectBind(first, []string{second}) != second || selectBind(first, nil) != "" {
		t.Fatal("retained an address after its interface disconnected")
	}
	if selectBind("", []string{second}) != second {
		t.Fatal("did not resume discovery after reconnecting")
	}
}

func TestOpenSharesFollowDesktopAddressChangesWithoutChangingIdentity(t *testing.T) {
	first, second := "192.168.1.2", "192.168.2.2"
	one := &server{config: config{Bind: first, Share: "first"}}
	two := &server{config: config{Bind: first, Share: "second"}}
	h := &hub{bind: first, hostname: "desktop.local", port: 42000, sessions: []*server{one, two}}
	for _, addresses := range [][]string{{second}, nil, {first}} {
		h.updateNetwork(addresses)
		want := ""
		if len(addresses) > 0 {
			want = addresses[0]
		}
		if h.bind != want || one.networkAddress() != want || two.networkAddress() != want {
			t.Fatal("open shares retained a disconnected network address")
		}
		if h.hostname != "desktop.local" || h.port != 42000 || one.Share != "first" || two.Share != "second" {
			t.Fatal("a network change replaced the pairing or share identity")
		}
	}
}

func TestDiscoveryRejectsForeignAddressesAndExpiresPresence(t *testing.T) {
	self, remote, share := uuid.NewString(), uuid.NewString(), uuid.NewString()
	now := time.Now()
	entry := &zeroconf.ServiceEntry{ServiceRecord: zeroconf.ServiceRecord{Instance: share}, Port: 42000, Text: []string{"v=1", "id=" + remote, "name=Workstation", "fp=" + strings.Repeat("AB", 32)}, AddrIPv4: []net.IP{net.ParseIP("192.168.1.2")}}
	peer, ok := discovered(entry, self, now)
	if !ok || peer.ID != remote {
		t.Fatal("valid nearby desktop was not found")
	}
	h := &hub{discovered: map[string]nearbyDevice{share: peer}}
	if len(h.nearby()) != 1 {
		t.Fatal("online desktop missing")
	}
	peer.seen = now.Add(-2 * discoveryExpiry)
	h.discovered[share] = peer
	if len(h.nearby()) != 0 {
		t.Fatal("offline desktop remained online")
	}
	for _, ip := range []string{"8.8.8.8", "127.0.0.1", "224.0.0.251", "0.0.0.0"} {
		entry.AddrIPv4 = []net.IP{net.ParseIP(ip)}
		if _, ok := discovered(entry, self, now); ok {
			t.Fatalf("accepted non-LAN address %s", ip)
		}
	}
	entry.AddrIPv4 = []net.IP{net.ParseIP("192.168.1.2")}
	if _, ok := discovered(entry, remote, now); ok {
		t.Fatal("discovered ourselves")
	}
	entry.Text[3] = "fp=invalid"
	if _, ok := discovered(entry, self, now); ok {
		t.Fatal("accepted malformed identity")
	}
}

func TestDesktopVerificationBindsBothIdentitiesAndFreshNonces(t *testing.T) {
	a, b := "12345678-1234-4123-8123-123456789abc", "22345678-1234-4123-8123-123456789abc"
	fpA, fpB := strings.Repeat("AB", 32), strings.Repeat("CD", 32)
	nonceA, nonceB := strings.Repeat("12", 32), strings.Repeat("34", 32)
	code := verificationCode(a, fpA, nonceA, b, fpB, nonceB)
	if len(code) != 6 || code != verificationCode(b, fpB, nonceB, a, fpA, nonceA) {
		t.Fatal("devices did not derive the same code")
	}
	if code == verificationCode(a, fpA, nonceA, b, fpA, nonceB) || code == verificationCode(a, fpA, nonceB, b, fpB, nonceB) {
		t.Fatal("code did not bind the certificate and handshake")
	}
	if nonceCommitment(nonceA) == nonceCommitment(nonceB) {
		t.Fatal("nonce commitment did not bind the reveal")
	}
	for _, approval := range []struct{ local, remote bool }{{false, false}, {true, false}, {false, true}} {
		link := &desktopLink{code: code, approved: approval.local, confirmed: approval.remote}
		link.activate()
		if link.ready.Load() {
			t.Fatal("sharing began before both sides confirmed")
		}
	}
}

func TestDesktopTrustPersistsAndRevokesThePinnedCertificate(t *testing.T) {
	directory := t.TempDir()
	store, _, err := loadIdentity(directory)
	if err != nil {
		t.Fatal(err)
	}
	remote, fp := uuid.NewString(), strings.Repeat("AB", 32)
	if store.trustsDesktop(remote, fp) {
		t.Fatal("unknown desktop trusted")
	}
	if err := store.rememberDesktop(remote, "Workstation", fp); err != nil {
		t.Fatal(err)
	}
	restored, _, err := loadIdentity(directory)
	if err != nil {
		t.Fatal(err)
	}
	if !restored.trustsDesktop(remote, fp) || restored.trustsDesktop(remote, strings.Repeat("CD", 32)) {
		t.Fatal("certificate pin was not preserved")
	}
	if !restored.devices()[0].Desktop {
		t.Fatal("native desktop not identified")
	}
	if err := restored.forget(remote); err != nil {
		t.Fatal(err)
	}
	if restored.trustsDesktop(remote, fp) {
		t.Fatal("forgotten desktop remains trusted")
	}
}

func TestUnknownDesktopCannotForgetAPairedIdentity(t *testing.T) {
	store, _, err := loadIdentity(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	id, fp := uuid.NewString(), strings.Repeat("AB", 32)
	if err := store.rememberDesktop(id, "Workstation", fp); err != nil {
		t.Fatal(err)
	}
	rejected := false
	link := &desktopLink{h: &hub{store: store}, ctx: context.Background(), cancel: func() { rejected = true }, remote: desktopHello{ID: id}}
	link.message(webrtc.DataChannelMessage{IsString: true, Data: []byte("forgotten")})
	if !rejected || !store.trustsDesktop(id, fp) {
		t.Fatal("unconfirmed desktop revoked a trusted identity")
	}
}

func TestRevokedPairingCannotBeRestoredByAPendingConfirmation(t *testing.T) {
	store, _, err := loadIdentity(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	id, fp := uuid.NewString(), strings.Repeat("AB", 32)
	if err := store.rememberDesktop(id, "Workstation", fp); err != nil {
		t.Fatal(err)
	}
	if err := store.forget(id); err != nil {
		t.Fatal(err)
	}
	cancelled := false
	link := &desktopLink{h: &hub{store: store}, ctx: context.Background(), cancel: func() { cancelled = true },
		hello: desktopHello{Trusted: true}, remote: desktopHello{ID: id}, remoteFingerprint: fp,
		code: "123456", approved: true, confirmed: true}
	link.activate()
	if store.contains(id) || link.ready.Load() || !cancelled {
		t.Fatal("a delayed confirmation restored a revoked pairing")
	}
}

func TestUnpairedDesktopRemovalRequiresTheSavedCertificate(t *testing.T) {
	store, _, err := loadIdentity(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	id, fp := uuid.NewString(), strings.Repeat("AB", 32)
	if err := store.rememberDesktop(id, "Workstation", fp); err != nil {
		t.Fatal(err)
	}
	link := &desktopLink{h: &hub{store: store}}
	link.forgetPairing(id, strings.Repeat("CD", 32))
	if !store.trustsDesktop(id, fp) || link.intentional.Load() {
		t.Fatal("an unrelated certificate revoked the pairing")
	}
	link.forgetPairing(id, fp)
	if store.contains(id) || !link.intentional.Load() {
		t.Fatal("the known device did not revoke its pairing or stop retries")
	}
	restored, _, err := loadIdentity(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if err := restored.rememberDesktop(id, "Workstation", fp); err != nil {
		t.Fatal(err)
	}
	restored.path += "/missing"
	if changed, err := restored.forgetDesktop(id, fp); err == nil || changed || !restored.trustsDesktop(id, fp) {
		t.Fatal("failed persistence removed the pairing")
	}
}

func TestReconnectFindsTheRequestedOrbitAmongOtherOpenShelves(t *testing.T) {
	now := time.Now()
	h := &hub{discovered: map[string]nearbyDevice{
		"first":  {ID: "desktop", Share: "first", seen: now},
		"second": {ID: "desktop", Share: "second", seen: now},
	}}
	peer, err := h.findNearby("desktop", "second")
	if err != nil || peer.Share != "second" {
		t.Fatal("reconnect selected another Orbit", err)
	}
	if _, err := h.findNearby("another-desktop", "second"); err == nil {
		t.Fatal("accepted a different device")
	}
	delete(h.discovered, "second")
	if _, err := h.findNearby("desktop", "second"); err == nil {
		t.Fatal("closed Orbit redirected to another shelf")
	}
}
