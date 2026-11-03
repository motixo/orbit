// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/google/uuid"
)

type openRequest struct {
	device  string
	ready   chan *server
	claimed bool
}

func (h *hub) openOrbit(ctx context.Context, device string) (*server, error) {
	if !h.store.canOpen(device) {
		return nil, errors.New("opening Orbits is not allowed")
	}
	if !filepath.IsAbs(h.opener) {
		return nil, errors.New("Orbit is unavailable")
	}
	h.mu.Lock()
	if h.openings == nil {
		h.openings = make(map[string]*openRequest)
	}
	if h.lastOpen == nil {
		h.lastOpen = make(map[string]time.Time)
	}
	for _, pending := range h.openings {
		if pending.device == device {
			h.mu.Unlock()
			return nil, errors.New("Orbit is already opening")
		}
	}
	if time.Since(h.lastOpen[device]) < 3*time.Second || len(h.sessions)+len(h.openings) >= 32 {
		h.mu.Unlock()
		return nil, errors.New("try again shortly")
	}
	id := uuid.NewString()
	pending := &openRequest{device: device, ready: make(chan *server, 1)}
	h.openings[id] = pending
	h.lastOpen[device] = time.Now()
	h.mu.Unlock()
	defer func() { h.mu.Lock(); delete(h.openings, id); h.mu.Unlock() }()
	if ctx.Err() != nil || !h.store.canOpen(device) {
		return nil, errors.New("opening Orbit canceled")
	}
	child := exec.Command(h.opener, "--remote-share", id)
	for _, value := range os.Environ() {
		key, _, _ := strings.Cut(value, "=")
		if key != "WAYLAND_SOCKET" && key != "ORBIT_START_PINNED" && key != "XDG_ACTIVATION_TOKEN" && key != "DESKTOP_STARTUP_ID" && key != "COSMIC_SINGLE_INSTANCE" {
			child.Env = append(child.Env, value)
		}
	}
	child.Env = append(child.Env, "COSMIC_SINGLE_INSTANCE=0")
	child.Stderr = os.Stderr
	if err := child.Start(); err != nil {
		return nil, err
	}
	exited := make(chan struct{})
	go func() {
		child.Wait()
		close(exited)
	}()
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-exited:
		return nil, errors.New("Orbit did not open")
	case <-time.After(15 * time.Second):
		return nil, errors.New("Orbit did not open")
	case s := <-pending.ready:
		if !h.store.canOpen(device) {
			return nil, errors.New("opening Orbits is not allowed")
		}
		return s, nil
	}
}

func (h *hub) completeOpen(id string, s *server) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	pending := h.openings[id]
	if pending == nil || pending.claimed || !h.store.canOpen(pending.device) {
		return false
	}
	pending.claimed = true
	pending.ready <- s
	return true
}
