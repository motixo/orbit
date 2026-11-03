// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"context"
	"encoding/hex"
	"fmt"
	"net"
	"os"
	"slices"
	"sort"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/libp2p/zeroconf/v2"
)

const discoveryService = "_orbit-share._udp"
const discoveryExpiry = 10 * time.Second

func networkAddresses() ([]string, error) {
	interfaces, err := net.Interfaces()
	if err != nil {
		return nil, err
	}
	sort.Slice(interfaces, func(i, j int) bool {
		wifiI, wifiJ := strings.HasPrefix(interfaces[i].Name, "wl"), strings.HasPrefix(interfaces[j].Name, "wl")
		if wifiI != wifiJ {
			return wifiI
		}
		return interfaces[i].Name < interfaces[j].Name
	})
	var result []string
	for _, iface := range interfaces {
		if iface.Flags&net.FlagUp == 0 || iface.Flags&net.FlagRunning == 0 || iface.Flags&net.FlagLoopback != 0 {
			continue
		}
		if slices.ContainsFunc([]string{"docker", "br-", "veth", "virbr"}, func(prefix string) bool { return strings.HasPrefix(iface.Name, prefix) }) {
			continue
		}
		addresses, err := iface.Addrs()
		if err != nil {
			return nil, err
		}
		for _, address := range addresses {
			ip, _, err := net.ParseCIDR(address.String())
			if err == nil && ip.To4() != nil && (ip.IsPrivate() || ip.IsLinkLocalUnicast()) {
				result = append(result, ip.String())
			}
		}
	}
	return result, nil
}

func selectBind(preferred string, addresses []string) string {
	if slices.Contains(addresses, preferred) {
		return preferred
	}
	if len(addresses) > 0 {
		return addresses[0]
	}
	return ""
}

type nearbyDevice struct {
	ID          string `json:"id"`
	Name        string `json:"name"`
	IP          string `json:"ip"`
	Share       string `json:"share"`
	Port        int    `json:"port"`
	Fingerprint string `json:"fingerprint"`
	seen        time.Time
}

func fingerprintHex(value string) string {
	value = strings.ToUpper(strings.ReplaceAll(value, ":", ""))
	decoded, err := hex.DecodeString(value)
	if err != nil || len(decoded) != 32 {
		return ""
	}
	return value
}

func fingerprintSDP(value string) string {
	value = fingerprintHex(value)
	var parts []string
	for i := 0; i < len(value); i += 2 {
		parts = append(parts, value[i:i+2])
	}
	return strings.Join(parts, ":")
}

func discovered(entry *zeroconf.ServiceEntry, self string, now time.Time) (nearbyDevice, bool) {
	var peer nearbyDevice
	if entry == nil || len(entry.Text) > 8 || entry.Port < 1 || entry.Port > 65535 || uuid.Validate(entry.Instance) != nil {
		return peer, false
	}
	fields := map[string]string{}
	for _, field := range entry.Text {
		if len(field) > 255 {
			return peer, false
		}
		key, value, ok := strings.Cut(field, "=")
		if !ok || fields[key] != "" {
			return peer, false
		}
		fields[key] = value
	}
	if fields["v"] != "1" || uuid.Validate(fields["id"]) != nil || fields["id"] == self {
		return peer, false
	}
	for _, ip := range entry.AddrIPv4 {
		if ip.To4() != nil && (ip.IsPrivate() || ip.IsLinkLocalUnicast()) {
			peer.IP = ip.String()
			break
		}
	}
	peer.ID, peer.Share, peer.Name = fields["id"], entry.Instance, cleanDeviceName(fields["name"])
	peer.Port, peer.Fingerprint, peer.seen = entry.Port, fingerprintHex(fields["fp"]), now
	return peer, peer.IP != "" && peer.Name != "" && peer.Fingerprint != ""
}

func (h *hub) nearby() []nearbyDevice {
	h.mu.Lock()
	defer h.mu.Unlock()
	peers := map[string]nearbyDevice{}
	now := time.Now()
	for share, peer := range h.discovered {
		if now.Sub(peer.seen) > discoveryExpiry {
			delete(h.discovered, share)
			continue
		}
		previous, exists := peers[peer.ID]
		if !exists || previous.Share == previous.ID && peer.Share != peer.ID || (peer.Share != peer.ID) == (previous.Share != previous.ID) && peer.Share < previous.Share {
			peers[peer.ID] = peer
		}
	}
	result := make([]nearbyDevice, 0, len(peers))
	for _, peer := range peers {
		result = append(result, peer)
	}
	sort.Slice(result, func(i, j int) bool { return result[i].ID < result[j].ID })
	return result
}

func (h *hub) broadcastDiscovery() {
	peers := h.nearby()
	h.mu.Lock()
	sessions := append([]*server(nil), h.sessions...)
	h.mu.Unlock()
	for _, s := range sessions {
		s.emit(map[string]any{"nearby": peers})
	}
}

func (h *hub) advertiseShare(share, bind string) (func(), error) {
	ip := net.ParseIP(bind)
	if ip == nil || ip.To4() == nil || (!ip.IsPrivate() && !ip.IsLinkLocalUnicast()) {
		return nil, fmt.Errorf("invalid network address")
	}
	name, _ := os.Hostname()
	text := []string{"v=1", "id=" + h.store.Host, "name=" + cleanDeviceName(name), "fp=" + fingerprintHex(h.fingerprint)}
	advertiser, err := zeroconf.RegisterProxy(share, discoveryService, "local", h.port, h.store.Host, []string{bind}, text, nil, zeroconf.TTL(10))
	if err != nil {
		return nil, err
	}
	return advertiser.Shutdown, nil
}

func (h *hub) advertise(s *server) func() {
	stop, err := h.advertiseShare(s.Share, s.Bind)
	if err != nil {
		s.emit(map[string]string{"discoveryError": "Network discovery is unavailable"})
		return func() {}
	}
	return stop
}

func (h *hub) discover(ctx context.Context) {
	for ctx.Err() == nil {
		scan, cancel := context.WithTimeout(ctx, 3*time.Second)
		entries := make(chan *zeroconf.ServiceEntry, 32)
		done := make(chan struct{})
		go func() {
			defer close(done)
			zeroconf.Browse(scan, discoveryService, "local", entries, zeroconf.SelectIPTraffic(zeroconf.IPv4))
		}()
	scanLoop:
		for {
			select {
			case entry, ok := <-entries:
				if !ok {
					break scanLoop
				}
				if peer, ok := discovered(entry, h.store.Host, time.Now()); ok {
					h.mu.Lock()
					if len(h.discovered) < 256 || h.discovered[peer.Share].ID != "" {
						h.discovered[peer.Share] = peer
					}
					h.mu.Unlock()
					h.broadcastDiscovery()
				}
			case <-scan.Done():
				break scanLoop
			case <-done:
				break scanLoop
			}
		}
		cancel()
	drain:
		for {
			select {
			case <-done:
				break drain
			case <-entries:
			}
		}
		h.broadcastDiscovery()
		select {
		case <-ctx.Done():
			return
		case <-time.After(time.Second):
		}
	}
}

func (h *hub) findNearby(id, share string) (nearbyDevice, error) {
	if share != "" {
		h.mu.Lock()
		peer, found := h.discovered[share]
		h.mu.Unlock()
		if found && peer.ID == id && time.Since(peer.seen) <= discoveryExpiry {
			return peer, nil
		}
		return nearbyDevice{}, fmt.Errorf("Device is offline")
	}
	for _, peer := range h.nearby() {
		if peer.ID == id {
			return peer, nil
		}
	}
	return nearbyDevice{}, fmt.Errorf("Device is offline")
}
