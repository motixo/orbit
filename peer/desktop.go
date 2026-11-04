// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/google/uuid"
	"github.com/pion/webrtc/v4"
)

type desktopHello struct {
	Open       bool   `json:"open"`
	Type       string `json:"type"`
	ID         string `json:"id"`
	Name       string `json:"name"`
	Share      string `json:"share"`
	Target     string `json:"target"`
	Nonce      string `json:"nonce"`
	Commitment string `json:"commitment"`
	Trusted    bool   `json:"trusted"`
	Resume     bool   `json:"resume"`
}

type desktopLink struct {
	h                   *hub
	id                  string
	pc                  *webrtc.PeerConnection
	dc                  *webrtc.DataChannel
	ctx                 context.Context
	cancel              context.CancelFunc
	deadline            *time.Timer
	mu                  sync.Mutex
	local               *server
	target              nearbyDevice
	hello               desktopHello
	nonce               string
	remote              desktopHello
	remoteFingerprint   string
	code                string
	approved, confirmed bool
	rejected            bool
	ready               atomic.Bool
	intentional         atomic.Bool
	wasReady            atomic.Bool
	lastPong            atomic.Int64
	requests            chan struct{}
}

func verificationCode(aID, aFP, aNonce, bID, bFP, bNonce string) string {
	identities := []string{aID + "/" + fingerprintHex(aFP) + "/" + aNonce, bID + "/" + fingerprintHex(bFP) + "/" + bNonce}
	sort.Strings(identities)
	sum := sha256.Sum256([]byte("orbit-desktop-v1\n" + strings.Join(identities, "\n")))
	return fmt.Sprintf("%06d", binary.BigEndian.Uint32(sum[:4])%1000000)
}

func (h *hub) desktopChannel(parent context.Context, pc *webrtc.PeerConnection, dc *webrtc.DataChannel, local *server, target nearbyDevice, closeConnection context.CancelFunc) *desktopLink {
	ctx, cancel := context.WithCancel(parent)
	l := &desktopLink{h: h, id: uuid.NewString(), pc: pc, dc: dc, ctx: ctx, cancel: cancel, local: local, target: target, requests: make(chan struct{}, 4)}
	var nonce [32]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		cancel()
		closeConnection()
		return l
	}
	name, _ := os.Hostname()
	l.nonce = hex.EncodeToString(nonce[:])
	l.hello = desktopHello{Type: "hello", ID: h.store.Host, Name: cleanDeviceName(name), Commitment: nonceCommitment(l.nonce)}
	if local != nil {
		l.hello.Share = local.Share
		l.hello.Target = target.Share
		l.hello.Trusted = h.store.trustsDesktop(target.ID, target.Fingerprint)
	}
	l.deadline = time.AfterFunc(12*time.Second, cancel)
	h.mu.Lock()
	h.native[l.id] = l
	h.mu.Unlock()
	dc.OnOpen(func() {
		if local != nil {
			if sendJSON(ctx, dc, l.hello) != nil {
				cancel()
			}
		}
	})
	dc.OnMessage(l.message)
	dc.OnClose(cancel)
	dc.OnError(func(error) { cancel() })
	go func() {
		<-ctx.Done()
		l.deadline.Stop()
		l.mu.Lock()
		l.ready.Store(false)
		s := l.local
		if s != nil {
			s.emit(map[string]any{"pairRequest": map[string]string{"id": l.id}})
			if l.wasReady.Load() {
				s.presence(l.id, nil, l.intentional.Load())
			}
		}
		l.mu.Unlock()
		h.mu.Lock()
		delete(h.native, l.id)
		delete(h.peers, pc)
		h.mu.Unlock()
		closeConnection()
	}()
	return l
}

func (l *desktopLink) message(message webrtc.DataChannelMessage) {
	if !message.IsString || len(message.Data) > 4096 || l.ctx.Err() != nil {
		l.cancel()
		return
	}
	switch string(message.Data) {
	case "not-paired":
		if l.target.ID != "" && l.pc != nil && l.pc.SCTP() != nil {
			cert := l.pc.SCTP().Transport().GetRemoteCertificate()
			if len(cert) != 0 {
				sum := sha256.Sum256(cert)
				l.forgetPairing(l.target.ID, hex.EncodeToString(sum[:]))
			}
		}
		l.dc.SendText("disconnect-ack")
		l.cancel()
		return
	case "disconnect", "forgotten":
		l.intentional.Store(true)
		if string(message.Data) == "forgotten" {
			if !l.ready.Load() {
				l.cancel()
				return
			}
			l.mu.Lock()
			id, fingerprint := l.remote.ID, l.remoteFingerprint
			l.mu.Unlock()
			if id != "" {
				l.forgetPairing(id, fingerprint)
			}
		}
		l.dc.SendText("disconnect-ack")
		l.cancel()
		return
	case "open-denied", "open-failed":
		if l.local != nil && l.hello.Open {
			err := "Opening Orbits is not allowed"
			if string(message.Data) == "open-failed" {
				err = "Could not open Orbit"
			}
			l.local.emit(map[string]string{"pairingError": err})
			l.intentional.Store(true)
		}
		l.cancel()
		return
	case "disconnect-ack":
		l.cancel()
		return
	case "ping", "pong":
		l.lastPong.Store(time.Now().UnixNano())
		if string(message.Data) == "ping" && l.dc.SendText("pong") != nil {
			l.cancel()
		}
		return
	}
	var command desktopHello
	if json.Unmarshal(message.Data, &command) != nil {
		l.cancel()
		return
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.rejected {
		l.cancel()
		return
	}
	switch command.Type {
	case "hello":
		if l.remote.ID != "" || command.ID == l.h.store.Host || uuid.Validate(command.ID) != nil || uuid.Validate(command.Share) != nil ||
			uuid.Validate(command.Target) != nil || len(command.Commitment) != 64 || command.Nonce != "" || cleanDeviceName(command.Name) == "" {
			l.cancel()
			return
		}
		if commitment, err := hex.DecodeString(command.Commitment); err != nil || len(commitment) != 32 {
			l.cancel()
			return
		}
		cert := l.pc.SCTP().Transport().GetRemoteCertificate()
		if len(cert) == 0 {
			l.cancel()
			return
		}
		sum := sha256.Sum256(cert)
		l.remoteFingerprint = strings.ToUpper(hex.EncodeToString(sum[:]))
		trusted := l.h.store.trustsDesktop(command.ID, l.remoteFingerprint)
		if command.Trusted && !trusted {
			l.intentional.Store(true)
			l.reject("not-paired")
			return
		}
		incomingOpen := l.local == nil && command.Open
		if incomingOpen && (command.Resume || !l.h.store.trustsDesktop(command.ID, l.remoteFingerprint) || !l.h.store.canOpen(command.ID)) {
			l.reject("open-denied")
			return
		}
		if l.local == nil {
			if incomingOpen {
				l.deadline.Reset(20 * time.Second)
				var err error
				l.local, err = l.h.openOrbit(l.ctx, command.ID)
				if err != nil {
					reason := "open-failed"
					if !l.h.store.canOpen(command.ID) {
						reason = "open-denied"
					}
					l.reject(reason)
					return
				}
			} else {
				l.local = l.h.session(command.Target, command.Resume)
			}
			if l.local == nil {
				l.cancel()
				return
			}
			l.hello.Share, l.hello.Target = l.local.Share, command.Share
		}
		if !incomingOpen && command.Target != l.local.Share || l.target.ID != "" && (command.ID != l.target.ID || !l.hello.Open && command.Share != l.target.Share) {
			l.cancel()
			return
		}
		if l.hello.Open {
			l.target.Share = command.Share
		}
		l.remote = command
		l.remote.Name = cleanDeviceName(command.Name)
		if trusted && !command.Trusted {
			l.forgetPairing(command.ID, l.remoteFingerprint)
			l.reject("not-paired")
			return
		}
		if command.Resume && !l.h.store.trustsDesktop(command.ID, l.remoteFingerprint) {
			go l.stop("disconnect")
			return
		}
		if l.target.ID == "" {
			l.hello.Trusted = l.h.store.trustsDesktop(command.ID, l.remoteFingerprint)
		}
		l.deadline.Reset(60 * time.Second)
		if l.target.ID == "" && sendJSON(l.ctx, l.dc, l.hello) != nil {
			l.cancel()
			return
		}
		if sendJSON(l.ctx, l.dc, desktopHello{Type: "reveal", Nonce: l.nonce}) != nil {
			l.cancel()
			return
		}
		go func() {
			select {
			case <-l.local.done:
				if l.local.intentional {
					l.stop("disconnect")
				} else {
					l.cancel()
				}
			case <-l.ctx.Done():
			}
		}()
	case "reveal":
		if l.remote.ID == "" || l.remote.Nonce != "" || len(command.Nonce) != 64 || nonceCommitment(command.Nonce) != l.remote.Commitment {
			l.cancel()
			return
		}
		if nonce, err := hex.DecodeString(command.Nonce); err != nil || len(nonce) != 32 {
			l.cancel()
			return
		}
		l.remote.Nonce = command.Nonce
		l.code = verificationCode(l.hello.ID, l.h.fingerprint, l.nonce, l.remote.ID, l.remoteFingerprint, command.Nonce)
		l.approved = l.remote.Trusted && l.h.store.trustsDesktop(l.remote.ID, l.remoteFingerprint)
		if l.approved {
			if sendJSON(l.ctx, l.dc, map[string]string{"type": "confirm"}) != nil {
				l.cancel()
				return
			}
		} else {
			l.showPairRequest()
		}
		l.activate()
	case "confirm":
		if l.code == "" {
			l.cancel()
			return
		}
		l.confirmed = true
		l.activate()
	default:
		l.cancel()
	}
}

func (l *desktopLink) forgetPairing(id, fingerprint string) {
	l.h.pairingsMu.Lock()
	changed, err := l.h.store.forgetDesktop(id, fingerprint)
	l.h.pairingsMu.Unlock()
	if err != nil {
		l.intentional.Store(true)
		if l.local != nil {
			l.local.emit(map[string]string{"pairingError": "Could not forget device"})
		}
	} else if changed {
		l.intentional.Store(true)
		l.h.broadcastPairings()
		if l.local != nil {
			l.local.emit(map[string]string{"pairingError": "Device unpaired. Pair again."})
		}
	}
}

func (l *desktopLink) reject(reason string) {
	l.rejected = true
	l.deadline.Reset(2 * time.Second)
	if l.dc.SendText(reason) != nil {
		l.cancel()
	}
}

func (l *desktopLink) showPairRequest() {
	l.local.emit(map[string]any{"pairRequest": map[string]any{"id": l.id, "name": l.remote.Name, "code": l.code, "approved": l.approved}})
}

func (l *desktopLink) activate() {
	if l.ctx.Err() != nil || l.rejected || l.code == "" || l.ready.Load() || !l.approved || !l.confirmed {
		return
	}
	l.h.pairingsMu.Lock()
	if (l.hello.Trusted || l.remote.Trusted) && !l.h.store.trustsDesktop(l.remote.ID, l.remoteFingerprint) {
		l.h.pairingsMu.Unlock()
		l.cancel()
		return
	}
	err := l.h.store.rememberDesktop(l.remote.ID, l.remote.Name, l.remoteFingerprint)
	l.h.pairingsMu.Unlock()
	if err != nil {
		l.local.emit(map[string]string{"pairingError": "Could not remember device"})
		l.cancel()
		return
	}
	l.deadline.Stop()
	l.ready.Store(true)
	l.wasReady.Store(true)
	l.lastPong.Store(time.Now().UnixNano())
	l.h.mu.Lock()
	l.h.peers[l.pc] = l.remote.ID
	l.h.mu.Unlock()
	l.h.broadcastPairings()
	l.local.emit(map[string]any{"pairRequest": map[string]string{"id": l.id}})
	peer := device{IP: l.target.IP, Name: l.remote.Name, PairID: l.remote.ID}
	l.local.presence(l.id, &peer, false)
	go l.heartbeat(peer)
	go l.publish()
}

func (l *desktopLink) heartbeat(peer device) {
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-l.ctx.Done():
			return
		case <-ticker.C:
			l.mu.Lock()
			if l.ctx.Err() != nil || !l.ready.Load() {
				l.mu.Unlock()
				return
			}
			elapsed := time.Since(time.Unix(0, l.lastPong.Load()))
			if stalled := elapsed >= 4*time.Second; peer.Reconnecting != stalled {
				peer.Reconnecting = stalled
				l.local.presence(l.id, &peer, false)
			}
			l.mu.Unlock()
			if elapsed >= 8*time.Second || l.dc.SendText("ping") != nil {
				l.cancel()
				return
			}
		}
	}
}

func (l *desktopLink) stop(reason string) {
	l.intentional.Store(true)
	l.dc.SendText(reason)
	select {
	case <-l.ctx.Done():
	case <-time.After(250 * time.Millisecond):
	}
	l.cancel()
}

func (l *desktopLink) request(dc *webrtc.DataChannel) {
	if strings.HasPrefix(dc.Label(), "croc:") {
		l.crocChannel(dc)
		return
	}
	if dc.Label() != "request" || !l.ready.Load() || l.ctx.Err() != nil {
		dc.Close()
		return
	}
	select {
	case l.requests <- struct{}{}:
		l.mu.Lock()
		s, remote := l.local, l.remote
		l.mu.Unlock()
		s.request(l.ctx, dc, device{IP: l.target.IP, Name: remote.Name, PairID: remote.ID, connection: l.nonce, native: true}, func() { <-l.requests })
	default:
		dc.Close()
	}
}

func (h *hub) decideDesktop(s *server, id string, approve bool) {
	h.mu.Lock()
	l := h.native[id]
	h.mu.Unlock()
	if l == nil {
		return
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.local != s || l.remote.ID == "" || l.ctx.Err() != nil {
		return
	}
	if !approve {
		l.rejected = true
		go l.stop("disconnect")
		return
	}
	l.approved = true
	l.showPairRequest()
	if sendJSON(l.ctx, l.dc, map[string]string{"type": "confirm"}) != nil {
		l.cancel()
		return
	}
	l.activate()
}

func (h *hub) disconnectPeer(s *server, id string) {
	s.mu.Lock()
	var browsers []func()
	for _, client := range s.clients {
		if client.PairID == id && client.disconnect != nil {
			browsers = append(browsers, client.disconnect)
		}
	}
	s.mu.Unlock()
	for _, stop := range browsers {
		go stop()
	}
	h.mu.Lock()
	var stops []context.CancelFunc
	prefix := s.Share + ":" + id
	for key, stop := range h.attempts {
		if key == prefix || strings.HasPrefix(key, prefix+":") {
			stops = append(stops, stop)
		}
	}
	links := make([]*desktopLink, 0, len(h.native))
	for _, l := range h.native {
		links = append(links, l)
	}
	h.mu.Unlock()
	for _, l := range links {
		l.mu.Lock()
		matches := l.local == s && (l.remote.ID == id || l.target.ID == id)
		l.mu.Unlock()
		if matches {
			go l.stop("disconnect")
		}
	}
	if len(stops) > 0 {
		go func() {
			time.Sleep(300 * time.Millisecond)
			for _, stop := range stops {
				stop()
			}
		}()
	}
}

func (h *hub) forgetDesktop(id string) {
	h.mu.Lock()
	for key, stop := range h.attempts {
		if parts := strings.Split(key, ":"); len(parts) >= 2 && parts[1] == id {
			defer stop()
		}
	}
	links := make([]*desktopLink, 0, len(h.native))
	for _, l := range h.native {
		links = append(links, l)
	}
	h.mu.Unlock()
	for _, l := range links {
		l.mu.Lock()
		matches := l.remote.ID == id
		l.mu.Unlock()
		if matches {
			l.stop("forgotten")
		}
	}
}

func (h *hub) connectDesktop(s *server, id string, open bool) {
	peer, err := h.findNearby(id, "")
	if err != nil {
		s.emit(map[string]string{"pairingError": err.Error()})
		return
	}
	if open && !h.store.trustsDesktop(id, peer.Fingerprint) {
		s.emit(map[string]string{"pairingError": "Pair this device first"})
		return
	}
	h.mu.Lock()
	links := make([]*desktopLink, 0, len(h.native))
	for _, l := range h.native {
		links = append(links, l)
	}
	h.mu.Unlock()
	for _, l := range links {
		l.mu.Lock()
		matches := l.local == s && (l.remote.ID == id || l.target.ID == id)
		l.mu.Unlock()
		if !open && matches && l.ctx.Err() == nil {
			return
		}
	}
	key := s.Share + ":" + id
	if open {
		key += ":" + uuid.NewString()
	}
	ctx, stop := context.WithCancel(context.Background())
	h.mu.Lock()
	if h.attempts == nil {
		h.attempts = make(map[string]context.CancelFunc)
	}
	if h.attempts[key] != nil {
		h.mu.Unlock()
		stop()
		return
	}
	h.attempts[key] = stop
	h.mu.Unlock()
	s.emit(map[string]any{"connecting": map[string]any{"id": id, "active": true}})
	go func() {
		defer func() {
			stop()
			h.mu.Lock()
			delete(h.attempts, key)
			h.mu.Unlock()
			s.emit(map[string]any{"connecting": map[string]any{"id": id, "active": false}})
		}()
		retry := false
		for {
			select {
			case <-ctx.Done():
				return
			case <-s.done:
				return
			default:
			}
			target := ""
			if retry {
				target = peer.Share
			}
			if next, err := h.findNearby(id, target); err == nil {
				if retry && !h.store.trustsDesktop(id, next.Fingerprint) {
					return
				}
				peer = next
			}
			connected, intentional, err := h.dialDesktop(ctx, s, &peer, retry, open && !retry)
			if ctx.Err() != nil {
				return
			}
			if intentional {
				return
			}
			retry = retry || connected
			if !retry {
				if err != nil {
					s.emit(map[string]string{"pairingError": "Could not connect to device"})
				}
				return
			}
			select {
			case <-ctx.Done():
				return
			case <-s.done:
				return
			case <-time.After(2 * time.Second):
			}
		}
	}()
}

func (h *hub) dialDesktop(parent context.Context, s *server, peer *nearbyDevice, resume, open bool) (bool, bool, error) {
	settings := webrtc.SettingEngine{}
	settings.SetNetworkTypes([]webrtc.NetworkType{webrtc.NetworkTypeUDP4})
	settings.SetSCTPMaxMessageSize(65536)
	pc, err := webrtc.NewAPI(webrtc.WithSettingEngine(settings)).NewPeerConnection(webrtc.Configuration{Certificates: []webrtc.Certificate{*h.certificate}})
	if err != nil {
		return false, false, err
	}
	defer pc.Close()
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	dc, err := pc.CreateDataChannel("desktop", nil)
	if err != nil {
		return false, false, err
	}
	l := h.desktopChannel(ctx, pc, dc, s, *peer, cancel)
	l.hello.Resume = resume
	l.hello.Open = open
	if open {
		l.deadline.Reset(25 * time.Second)
	}
	pc.OnDataChannel(l.request)
	pc.OnConnectionStateChange(func(state webrtc.PeerConnectionState) {
		if state == webrtc.PeerConnectionStateFailed || state == webrtc.PeerConnectionStateClosed {
			cancel()
		}
	})
	go func() {
		select {
		case <-s.done:
			if s.intentional {
				l.stop("disconnect")
			} else {
				l.cancel()
			}
		case <-ctx.Done():
		}
	}()
	offer, err := pc.CreateOffer(nil)
	if err != nil {
		return false, false, err
	}
	if err = pc.SetLocalDescription(offer); err != nil {
		return false, false, err
	}
	password := ""
	for _, line := range strings.Split(offer.SDP, "\r\n") {
		if strings.HasPrefix(line, "a=ice-pwd:") {
			password = strings.TrimPrefix(line, "a=ice-pwd:")
			break
		}
	}
	if password == "" {
		return false, false, errors.New("missing ICE credentials")
	}
	credentials := "libp2p+webrtc+v2/" + password
	answer := fmt.Sprintf("v=0\r\no=- 0 0 IN IP4 %s\r\ns=-\r\nt=0 0\r\na=ice-lite\r\nm=application %d UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 %s\r\na=mid:0\r\na=ice-options:ice2\r\na=ice-ufrag:%s\r\na=ice-pwd:%s\r\na=fingerprint:sha-256 %s\r\na=setup:passive\r\na=sctp-port:5000\r\na=max-message-size:65536\r\na=candidate:1 1 UDP 1 %s %d typ host\r\na=end-of-candidates\r\n", peer.IP, peer.Port, peer.IP, credentials, credentials, fingerprintSDP(peer.Fingerprint), peer.IP, peer.Port)
	if err = pc.SetRemoteDescription(webrtc.SessionDescription{Type: webrtc.SDPTypeAnswer, SDP: answer}); err != nil {
		return false, false, err
	}
	<-ctx.Done()
	l.mu.Lock()
	if l.wasReady.Load() {
		peer.Share = l.target.Share
	}
	l.mu.Unlock()
	if !l.wasReady.Load() && !l.intentional.Load() {
		return false, false, errors.New("connection failed")
	}
	return l.wasReady.Load(), l.intentional.Load(), nil
}

type desktopItem struct {
	Index   uint32 `json:"index"`
	Name    string `json:"name"`
	Size    *int64 `json:"size"`
	Local   bool   `json:"local"`
	Content bool   `json:"content"`
}

type publishRetry struct {
	attempts int
	next     time.Time
}

func (retry publishRetry) ready(now time.Time) bool {
	return retry.attempts < 3 && !now.Before(retry.next)
}

func (retry *publishRetry) failed(now time.Time) {
	retry.attempts++
	retry.next = now.Add(time.Second << retry.attempts)
}

func (l *desktopLink) publish() {
	sent := map[uint32]bool{}
	retries := map[uint32]publishRetry{}
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	for l.ctx.Err() == nil {
		req, _ := http.NewRequestWithContext(l.ctx, http.MethodGet, l.local.URL+"items", nil)
		response, err := l.local.client.Do(req)
		if err == nil {
			var items []desktopItem
			err = json.NewDecoder(response.Body).Decode(&items)
			response.Body.Close()
			if err == nil {
				for _, item := range items {
					if !item.Local || item.Size == nil || *item.Size < 0 || sent[item.Index] || !retries[item.Index].ready(time.Now()) {
						continue
					}
					if err := l.pushCroc(item); err == nil {
						sent[item.Index] = true
						if _, failed := retries[item.Index]; failed {
							l.local.emit(map[string]any{"transferError": map[string]string{"name": item.Name, "message": ""}})
							delete(retries, item.Index)
						}
					} else {
						if l.ctx.Err() != nil {
							return
						}
						retry := retries[item.Index]
						retry.failed(time.Now())
						retries[item.Index] = retry
						l.local.emit(map[string]any{"transferError": map[string]string{"name": item.Name, "message": err.Error()}})
					}
				}
			}
		}
		select {
		case <-l.ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

func (l *desktopLink) pushCroc(item desktopItem) error {
	source := fmt.Sprintf("%s/%d", l.local.Share, item.Index)
	body, err := l.rpc(request{Action: "received", Source: source})
	if err != nil {
		return err
	}
	var received bool
	if json.Unmarshal(body, &received) != nil {
		return errors.New("invalid receipt")
	}
	if received {
		return nil
	}
	transfer, err := l.local.prepareCroc(l.ctx, request{Action: "croc-file", Index: item.Index}, device{
		IP: l.target.IP, PairID: l.remote.ID, connection: l.nonce, native: true,
	})
	if err != nil {
		return err
	}
	defer transfer.cancel()
	command := request{Action: "croc-receive", Name: transfer.Name, Size: transfer.Size,
		Source: source, Content: item.Content && transfer.Size <= 1024*1024,
		Relay: &transfer.crocTicket,
	}
	if !command.valid() {
		return errors.New("invalid transfer")
	}
	body, err = l.rpc(command)
	if err != nil {
		return err
	}
	var receiver crocTicket
	if json.Unmarshal(body, &receiver) != nil || !receiver.valid() {
		return errors.New("invalid receiver")
	}
	defer func() { l.rpc(request{Action: "croc-cancel", ID: receiver.ID}) }()
	if _, err = l.rpc(request{Action: "croc-result", ID: receiver.ID}); err != nil {
		return err
	}
	select {
	case <-l.ctx.Done():
		return l.ctx.Err()
	case <-transfer.done:
		return transfer.err
	}
}

func (l *desktopLink) rpc(command request) ([]byte, error) {
	ctx, cancel := context.WithCancel(l.ctx)
	defer cancel()
	dc, err := l.pc.CreateDataChannel("request", nil)
	if err != nil {
		return nil, err
	}
	defer dc.Close()
	done := make(chan error, 1)
	status, size := 0, int64(-1)
	body := []byte{}
	var payload []byte
	var once sync.Once
	finish := func(err error) {
		once.Do(func() {
			if err == nil {
				payload = append([]byte(nil), body...)
			}
			done <- err
		})
	}
	deadline := time.AfterFunc(60*time.Second, func() { finish(errors.New("request timed out")) })
	defer deadline.Stop()
	dc.OnMessage(func(message webrtc.DataChannelMessage) {
		deadline.Reset(60 * time.Second)
		if !message.IsString {
			if status == 0 || len(body)+len(message.Data) > 65536 {
				finish(errors.New("invalid response"))
				return
			}
			body = append(body, message.Data...)
			return
		}
		var response struct {
			Type    string
			Status  int
			Size    int64
			Message string
		}
		if len(message.Data) > 4096 || json.Unmarshal(message.Data, &response) != nil {
			finish(errors.New("invalid response"))
			return
		}
		switch response.Type {
		case "response":
			if status != 0 || response.Size < 0 || response.Size > 65536 {
				finish(errors.New("invalid response"))
				return
			}
			status, size = response.Status, response.Size
		case "end":
			if status >= 200 && status < 300 && int64(len(body)) == size {
				finish(nil)
			} else {
				finish(errors.New("transfer failed"))
			}
		case "pending":
		case "error":
			finish(fmt.Errorf("transfer failed: %.256s", response.Message))
		default:
			finish(errors.New("transfer failed"))
		}
	})
	dc.OnClose(func() { finish(errors.New("request closed")) })
	dc.OnError(func(err error) { finish(err) })
	dc.OnOpen(func() {
		if err := sendJSON(ctx, dc, command); err != nil {
			finish(err)
		}
	})
	select {
	case err := <-done:
		return payload, err
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// Commit before revealing so neither peer can choose its nonce after seeing the other's.
func nonceCommitment(nonce string) string {
	sum := sha256.Sum256([]byte(nonce))
	return hex.EncodeToString(sum[:])
}
