// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log"
	"net"
	"net/http"
	"os"
	"slices"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode"

	"github.com/google/uuid"
	"github.com/libp2p/go-libp2p/p2p/transport/webrtc/udpmux"
	"github.com/pion/webrtc/v4"
)

const clientSDP = "v=0\r\no=- 0 0 IN IP4 %s\r\ns=-\r\nc=IN IP4 %s\r\nt=0 0\r\nm=application %d UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:0\r\na=ice-options:ice2\r\na=ice-ufrag:%s\r\na=ice-pwd:%s\r\na=fingerprint:sha-256 ba:78:16:bf:8f:01:cf:ea:41:41:40:de:5d:ae:22:23:b0:03:61:a3:96:17:7a:9c:b4:10:ff:61:f2:00:15:ad\r\na=setup:actpass\r\na=sctp-port:5000\r\na=max-message-size:65536\r\n"

type config struct {
	Bind, URL, Token, Share, PWA, Open string
	UpdatePWA                          string
}

type server struct {
	config
	client      *http.Client
	connection  net.Conn
	outputMu    sync.Mutex
	done        chan struct{}
	intentional bool
	mu          sync.Mutex
	clients     map[string]device
	revision    atomic.Uint64
	crocMu      sync.Mutex
	croc        map[string]*crocTransfer
}

func (s *server) emit(value any) {
	s.outputMu.Lock()
	defer s.outputMu.Unlock()
	s.connection.SetWriteDeadline(time.Now().Add(5 * time.Second))
	if err := json.NewEncoder(s.connection).Encode(value); err != nil {
		s.connection.Close()
	}
	s.connection.SetWriteDeadline(time.Time{})
}

type device struct {
	connection   string
	native       bool
	disconnect   func()
	IP           string `json:"ip"`
	Name         string `json:"name,omitempty"`
	PairID       string `json:"pair_id,omitempty"`
	Reconnecting bool   `json:"reconnecting,omitempty"`
}

func (s *server) networkAddress() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.Bind
}

func connectedDevices(clients map[string]device) []device {
	devices := make([]device, 0, len(clients))
	for _, client := range clients {
		devices = append(devices, client)
	}
	sort.Slice(devices, func(i, j int) bool {
		if devices[i].IP == devices[j].IP {
			if devices[i].PairID != devices[j].PairID {
				return devices[i].PairID < devices[j].PairID
			}
			if devices[i].Reconnecting != devices[j].Reconnecting {
				return !devices[i].Reconnecting
			}
			return devices[i].Name > devices[j].Name
		}
		return devices[i].IP < devices[j].IP
	})
	return slices.CompactFunc(devices, func(a, b device) bool { return a.IP == b.IP && a.PairID == b.PairID })
}

func cleanDeviceName(name string) string {
	name = strings.Map(func(r rune) rune {
		if unicode.IsControl(r) || unicode.Is(unicode.Cf, r) {
			return -1
		}
		return r
	}, name)
	runes := []rune(strings.TrimSpace(name))
	if len(runes) > 80 {
		runes = runes[:80]
	}
	return string(runes)
}

func (s *server) presence(id string, client *device, intentional bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.clients == nil {
		s.clients = make(map[string]device)
	}
	if client == nil {
		delete(s.clients, id)
	} else {
		s.clients[id] = *client
	}
	s.emit(map[string]any{"devices": connectedDevices(s.clients), "intentional": intentional})
}

func (h *hub) accept(mux *udpmux.UDPMux, candidate udpmux.Candidate) error {
	settings := webrtc.SettingEngine{}
	settings.SetAnsweringDTLSRole(webrtc.DTLSRoleServer)
	settings.SetICECredentials(candidate.LocalUfrag, candidate.LocalUfrag)
	settings.SetLite(true)
	settings.SetICEUDPMux(mux)
	settings.SetNetworkTypes([]webrtc.NetworkType{webrtc.NetworkTypeUDP4})
	// Browser consent checks can be six seconds apart.
	settings.SetICETimeouts(15*time.Second, 15*time.Second, 2*time.Second)
	settings.SetSCTPMaxMessageSize(65536)
	// The browser pins the desktop certificate; the remote device proves its QR or pairing token.
	settings.DisableCertificateFingerprintVerification(true)
	pc, err := webrtc.NewAPI(webrtc.WithSettingEngine(settings)).NewPeerConnection(webrtc.Configuration{Certificates: []webrtc.Certificate{*h.certificate}})
	if err != nil {
		return err
	}
	defer pc.Close()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	dataContext, stopTransfers := context.WithCancel(ctx)
	defer stopTransfers()
	deadline := time.AfterFunc(12*time.Second, cancel)
	defer deadline.Stop()
	var authenticated, control atomic.Bool
	var desktop atomic.Pointer[desktopLink]
	var life sync.Mutex
	lastPong := time.Now()
	closed, named, announced := false, false, false
	var selected *server
	var pairID, authToken string
	var challengeID, challengeShare, challenge string
	var openRequested bool
	client := device{IP: candidate.Addr.IP.String(), connection: candidate.LocalUfrag}
	defer func() {
		life.Lock()
		defer life.Unlock()
		closed = true
		authenticated.Store(false)
		if announced {
			selected.presence(candidate.LocalUfrag, nil, false)
		}
		h.mu.Lock()
		delete(h.peers, pc)
		h.mu.Unlock()
	}()
	pc.OnConnectionStateChange(func(state webrtc.PeerConnectionState) {
		switch state {
		case webrtc.PeerConnectionStateFailed, webrtc.PeerConnectionStateClosed:
			cancel()
		}
	})
	requests := make(chan struct{}, 4)
	pc.OnDataChannel(func(dc *webrtc.DataChannel) {
		switch dc.Label() {
		case "desktop":
			if !control.CompareAndSwap(false, true) {
				cancel()
				return
			}
			deadline.Stop()
			desktop.Store(h.desktopChannel(ctx, pc, dc, nil, nearbyDevice{IP: candidate.Addr.IP.String()}, cancel))
		case "orbit":
			if !control.CompareAndSwap(false, true) {
				cancel()
				return
			}
			stop := func(reply string) {
				closed = true
				authenticated.Store(false)
				stopTransfers()
				selected.presence(candidate.LocalUfrag, nil, true)
				announced = false
				deadline.Reset(time.Second)
				if dc.SendText(reply) != nil {
					cancel()
				}
			}
			client.disconnect = func() {
				life.Lock()
				defer life.Unlock()
				if !closed && ctx.Err() == nil && authenticated.Load() {
					stop("disconnect")
				}
			}
			dc.OnMessage(func(message webrtc.DataChannelMessage) {
				life.Lock()
				defer life.Unlock()
				if closed || ctx.Err() != nil {
					cancel()
					return
				}
				if announced && !authenticated.Load() {
					cancel()
					return
				}
				if authenticated.Load() {
					if message.IsString && string(message.Data) == "disconnect" {
						stop("disconnect-ack")
						return
					}
					if message.IsString && string(message.Data) == "disconnect-ack" {
						cancel()
						return
					}
					if message.IsString && (string(message.Data) == "pong" || string(message.Data) == "ping") {
						lastPong = time.Now()
						if client.Reconnecting {
							client.Reconnecting = false
							selected.presence(candidate.LocalUfrag, &client, false)
						}
						if string(message.Data) == "ping" && dc.SendText("pong") != nil {
							cancel()
						}
						return
					}
					var metadata struct {
						Type, Name, ID string
						Done           int64
					}
					if !message.IsString || len(message.Data) > 512 || json.Unmarshal(message.Data, &metadata) != nil {
						return
					}
					switch metadata.Type {
					case "croc-progress":
						selected.crocProgress(metadata.ID, client.connection, metadata.Done)
					case "device":
						if !named {
							named = true
							client.Name = cleanDeviceName(metadata.Name)
							selected.presence(candidate.LocalUfrag, &client, false)
						}
					case "pair":
						if pairID == "" {
							name := cleanDeviceName(metadata.Name)
							if name == "" {
								name = "Browser"
							}
							var err error
							pairID, authToken, err = h.store.pair(name, selected.PWA)
							if err != nil {
								sendJSON(ctx, dc, map[string]string{"type": "pairing-error", "message": "Could not remember device"})
								return
							}
							h.mu.Lock()
							h.peers[pc] = pairID
							h.mu.Unlock()
							client.PairID = pairID
							selected.presence(candidate.LocalUfrag, &client, false)
							h.broadcastPairings()
						}
						name, _ := os.Hostname()
						sendJSON(ctx, dc, map[string]any{
							"type": "paired", "id": pairID, "token": authToken,
							"host": selected.networkAddress(), "mdns": h.hostname, "name": cleanDeviceName(name),
							"port": h.port, "fingerprint": h.fingerprint, "share": selected.Share,
						})
					case "forget":
						if pairID != "" {
							if err := h.forget(pairID, pc); err != nil {
								sendJSON(ctx, dc, map[string]string{"type": "forget-error"})
								return
							}
							closed = true
							authenticated.Store(false)
							stopTransfers()
							deadline.Reset(2 * time.Second)
							dc.SendText("forgotten")
						}
					}
					return
				}
				if !message.IsString || len(message.Data) > 512 {
					cancel()
					return
				}
				if challengeID != "" {
					var proof struct{ Type, Signature string }
					if json.Unmarshal(message.Data, &proof) != nil || proof.Type != "proof" ||
						!h.store.verify(challengeID, challenge, proof.Signature) {
						cancel()
						return
					}
					pairID = challengeID
					if openRequested {
						deadline.Reset(20 * time.Second)
						var err error
						selected, err = h.openOrbit(ctx, pairID)
						if err != nil {
							closed = true
							deadline.Reset(2 * time.Second)
							reason := "open-failed"
							if !h.store.canOpen(pairID) {
								reason = "open-denied"
							}
							if dc.SendText(reason) != nil {
								cancel()
							}
							return
						}
					} else {
						selected = h.session(challengeShare, true)
					}
					challengeID = ""
				} else if len(message.Data) > 0 && message.Data[0] == '{' {
					var auth struct {
						Type, ID, Share string
						Resume          bool
						Open            bool
					}
					if json.Unmarshal(message.Data, &auth) != nil || auth.Type != "auth" {
						dc.SendText("rejected")
						cancel()
						return
					}
					if !h.store.contains(auth.ID) {
						closed = true
						deadline.Reset(2 * time.Second)
						if dc.SendText("not-paired") != nil {
							cancel()
						}
						return
					}
					var nonce [32]byte
					if _, err := rand.Read(nonce[:]); err != nil {
						cancel()
						return
					}
					challengeID, challengeShare = auth.ID, auth.Share
					if uuid.Validate(auth.Share) != nil {
						cancel()
						return
					}
					openRequested = auth.Open
					if openRequested && auth.Resume {
						cancel()
						return
					}
					if target := h.session(auth.Share, auth.Resume); target != nil && !openRequested {
						challengeShare = target.Share
					}
					purpose := "connect"
					if openRequested {
						purpose = "open"
					}
					challenge = "orbit-pair-v3\n" + h.fingerprint + "\n" + challengeShare + "\n" + purpose + "\n" + hex.EncodeToString(nonce[:])
					if err := sendJSON(ctx, dc, map[string]string{"type": "challenge", "value": challenge}); err != nil {
						cancel()
					}
					return
				} else {
					selected = h.qrSession(string(message.Data))
				}
				if selected == nil {
					if pairID != "" {
						dc.SendText("unavailable")
					} else {
						dc.SendText("rejected")
					}
					cancel()
					return
				}
				h.mu.Lock()
				if pairID != "" && !h.store.contains(pairID) {
					h.mu.Unlock()
					cancel()
					return
				}
				h.peers[pc] = pairID
				h.mu.Unlock()
				authenticated.Store(true)
				lastPong = time.Now()
				announced = true
				deadline.Stop()
				client.PairID = pairID
				selected.presence(candidate.LocalUfrag, &client, false)
				go func() {
					select {
					case <-selected.done:
						stopTransfers()
						if selected.intentional && dc.SendText("disconnect") == nil {
							select {
							case <-ctx.Done():
							case <-time.After(time.Second):
							}
						}
						cancel()
					case <-ctx.Done():
					}
				}()
				if sendJSON(ctx, dc, map[string]any{"type": "session", "share": selected.Share}) != nil {
					cancel()
					return
				}
				for _, notification := range []string{"ready", "device-info", "pairing"} {
					if err := dc.SendText(notification); err != nil {
						cancel()
						return
					}
				}
				go func() {
					ticker := time.NewTicker(time.Second)
					defer ticker.Stop()
					for {
						select {
						case <-ctx.Done():
							return
						case <-ticker.C:
							life.Lock()
							if closed || ctx.Err() != nil {
								life.Unlock()
								return
							}
							expired := time.Since(lastPong) >= 8*time.Second
							if reconnecting := time.Since(lastPong) >= 4*time.Second; reconnecting != client.Reconnecting {
								client.Reconnecting = reconnecting
								selected.presence(candidate.LocalUfrag, &client, false)
							}
							life.Unlock()
							if expired || dc.SendText("ping") != nil {
								cancel()
								return
							}
							if sendJSON(ctx, dc, map[string]any{"type": "items", "revision": selected.revision.Load()}) != nil {
								cancel()
								return
							}
						}
					}
				}()
			})
			dc.OnClose(cancel)
		case "request":
			if native := desktop.Load(); native != nil {
				native.request(dc)
				return
			}
			if !authenticated.Load() || ctx.Err() != nil {
				cancel()
				return
			}
			select {
			case requests <- struct{}{}:
				life.Lock()
				peer := client
				life.Unlock()
				selected.request(dataContext, dc, peer, func() { <-requests })
			default:
				dc.Close()
			}
		default:
			if strings.HasPrefix(dc.Label(), "croc:") && authenticated.Load() {
				selected.crocChannel(dataContext, dc, candidate.LocalUfrag)
			} else if native := desktop.Load(); native != nil && strings.HasPrefix(dc.Label(), "croc:") {
				native.crocChannel(dc)
			} else {
				cancel()
			}
		}
	})
	remote := fmt.Sprintf(clientSDP, candidate.Addr.IP, candidate.Addr.IP, candidate.Addr.Port, candidate.RemoteUfrag, candidate.RemotePwd)
	if err := pc.SetRemoteDescription(webrtc.SessionDescription{Type: webrtc.SDPTypeOffer, SDP: remote}); err != nil {
		return err
	}
	answer, err := pc.CreateAnswer(nil)
	if err != nil {
		return err
	}
	if err := pc.SetLocalDescription(answer); err != nil {
		return err
	}
	<-ctx.Done()
	return nil
}

func main() {
	if len(os.Args) > 1 && os.Args[1] == "--croc" {
		runCroc()
		return
	}
	if err := runService(); err != nil {
		log.Fatal(err)
	}
}
