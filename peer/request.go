// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/google/uuid"
	"github.com/pion/webrtc/v4"
)

type request struct {
	Action   string
	Index    uint32
	Name     string
	Size     int64
	Source   string
	Content  bool
	ID       string
	Relay    *crocTicket
	Checksum string
}

func (r request) valid() bool {
	if r.Checksum != "" {
		if r.Action != "croc-result" || len(r.Checksum) != 16 {
			return false
		}
		checksum, err := hex.DecodeString(r.Checksum)
		if err != nil || len(checksum) != 8 {
			return false
		}
	}
	if r.Relay != nil && r.Action != "croc-receive" {
		return false
	}
	if r.Source != "" {
		share, index, ok := strings.Cut(r.Source, "/")
		if !ok || uuid.Validate(share) != nil {
			return false
		}
		if _, err := strconv.ParseUint(index, 10, 32); err != nil {
			return false
		}
	}
	if r.Content && ((r.Action != "croc-upload" && r.Action != "croc-receive") || r.Source == "" || r.Size > 1024*1024) {
		return false
	}
	switch r.Action {
	case "list", "remove", "thumbnail", "croc-file":
		return r.Size == 0
	case "received":
		return r.Size == 0 && r.Source != ""
	case "croc-result", "croc-cancel":
		return r.Size == 0 && uuid.Validate(r.ID) == nil
	case "croc-receive":
		if r.Relay == nil || !r.Relay.valid() || r.Relay.Size != r.Size || r.Relay.Name != r.Name {
			return false
		}
		r.Action, r.Relay = "croc-upload", nil
		return r.valid()
	case "text":
		return r.Size > 0 && r.Size <= 1024*1024
	case "croc-upload":
		return r.Size >= 0 && r.Name != "" && r.Name != "." &&
			r.Name != ".." && !strings.HasPrefix(r.Name, ".lease") && len(r.Name) <= 255 &&
			!strings.ContainsAny(r.Name, "/\\") && strings.IndexFunc(r.Name, unicode.IsControl) < 0
	}
	return false
}

func send(ctx context.Context, dc *webrtc.DataChannel, message []byte, text bool) error {
	for dc.BufferedAmount() > 256*1024 {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(5 * time.Millisecond):
		}
	}
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if text {
		return dc.SendText(string(message))
	}
	return dc.Send(message)
}

func sendJSON(ctx context.Context, dc *webrtc.DataChannel, value any) error {
	data, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return send(ctx, dc, data, true)
}

func (s *server) request(parent context.Context, dc *webrtc.DataChannel, peer device, release func()) {
	ctx, cancel := context.WithCancel(parent)
	reader, writer := io.Pipe()
	var closeOnce sync.Once
	closeRequest := func() {
		closeOnce.Do(func() {
			cancel()
			reader.CloseWithError(context.Canceled)
			writer.CloseWithError(context.Canceled)
			release()
		})
	}
	dc.OnClose(closeRequest)
	dc.OnError(func(error) { closeRequest() })
	first := true
	var command request
	var received int64
	deadline := time.AfterFunc(60*time.Second, func() { closeRequest(); dc.Close() })
	dc.OnMessage(func(message webrtc.DataChannelMessage) {
		if ctx.Err() != nil {
			return
		}
		deadline.Reset(60 * time.Second)
		if first {
			first = false
			if !message.IsString || len(message.Data) > 4096 ||
				json.Unmarshal(message.Data, &command) != nil || !command.valid() {
				closeRequest()
				dc.Close()
				return
			}
			go func() {
				defer deadline.Stop()
				defer closeRequest()
				defer dc.Close()
				var err error
				if strings.HasPrefix(command.Action, "croc-") {
					err = s.respondCroc(parent, ctx, dc, command, peer, func() { deadline.Reset(60 * time.Second) })
				} else {
					err = s.respond(ctx, dc, command, peer, reader, func() { deadline.Reset(60 * time.Second) })
				}
				if err != nil && ctx.Err() == nil {
					message := "Transfer failed"
					var failure *crocFailure
					if errors.As(err, &failure) {
						message = failure.Error()
					}
					sendJSON(ctx, dc, map[string]any{"type": "error", "message": message})
				}
			}()
			return
		}
		if message.IsString {
			if string(message.Data) != "end" || received != command.Size {
				closeRequest()
				dc.Close()
				return
			}
			writer.Close()
			return
		}
		if command.Action != "text" || len(message.Data) > 16384 ||
			int64(len(message.Data)) > command.Size-received {
			closeRequest()
			dc.Close()
			return
		}
		received += int64(len(message.Data))
		if _, err := writer.Write(message.Data); err != nil {
			closeRequest()
		}
	})
	go func() {
		<-ctx.Done()
		deadline.Stop()
		reader.CloseWithError(ctx.Err())
		writer.CloseWithError(ctx.Err())
	}()
}

func (s *server) respond(ctx context.Context, dc *webrtc.DataChannel, command request, peer device, input io.Reader, touch func()) error {
	path, method := "items", http.MethodGet
	var body io.Reader
	headers := make(http.Header)
	headers.Set("X-Orbit-Device", peer.PairID)
	headers.Set("X-Orbit-IP", peer.IP)
	switch command.Action {
	case "received":
		path = "received/" + command.Source
	case "remove":
		path, method = fmt.Sprintf("files/%d", command.Index), http.MethodDelete
	case "thumbnail":
		path = fmt.Sprintf("thumbnails/%d", command.Index)
	case "text":
		text, err := io.ReadAll(io.LimitReader(input, 1024*1024+1))
		if err != nil || int64(len(text)) != command.Size || !utf8.Valid(text) {
			return errors.New("invalid text")
		}
		path, method, body = "text", http.MethodPost, bytes.NewReader(text)
		headers.Set("Content-Type", "text/plain; charset=utf-8")
	}
	req, err := http.NewRequestWithContext(ctx, method, s.URL+path, body)
	if err != nil {
		return err
	}
	req.Header = headers
	resp, err := s.client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if err := sendJSON(ctx, dc, map[string]any{"type": "response", "status": resp.StatusCode, "size": resp.ContentLength}); err != nil {
		return err
	}
	buffer := make([]byte, 16384)
	var total int64
	for {
		n, err := resp.Body.Read(buffer)
		total += int64(n)
		if resp.ContentLength >= 0 && total > resp.ContentLength {
			return errors.New("oversized response")
		}
		if n > 0 {
			if err := send(ctx, dc, buffer[:n], false); err != nil {
				return err
			}
			touch()
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return err
		}
	}
	if resp.ContentLength >= 0 && total != resp.ContentLength {
		return errors.New("incomplete response")
	}
	if err := sendJSON(ctx, dc, map[string]any{"type": "end"}); err != nil {
		return err
	}
	for dc.BufferedAmount() > 0 {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(5 * time.Millisecond):
		}
	}
	return nil
}
