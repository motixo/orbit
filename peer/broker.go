// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"bufio"
	"context"
	"crypto/subtle"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/libp2p/go-libp2p/p2p/transport/webrtc/udpmux"
	"github.com/pion/webrtc/v4"
	"golang.org/x/sys/unix"
)

type hub struct {
	opener      string
	bind        string
	openings    map[string]*openRequest
	lastOpen    map[string]time.Time
	mu          sync.Mutex
	pairingsMu  sync.Mutex
	store       *pairStore
	certificate *webrtc.Certificate
	fingerprint string
	hostname    string
	port        int
	sessions    []*server
	peers       map[*webrtc.PeerConnection]string
	discovered  map[string]nearbyDevice
	native      map[string]*desktopLink
	attempts    map[string]context.CancelFunc
}

func runtimePath() (string, error) {
	directory := os.Getenv("XDG_RUNTIME_DIR")
	if directory == "" || !filepath.IsAbs(directory) {
		return "", errors.New("missing runtime directory")
	}
	directory = filepath.Join(directory, "orbit-share")
	if version := os.Getenv("ORBIT_PEER_VERSION"); version != "" {
		decoded, err := hex.DecodeString(version)
		if err != nil || len(decoded) != 8 {
			return "", errors.New("invalid peer version")
		}
		directory = filepath.Join(directory, version)
	}
	if err := os.MkdirAll(directory, 0700); err != nil {
		return "", err
	}
	if err := os.Chmod(directory, 0700); err != nil {
		return "", err
	}
	return directory, nil
}

func validPWA(value string) bool {
	endpoint, err := url.Parse(value)
	return err == nil && endpoint.Scheme == "https" && endpoint.Hostname() != "" &&
		endpoint.User == nil && endpoint.RawQuery == "" && !endpoint.ForceQuery && endpoint.Fragment == ""
}

func validConfig(cfg config) bool {
	ip := net.ParseIP(cfg.Bind)
	endpoint, err := url.Parse(cfg.URL)
	key, tokenErr := hex.DecodeString(cfg.Token)
	return (cfg.Open == "" || uuid.Validate(cfg.Open) == nil) && validPWA(cfg.PWA) && uuid.Validate(cfg.Share) == nil && err == nil && endpoint.Scheme == "http" && endpoint.Hostname() == "127.0.0.1" &&
		endpoint.User == nil && endpoint.RawQuery == "" && !endpoint.ForceQuery && endpoint.Fragment == "" &&
		endpoint.Path == "/"+cfg.Token+"/" && endpoint.Port() != "" && tokenErr == nil && len(key) == 32 &&
		ip != nil && ip.To4() != nil && (ip.IsPrivate() || ip.IsLinkLocalUnicast())
}

func (h *hub) setPWA(pwa string) error {
	changed, err := h.store.setPWA(pwa)
	if err != nil || !changed {
		return err
	}
	h.mu.Lock()
	sessions := h.sessions
	h.sessions = nil
	peers := make([]*webrtc.PeerConnection, 0, len(h.peers))
	for peer := range h.peers {
		peers = append(peers, peer)
	}
	h.mu.Unlock()
	for _, session := range sessions {
		session.connection.Close()
	}
	for _, peer := range peers {
		go peer.Close()
	}
	return nil
}

func (h *hub) register(connection net.Conn) {
	defer connection.Close()
	connection.SetReadDeadline(time.Now().Add(10 * time.Second))
	input := bufio.NewScanner(connection)
	input.Buffer(make([]byte, 4096), 8192)
	var cfg config
	if !input.Scan() || json.Unmarshal(input.Bytes(), &cfg) != nil {
		return
	}
	if cfg.UpdatePWA != "" {
		h.pairingsMu.Lock()
		err := h.setPWA(cfg.UpdatePWA)
		h.pairingsMu.Unlock()
		connection.SetWriteDeadline(time.Now().Add(5 * time.Second))
		json.NewEncoder(connection).Encode(map[string]bool{"ok": err == nil})
		return
	}
	if !validConfig(cfg) {
		return
	}
	connection.SetReadDeadline(time.Time{})
	s := &server{config: cfg, connection: connection, done: make(chan struct{}), client: &http.Client{Transport: &http.Transport{
		Proxy: nil, DialContext: (&net.Dialer{Timeout: 5 * time.Second}).DialContext,
		ResponseHeaderTimeout: 60 * time.Second, IdleConnTimeout: 60 * time.Second,
	}}}
	h.pairingsMu.Lock()
	if !h.store.matchesPWA(cfg.PWA) {
		h.pairingsMu.Unlock()
		return
	}
	h.mu.Lock()
	if len(h.sessions) >= 32 {
		h.mu.Unlock()
		h.pairingsMu.Unlock()
		return
	}
	s.emit(map[string]any{"port": h.port, "fingerprint": h.fingerprint})
	h.sessions = append(h.sessions, s)
	h.bind = cfg.Bind
	h.mu.Unlock()
	h.pairingsMu.Unlock()
	if cfg.Open != "" && !h.completeOpen(cfg.Open, s) {
		h.mu.Lock()
		for i, session := range h.sessions {
			if session == s {
				h.sessions = append(h.sessions[:i], h.sessions[i+1:]...)
				break
			}
		}
		h.mu.Unlock()
		close(s.done)
		return
	}
	h.broadcastPairings()
	if h.discovered != nil {
		s.emit(map[string]any{"nearby": h.nearby()})
	}
	defer func() {
		h.mu.Lock()
		for i, existing := range h.sessions {
			if existing == s {
				h.sessions = append(h.sessions[:i], h.sessions[i+1:]...)
				break
			}
		}
		close(s.done)
		h.mu.Unlock()
		s.client.CloseIdleConnections()
	}()
	for input.Scan() {
		var command struct {
			Items          bool
			Disconnect     bool
			Forget         string
			Rename         *struct{ ID, Name string }
			Connect        string
			Approve        string
			Reject         string
			DisconnectPeer string
			OpenOrbit      string
			AllowOpen      *struct {
				ID      string
				Allowed bool
			}
		}
		if json.Unmarshal(input.Bytes(), &command) != nil {
			continue
		}
		switch {
		case command.AllowOpen != nil:
			if _, err := h.store.setAllowOpen(command.AllowOpen.ID, command.AllowOpen.Allowed); err != nil {
				s.emit(map[string]string{"pairingError": "Could not save permission"})
			} else {
				h.broadcastPairings()
			}
		case command.OpenOrbit != "":
			h.connectDesktop(s, command.OpenOrbit, true)
		case command.Connect != "":
			h.connectDesktop(s, command.Connect, false)
		case command.Approve != "":
			h.decideDesktop(s, command.Approve, true)
		case command.Reject != "":
			h.decideDesktop(s, command.Reject, false)
		case command.DisconnectPeer != "":
			h.disconnectDesktop(s, command.DisconnectPeer)
		case command.Disconnect:
			s.intentional = true
			return
		case command.Items:
			s.revision.Add(1)
		case command.Forget != "":
			if err := h.forget(command.Forget, nil); err != nil {
				s.emit(map[string]string{"pairingError": "Could not forget device"})
			}
		case command.Rename != nil:
			if err := h.store.rename(command.Rename.ID, command.Rename.Name); err != nil {
				s.emit(map[string]string{"pairingError": "Could not rename device"})
			} else {
				h.broadcastPairings()
			}
		}
	}
}

func (h *hub) qrSession(token string) *server {
	if len(token) != 64 {
		return nil
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	for i := len(h.sessions) - 1; i >= 0; i-- {
		s := h.sessions[i]
		if subtle.ConstantTimeCompare([]byte(s.Token), []byte(token)) == 1 {
			return s
		}
	}
	return nil
}

func (h *hub) session(id string, resume bool) *server {
	h.mu.Lock()
	defer h.mu.Unlock()
	for i := len(h.sessions) - 1; i >= 0; i-- {
		if h.sessions[i].Share == id {
			return h.sessions[i]
		}
	}
	if !resume && len(h.sessions) == 1 {
		return h.sessions[0]
	}
	return nil
}

func (h *hub) broadcastPairings() {
	h.pairingsMu.Lock()
	defer h.pairingsMu.Unlock()
	devices := h.store.devices()
	h.mu.Lock()
	sessions := append([]*server(nil), h.sessions...)
	h.mu.Unlock()
	for _, session := range sessions {
		session.emit(map[string]any{"pairings": devices})
	}
}

func (h *hub) forget(id string, keep *webrtc.PeerConnection) error {
	if err := h.store.forget(id); err != nil {
		return err
	}
	h.forgetDesktop(id)
	h.mu.Lock()
	var peers []*webrtc.PeerConnection
	for peer, pairID := range h.peers {
		if pairID == id && peer != keep {
			peers = append(peers, peer)
		}
	}
	h.mu.Unlock()
	for _, peer := range peers {
		peer.Close()
	}
	h.broadcastPairings()
	return nil
}

func listenUDP(ctx context.Context, port int) (*net.UDPConn, error) {
	for {
		socket, err := net.ListenUDP("udp4", &net.UDPAddr{Port: port})
		if port == 0 || !errors.Is(err, unix.EADDRINUSE) {
			return socket, err
		}
		select {
		case <-ctx.Done():
			return nil, err
		case <-time.After(100 * time.Millisecond):
		}
	}
}

func lockNetworkService(ctx context.Context, path string) (*os.File, error) {
	lock, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0600)
	if err != nil {
		return nil, err
	}
	for {
		err = unix.Flock(int(lock.Fd()), unix.LOCK_EX|unix.LOCK_NB)
		if err == nil {
			return lock, nil
		}
		if !errors.Is(err, unix.EWOULDBLOCK) {
			lock.Close()
			return nil, err
		}
		select {
		case <-ctx.Done():
			lock.Close()
			return nil, ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
}

func runService() error {
	runtime, err := runtimePath()
	if err != nil {
		return err
	}
	lock, err := os.OpenFile(filepath.Join(runtime, "service.lock"), os.O_CREATE|os.O_RDWR, 0600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := unix.Flock(int(lock.Fd()), unix.LOCK_EX|unix.LOCK_NB); err != nil {
		return nil
	}
	startup, stopStartup := context.WithTimeout(context.Background(), 25*time.Second)
	defer stopStartup()
	// Different executable versions still share one network port and pairing identity.
	networkLock, err := lockNetworkService(startup, filepath.Join(os.Getenv("XDG_RUNTIME_DIR"), "orbit-share", "network.lock"))
	if err != nil {
		return err
	}
	defer networkLock.Close()
	configDir, err := os.UserConfigDir()
	if err != nil {
		return err
	}
	store, certificate, err := loadIdentity(filepath.Join(configDir, "orbit", "share"))
	if err != nil {
		return err
	}
	socket, err := listenUDP(startup, store.Port)
	if err != nil {
		return err
	}
	defer socket.Close()
	// Pairings may have changed while an existing service was releasing the port.
	store, certificate, err = loadIdentity(filepath.Join(configDir, "orbit", "share"))
	if err != nil {
		return err
	}
	if _, err := store.setPWA(os.Getenv("ORBIT_SHARE_PWA")); err != nil {
		return err
	}
	port := socket.LocalAddr().(*net.UDPAddr).Port
	if store.Port != 0 && store.Port != port {
		return errors.New("sharing port changed during startup")
	}
	if store.Port == 0 {
		store.Port = port
		if err := store.save(); err != nil {
			return err
		}
	}
	stopStartup()
	fingerprints, err := certificate.GetFingerprints()
	if err != nil || len(fingerprints) == 0 {
		return errors.New("missing certificate fingerprint")
	}
	h := &hub{opener: os.Getenv("ORBIT_EXECUTABLE"), bind: os.Getenv("ORBIT_PEER_BIND"), store: store, certificate: certificate, fingerprint: strings.ToUpper(fingerprints[0].Value),
		port: store.Port, hostname: store.Host + ".local", peers: make(map[*webrtc.PeerConnection]string),
		discovered: make(map[string]nearbyDevice), native: make(map[string]*desktopLink)}
	path := filepath.Join(runtime, "control.sock")
	os.Remove(path)
	listener, err := net.Listen("unix", path)
	if err != nil {
		return err
	}
	defer listener.Close()
	defer os.Remove(path)
	if err := os.Chmod(path, 0600); err != nil {
		return err
	}
	go func() {
		for {
			connection, err := listener.Accept()
			if err != nil {
				return
			}
			go h.register(connection)
		}
	}()
	mux := udpmux.NewUDPMux(socket)
	defer mux.Close()
	mux.Start()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go h.discover(ctx)
	go func() {
		ticker := time.NewTicker(time.Second)
		defer ticker.Stop()
		emptySince := time.Now()
		executable, _ := os.Executable()
		running, _ := os.Stat("/proc/self/exe")
		advertisements := make(map[string]func())
		var stopHostname func()
		advertisedBind := ""
		defer func() {
			for _, stop := range advertisements {
				stop()
			}
			if stopHostname != nil {
				stopHostname()
			}
		}()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				addresses, addressErr := networkAddresses()
				if addressErr == nil {
					h.updateNetwork(addresses)
				}
				h.mu.Lock()
				sessions := append([]*server(nil), h.sessions...)
				bind := h.bind
				h.mu.Unlock()
				empty := len(sessions) == 0
				if _, err := os.Stat(h.opener); os.IsNotExist(err) {
					mux.Close()
					return
				}
				if empty && running != nil {
					current, err := os.Stat(executable)
					if err != nil || !os.SameFile(running, current) {
						mux.Close()
						return
					}
				}
				paired := h.store.hasPairings()
				shares := make(map[string]bool, len(sessions)+1)
				if bind != "" {
					if paired {
						shares[h.store.Host] = true
					}
					for _, s := range sessions {
						shares[s.Share] = true
					}
				}
				for share, stop := range advertisements {
					if !shares[share] || bind != advertisedBind {
						stop()
						delete(advertisements, share)
					}
				}
				if stopHostname != nil && (len(shares) == 0 || bind != advertisedBind) {
					stopHostname()
					stopHostname = nil
				}
				advertisedBind = bind
				if len(shares) > 0 && stopHostname == nil {
					stopHostname, _ = h.answerHostname(bind)
				}
				for share := range shares {
					if advertisements[share] == nil {
						if stop, err := h.advertiseShare(share, bind); err == nil {
							advertisements[share] = stop
						} else {
							for _, s := range sessions {
								s.emit(map[string]string{"discoveryError": "Network discovery is unavailable"})
							}
						}
					}
				}
				if !empty || paired {
					emptySince = time.Now()
				} else if time.Since(emptySince) > time.Second {
					mux.Close()
					return
				}
			}
		}
	}()
	slots := make(chan struct{}, 32)
	for {
		candidate, err := mux.Accept(ctx)
		if err != nil {
			return nil
		}
		if !candidate.Addr.IP.IsPrivate() && !candidate.Addr.IP.IsLinkLocalUnicast() {
			mux.RemoveConnByUfrag(candidate.LocalUfrag)
			continue
		}
		select {
		case slots <- struct{}{}:
			go func() {
				defer func() { <-slots }()
				h.accept(mux, candidate)
				mux.RemoveConnByUfrag(candidate.LocalUfrag)
			}()
		default:
			mux.RemoveConnByUfrag(candidate.LocalUfrag)
		}
	}
}
