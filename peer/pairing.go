// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"math/big"
	"net"
	"os"
	"path/filepath"
	"sort"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/pion/webrtc/v4"
)

type pairedDevice struct {
	AllowOpen   bool   `json:"allow_open"`
	ID          string `json:"id"`
	Name        string `json:"name"`
	Hash        string `json:"hash,omitempty"`
	Fingerprint string `json:"fingerprint,omitempty"`
	Desktop     bool   `json:"desktop,omitempty"`
}

type identity struct {
	PWA         string                  `json:"pwa"`
	Certificate string                  `json:"certificate"`
	Host        string                  `json:"host"`
	Port        int                     `json:"port"`
	Devices     map[string]pairedDevice `json:"devices"`
}

type pairStore struct {
	sync.Mutex
	identity
	path string
}

func loadIdentity(directory string) (*pairStore, *webrtc.Certificate, error) {
	if err := os.MkdirAll(directory, 0700); err != nil {
		return nil, nil, err
	}
	if err := os.Chmod(directory, 0700); err != nil {
		return nil, nil, err
	}
	s := &pairStore{path: filepath.Join(directory, "identity.json")}
	data, err := os.ReadFile(s.path)
	if err == nil {
		if len(data) > 65536 || json.Unmarshal(data, &s.identity) != nil ||
			s.Port < 0 || s.Port > 65535 || len(s.Devices) > 64 || s.PWA != "" && !validPWA(s.PWA) {
			return nil, nil, errors.New("invalid pairing identity")
		}
		if _, err := uuid.Parse(s.Host); err != nil {
			return nil, nil, errors.New("invalid pairing hostname")
		}
		cert, err := webrtc.CertificateFromPEM(s.Certificate)
		if err != nil || !cert.Expires().After(time.Now()) {
			return nil, nil, errors.New("invalid pairing certificate")
		}
		if s.Devices == nil {
			s.Devices = make(map[string]pairedDevice)
		}
		for id, device := range s.Devices {
			key, err := hex.DecodeString(device.Hash)
			validKey := err == nil && len(key) == 32
			if device.Fingerprint != "" {
				validKey = fingerprintHex(device.Fingerprint) == device.Fingerprint && device.Hash == ""
			}
			if _, idErr := uuid.Parse(id); idErr != nil || device.ID != id || !validKey {
				return nil, nil, errors.New("invalid paired device")
			}
			device.Name = cleanDeviceName(device.Name)
			s.Devices[id] = device
		}
		return s, cert, nil
	}
	if !os.IsNotExist(err) {
		return nil, nil, err
	}
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, nil, err
	}
	serial, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	if err != nil {
		return nil, nil, err
	}
	cert, err := webrtc.NewCertificate(key, x509.Certificate{
		SerialNumber: serial,
		NotBefore:    time.Now().Add(-24 * time.Hour),
		NotAfter:     time.Now().AddDate(10, 0, 0),
	})
	if err != nil {
		return nil, nil, err
	}
	s.Certificate, err = cert.PEM()
	if err != nil {
		return nil, nil, err
	}
	s.Host, s.Devices = uuid.NewString(), make(map[string]pairedDevice)
	if err := s.save(); err != nil {
		return nil, nil, err
	}
	return s, cert, nil
}

func (s *pairStore) save() error {
	data, err := json.Marshal(s.identity)
	if err != nil {
		return err
	}
	file, err := os.CreateTemp(filepath.Dir(s.path), ".identity-")
	if err != nil {
		return err
	}
	defer os.Remove(file.Name())
	if _, err = file.Write(data); err == nil {
		err = file.Sync()
	}
	if closeErr := file.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	if err := os.Rename(file.Name(), s.path); err != nil {
		return err
	}
	directory, err := os.Open(filepath.Dir(s.path))
	if err != nil {
		return err
	}
	defer directory.Close()
	return directory.Sync()
}

func (s *pairStore) setPWA(pwa string) (bool, error) {
	if !validPWA(pwa) {
		return false, errors.New("invalid PWA address")
	}
	s.Lock()
	defer s.Unlock()
	if s.PWA == pwa {
		return false, nil
	}
	previous, devices := s.PWA, s.Devices
	s.PWA, s.Devices = pwa, make(map[string]pairedDevice)
	if err := s.save(); err != nil {
		s.PWA, s.Devices = previous, devices
		return false, err
	}
	return true, nil
}

func (s *pairStore) matchesPWA(pwa string) bool {
	s.Lock()
	defer s.Unlock()
	return s.PWA == pwa
}

func (s *pairStore) contains(id string) bool {
	s.Lock()
	defer s.Unlock()
	_, found := s.Devices[id]
	return found
}

func (s *pairStore) verify(id, challenge, signature string) bool {
	s.Lock()
	device, found := s.Devices[id]
	s.Unlock()
	if !found {
		return false
	}
	key, err := hex.DecodeString(device.Hash)
	proof, proofErr := base64.RawURLEncoding.DecodeString(signature)
	if err != nil || proofErr != nil || len(key) != 32 || len(proof) != 32 {
		return false
	}
	mac := hmac.New(sha256.New, key)
	mac.Write([]byte(challenge))
	return hmac.Equal(mac.Sum(nil), proof)
}

func (s *pairStore) pair(name, pwa string) (string, string, error) {
	s.Lock()
	defer s.Unlock()
	if s.PWA != pwa {
		return "", "", errors.New("PWA address changed")
	}
	if len(s.Devices) >= 64 {
		return "", "", errors.New("paired device limit reached")
	}
	var key [32]byte
	if _, err := rand.Read(key[:]); err != nil {
		return "", "", err
	}
	id, token := uuid.NewString(), hex.EncodeToString(key[:])
	hash := sha256.Sum256([]byte(token))
	s.Devices[id] = pairedDevice{ID: id, Name: cleanDeviceName(name), Hash: hex.EncodeToString(hash[:])}
	if err := s.save(); err != nil {
		delete(s.Devices, id)
		return "", "", err
	}
	return id, token, nil
}

func (s *pairStore) rename(id, name string) error {
	s.Lock()
	defer s.Unlock()
	name = cleanDeviceName(name)
	if name == "" {
		return errors.New("empty device name")
	}
	device, ok := s.Devices[id]
	if !ok {
		return errors.New("device not paired")
	}
	updated := device
	updated.Name = name
	s.Devices[id] = updated
	if err := s.save(); err != nil {
		s.Devices[id] = device
		return err
	}
	return nil
}

func (s *pairStore) rememberBrowserName(id, name string) (bool, error) {
	s.Lock()
	defer s.Unlock()
	name = cleanDeviceName(name)
	device, ok := s.Devices[id]
	if !ok || device.Desktop || name == "" || device.Name != "" && net.ParseIP(device.Name) == nil {
		return false, nil
	}
	updated := device
	updated.Name = name
	s.Devices[id] = updated
	if err := s.save(); err != nil {
		s.Devices[id] = device
		return false, err
	}
	return true, nil
}

func (s *pairStore) forget(id string) error {
	s.Lock()
	defer s.Unlock()
	device, ok := s.Devices[id]
	if !ok {
		return nil
	}
	delete(s.Devices, id)
	if err := s.save(); err != nil {
		s.Devices[id] = device
		return err
	}
	return nil
}

func (s *pairStore) setAllowOpen(id string, allowed bool) (bool, error) {
	s.Lock()
	defer s.Unlock()
	previous, ok := s.Devices[id]
	if !ok {
		return false, errors.New("device not paired")
	}
	if previous.AllowOpen == allowed {
		return false, nil
	}
	updated := previous
	updated.AllowOpen = allowed
	s.Devices[id] = updated
	if err := s.save(); err != nil {
		s.Devices[id] = previous
		return false, err
	}
	return true, nil
}

func (s *pairStore) canOpen(id string) bool {
	s.Lock()
	defer s.Unlock()
	device, ok := s.Devices[id]
	return ok && device.AllowOpen
}

func (s *pairStore) hasPairings() bool {
	s.Lock()
	defer s.Unlock()
	return len(s.Devices) != 0
}

func (s *pairStore) devices() []pairedDevice {
	s.Lock()
	defer s.Unlock()
	devices := make([]pairedDevice, 0, len(s.Devices))
	for _, device := range s.Devices {
		device.Hash = ""
		device.Desktop = device.Fingerprint != ""
		devices = append(devices, device)
	}
	sort.Slice(devices, func(i, j int) bool { return devices[i].ID < devices[j].ID })
	return devices
}

func (s *pairStore) trustsDesktop(id, fingerprint string) bool {
	s.Lock()
	defer s.Unlock()
	device, ok := s.Devices[id]
	return ok && device.Fingerprint != "" && device.Fingerprint == fingerprintHex(fingerprint)
}

func (s *pairStore) rememberDesktop(id, name, fingerprint string) error {
	if uuid.Validate(id) != nil || fingerprintHex(fingerprint) == "" {
		return errors.New("invalid desktop identity")
	}
	s.Lock()
	defer s.Unlock()
	previous, exists := s.Devices[id]
	if !exists && len(s.Devices) >= 64 {
		return errors.New("paired device limit reached")
	}
	if exists && previous.Fingerprint == "" {
		return errors.New("device identity conflict")
	}
	if exists {
		name = previous.Name
	}
	sameIdentity := exists && previous.Fingerprint == fingerprintHex(fingerprint)
	s.Devices[id] = pairedDevice{ID: id, Name: cleanDeviceName(name), Fingerprint: fingerprintHex(fingerprint), Desktop: true,
		AllowOpen: sameIdentity && previous.AllowOpen}
	if err := s.save(); err != nil {
		if exists {
			s.Devices[id] = previous
		} else {
			delete(s.Devices, id)
		}
		return err
	}
	return nil
}
