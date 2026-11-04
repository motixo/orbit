// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"context"
	"encoding/json"
	"io"
	"net"
	"strings"
	"testing"
	"testing/synctest"
	"time"
)

func TestInactiveDesktopDoesNotPublishHeartbeatPresence(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		broker, reader := net.Pipe()
		defer broker.Close()
		defer reader.Close()
		go io.Copy(io.Discard, reader)
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		s := &server{connection: broker}
		link := &desktopLink{ctx: ctx, cancel: cancel, local: s, id: "closed"}
		link.lastPong.Store(time.Now().Add(-10 * time.Second).UnixNano())
		go link.heartbeat(device{PairID: "remote"})
		time.Sleep(1100 * time.Millisecond)
		synctest.Wait()
		if len(s.clients) != 0 {
			t.Fatal("heartbeat restored an inactive device")
		}
	})
}

func TestDisconnectPeerOnlyStopsMatchingBrowsersInThisShare(t *testing.T) {
	stopped := make(chan bool, 4)
	s := &server{clients: map[string]device{
		"first-tab":  {PairID: "browser", disconnect: func() { stopped <- true }},
		"second-tab": {PairID: "browser", disconnect: func() { stopped <- true }},
		"other":      {PairID: "other", disconnect: func() { stopped <- false }},
	}}
	other := &server{clients: map[string]device{
		"same-device": {PairID: "browser", disconnect: func() { stopped <- false }},
	}}
	h := &hub{sessions: []*server{s, other}}
	h.disconnectPeer(s, "browser")
	for range 2 {
		select {
		case matching := <-stopped:
			if !matching {
				t.Fatal("disconnected another device or share")
			}
		case <-time.After(time.Second):
			t.Fatal("did not disconnect every tab of the selected device")
		}
	}
	select {
	case <-stopped:
		t.Fatal("disconnected another device or share")
	default:
	}
}

func TestRegistrationDistinguishesIntentionalCloseFromLostConnection(t *testing.T) {
	for _, intentional := range []bool{false, true} {
		t.Run(map[bool]string{false: "interrupted", true: "intentional"}[intentional], func(t *testing.T) {
			pwa, token := "https://share.example/", strings.Repeat("a", 64)
			h := &hub{store: &pairStore{identity: identity{PWA: pwa}}}
			broker, native := net.Pipe()
			defer native.Close()
			native.SetDeadline(time.Now().Add(5 * time.Second))
			go h.register(broker)
			cfg := config{PWA: pwa, Share: "12345678-1234-4123-8123-123456789abc", Bind: "192.168.1.2", Token: token, URL: "http://127.0.0.1:12345/" + token + "/"}
			if err := json.NewEncoder(native).Encode(cfg); err != nil {
				t.Fatal(err)
			}
			decoder := json.NewDecoder(native)
			for range 2 {
				var notification map[string]any
				if err := decoder.Decode(&notification); err != nil {
					t.Fatal(err)
				}
			}
			h.mu.Lock()
			s := h.sessions[0]
			h.mu.Unlock()
			if intentional {
				if err := json.NewEncoder(native).Encode(map[string]bool{"disconnect": true}); err != nil {
					t.Fatal(err)
				}
			} else {
				native.Close()
			}
			select {
			case <-s.done:
			case <-time.After(5 * time.Second):
				t.Fatal("registration did not close")
			}
			if s.intentional != intentional {
				t.Fatalf("incorrect shutdown reason: %v", s.intentional)
			}
		})
	}
}

func TestConnectedDevicesKeepOtherTabsAndDevices(t *testing.T) {
	clients := map[string]device{
		"first-tab":  {IP: "192.168.1.2", Name: "iPhone"},
		"second-tab": {IP: "192.168.1.2"},
		"other":      {IP: "192.168.1.3", Name: "Pixel"},
	}
	devices := connectedDevices(clients)
	if len(devices) != 2 || devices[0].Name != "iPhone" {
		t.Fatalf("missing or duplicated devices: %+v", devices)
	}
	delete(clients, "first-tab")
	devices = connectedDevices(clients)
	if len(devices) != 2 || devices[0].IP != "192.168.1.2" {
		t.Fatalf("closing one tab removed an active device: %+v", devices)
	}
	clients["stalled-tab"] = device{IP: "192.168.1.2", Name: "iPhone", Reconnecting: true}
	devices = connectedDevices(clients)
	if len(devices) != 2 || devices[0].Reconnecting {
		t.Fatalf("an unresponsive tab hid a working connection: %+v", devices)
	}
	delete(clients, "second-tab")
	devices = connectedDevices(clients)
	if !devices[0].Reconnecting || devices[1].Reconnecting {
		t.Fatalf("connection status was not kept per device: %+v", devices)
	}
}

func TestDeviceNamesCannotInjectControlsOrUnboundedText(t *testing.T) {
	if name := cleanDeviceName(" \nPixel\u202e\t "); name != "Pixel" {
		t.Fatalf("unsafe device name: %q", name)
	}
	if name := cleanDeviceName(strings.Repeat("a", 1000)); len(name) != 80 {
		t.Fatalf("unbounded device name: %d", len(name))
	}
}

func TestDistinctPairingsOnTheSameAddressRemainConnected(t *testing.T) {
	clients := map[string]device{
		"first":     {IP: "192.168.1.2", PairID: "first"},
		"other-tab": {IP: "192.168.1.2", PairID: "first"},
		"second":    {IP: "192.168.1.2", PairID: "second"},
	}
	if devices := connectedDevices(clients); len(devices) != 2 {
		t.Fatalf("lost an active sender: %+v", devices)
	}
	delete(clients, "first")
	if devices := connectedDevices(clients); len(devices) != 2 {
		t.Fatalf("closing a tab removed another sender: %+v", devices)
	}
}
