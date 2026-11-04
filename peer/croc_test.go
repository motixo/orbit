// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"

	"github.com/pion/webrtc/v4"
)

func TestFailedCrocBridgeDoesNotUseUpAConnectionSlot(t *testing.T) {
	const port = "1"
	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatal(err)
	}
	defer pc.Close()
	dc, err := pc.CreateDataChannel("croc:transfer:"+port, nil)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	transfer := &crocTransfer{crocTicket: crocTicket{Host: "127.0.0.1"}, ctx: ctx, owner: "device", ports: map[string]bool{port: true}}
	transfer.ready.Store(true)
	s := &server{croc: map[string]*crocTransfer{"transfer": transfer}}
	s.crocChannel(context.Background(), dc, "device")
	if transfer.bridges != 0 {
		t.Fatal("a cancelled bridge occupied a connection slot")
	}
}

func TestCrocVerifiesUploadsBeforeImport(t *testing.T) {
	for _, test := range []struct {
		name, data, digest string
		accept             bool
	}{
		{"complete", "hello", "26c7827d889f6da3", true},
		{"empty", "", "ef46db3751d8e999", true},
		{"corrupt", "jello", "26c7827d889f6da3", false},
	} {
		t.Run(test.name, func(t *testing.T) {
			directory := t.TempDir()
			if err := os.WriteFile(filepath.Join(directory, "upload.txt"), []byte(test.data), 0600); err != nil {
				t.Fatal(err)
			}
			stageID := "12345678-1234-4123-8123-123456789abc"
			imported := make(chan string, 1)
			endpoint := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				var file struct {
					Name string
					Size int64
				}
				if r.URL.Path != "/croc/import/"+stageID || json.NewDecoder(r.Body).Decode(&file) != nil || file.Name != "upload.txt" || file.Size != int64(len(test.data)) {
					http.Error(w, "invalid import", http.StatusBadRequest)
					return
				}
				data, err := os.ReadFile(filepath.Join(directory, file.Name))
				if err != nil {
					http.Error(w, "incomplete transfer", http.StatusBadRequest)
					return
				}
				imported <- string(data)
			}))
			defer endpoint.Close()
			checksum, err := hex.DecodeString(test.digest)
			if err != nil {
				t.Fatal(err)
			}
			s := &server{URL: endpoint.URL + "/", client: endpoint.Client()}
			err = s.importCroc(context.Background(), directory, stageID, request{Name: "upload.txt", Size: int64(len(test.data))}, device{}, checksum)
			if (err == nil) != test.accept {
				t.Fatalf("accept = %v, import error = %v", test.accept, err)
			}
			select {
			case data := <-imported:
				if !test.accept || data != test.data {
					t.Fatalf("unexpected imported data: %q", data)
				}
			default:
				if test.accept {
					t.Fatal("verified upload was not imported")
				}
			}
		})
	}
}
