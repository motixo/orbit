// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/google/uuid"
	"github.com/pion/webrtc/v4"
	"github.com/schollz/croc/v11/src/cli"
	"github.com/schollz/croc/v11/src/tcp"
	"github.com/schollz/croc/v11/src/utils"
)

type crocTicket struct {
	ID       string `json:"id"`
	Secret   string `json:"secret"`
	Password string `json:"password"`
	Port     string `json:"port"`
	Name     string `json:"name"`
	Size     int64  `json:"size"`
	Host     string `json:"host"`
	DataPort string `json:"data_port"`
	Verify   bool   `json:"verify"`
}

func (ticket crocTicket) valid() bool {
	secret, err := hex.DecodeString(ticket.Secret)
	password, passErr := hex.DecodeString(ticket.Password)
	port, portErr := strconv.Atoi(ticket.Port)
	dataPort, dataErr := strconv.Atoi(ticket.DataPort)
	return uuid.Validate(ticket.ID) == nil && err == nil && len(secret) == 32 && passErr == nil && len(password) == 32 &&
		portErr == nil && port > 0 && port <= 65535 && dataErr == nil && dataPort > 0 && dataPort <= 65535 && ticket.Size >= 0
}

type crocTransfer struct {
	crocTicket
	owner     string
	ctx       context.Context
	cancel    context.CancelFunc
	ports     map[string]bool
	done      chan struct{}
	err       error
	mu        sync.Mutex
	bridges   int
	direction string
	progress  int64
	sampled   time.Time
	finished  bool
	ready     atomic.Bool
	checksum  chan []byte
}

var crocPercent = regexp.MustCompile(`(?:^|\s)([0-9]{1,3})%`)

type crocOutput struct {
	s        *server
	transfer *crocTransfer
	line     string
	failure  string
}

type crocFailure struct{ message string }

func (failure *crocFailure) Error() string { return failure.message }

func (output *crocOutput) Write(data []byte) (int, error) {
	output.line += string(data)
	if len(output.line) > 4096 {
		output.line = output.line[len(output.line)-4096:]
	}
	for {
		index := strings.IndexAny(output.line, "\r\n")
		if index < 0 {
			break
		}
		if encoded, ok := strings.CutPrefix(output.line[:index], "ORBIT_CROC_ERROR "); ok {
			_ = json.Unmarshal([]byte(encoded), &output.failure)
		}
		matches := crocPercent.FindAllStringSubmatch(output.line[:index], -1)
		if len(matches) > 0 {
			percent, _ := strconv.ParseInt(matches[len(matches)-1][1], 10, 64)
			if percent <= 100 {
				size := output.transfer.Size
				output.s.crocProgress(output.transfer.ID, output.transfer.owner, size/100*percent+size%100*percent/100)
			}
		}
		output.line = output.line[index+1:]
	}
	return len(data), nil
}

func runCroc() {
	stderr := os.Stderr
	os.Args = append([]string{"croc"}, os.Args[2:]...)
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	err := cli.RunContext(ctx)
	utils.RemoveMarkedFiles()
	if err != nil {
		data, _ := json.Marshal(err.Error())
		fmt.Fprintf(stderr, "\nORBIT_CROC_ERROR %s\n", data)
		os.Exit(1)
	}
}

func randomSecret() (string, error) {
	var value [32]byte
	_, err := rand.Read(value[:])
	return hex.EncodeToString(value[:]), err
}

func (s *server) respondCroc(parent, ctx context.Context, dc *webrtc.DataChannel, command request, peer device, touch func()) error {
	var value any = struct{}{}
	if command.Action == "croc-file" || command.Action == "croc-upload" || command.Action == "croc-receive" {
		transfer, err := s.prepareCroc(parent, command, peer)
		if err != nil {
			return err
		}
		value = transfer
	} else {
		s.crocMu.Lock()
		transfer := s.croc[command.ID]
		s.crocMu.Unlock()
		if transfer == nil || transfer.owner != peer.connection {
			return errors.New("unknown transfer")
		}
		if command.Action == "croc-cancel" {
			transfer.cancel()
		} else {
			if transfer.checksum != nil {
				checksum, err := hex.DecodeString(command.Checksum)
				if err != nil || len(checksum) != 8 {
					return &crocFailure{message: "Missing upload checksum"}
				}
				select {
				case transfer.checksum <- checksum:
				default:
					return errors.New("upload checksum already supplied")
				}
			}
			for {
				select {
				case <-ctx.Done():
					return ctx.Err()
				case <-time.After(5 * time.Second):
					touch()
					if err := sendJSON(ctx, dc, map[string]string{"type": "pending"}); err != nil {
						return err
					}
				case <-transfer.done:
					goto completed
				}
			}
		completed:
			defer transfer.cancel()
			if transfer.err != nil {
				return transfer.err
			}
		}
	}
	data, err := json.Marshal(value)
	if err != nil {
		return err
	}
	if err = sendJSON(ctx, dc, map[string]any{"type": "response", "status": 200, "size": len(data)}); err != nil {
		return err
	}
	if err = send(ctx, dc, data, false); err != nil {
		return err
	}
	if err = sendJSON(ctx, dc, map[string]string{"type": "end"}); err != nil {
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

func (s *server) prepareCroc(parent context.Context, command request, peer device) (*crocTransfer, error) {
	ctx, cancel := context.WithCancel(parent)
	ok := false
	defer func() {
		if !ok {
			cancel()
		}
	}()
	transfer := &crocTransfer{crocTicket: crocTicket{ID: uuid.NewString(), Name: command.Name, Size: command.Size, Host: s.Bind},
		owner: peer.connection, ctx: ctx, cancel: cancel, done: make(chan struct{}), ports: make(map[string]bool)}
	if command.Action == "croc-upload" && !peer.native {
		transfer.Verify = true
		transfer.checksum = make(chan []byte, 1)
	}
	s.crocMu.Lock()
	count := 0
	for _, active := range s.croc {
		if active.owner == peer.connection {
			count++
		}
	}
	if count >= 3 {
		s.crocMu.Unlock()
		return nil, errors.New("too many transfers")
	}
	if s.croc == nil {
		s.croc = make(map[string]*crocTransfer)
	}
	s.croc[transfer.ID] = transfer
	s.crocMu.Unlock()
	defer func() {
		if !ok {
			s.crocMu.Lock()
			delete(s.croc, transfer.ID)
			s.crocMu.Unlock()
		}
	}()
	var err error
	transfer.Secret, err = randomSecret()
	if err != nil {
		return nil, err
	}
	transfer.Password, err = randomSecret()
	if err != nil {
		return nil, err
	}
	var directory, stageID string
	var lease io.ReadCloser
	defer func() {
		if !ok && lease != nil {
			lease.Close()
		}
	}()
	filename := ""
	path, method := "croc/staging", http.MethodPost
	if command.Action == "croc-file" {
		directory, err = os.MkdirTemp("", "orbit-croc-")
		if err != nil {
			return nil, err
		}
		defer func() {
			if !ok {
				os.RemoveAll(directory)
			}
		}()
		path, method = fmt.Sprintf("croc/files/%d", command.Index), http.MethodGet
	}
	req, _ := http.NewRequestWithContext(ctx, method, s.URL+path, nil)
	req.Header.Set("X-Orbit-Device", peer.PairID)
	req.Header.Set("X-Orbit-IP", peer.IP)
	response, err := s.client.Do(req)
	if err != nil {
		return nil, err
	}
	lease = response.Body
	if response.StatusCode != http.StatusOK {
		message, _ := io.ReadAll(io.LimitReader(lease, 256))
		return nil, &crocFailure{message: fmt.Sprintf("Could not prepare transfer (%d): %s", response.StatusCode, strings.TrimSpace(string(message)))}
	}
	var file struct {
		ID, Path, Name string
		Size           int64
	}
	if json.NewDecoder(io.LimitReader(lease, 64*1024)).Decode(&file) != nil || !filepath.IsAbs(file.Path) || file.Size < 0 {
		return nil, errors.New("invalid transfer path")
	}
	if command.Action == "croc-file" {
		filename, transfer.Name, transfer.Size = file.Path, file.Name, file.Size
	} else {
		if uuid.Validate(file.ID) != nil {
			return nil, errors.New("invalid staging lease")
		}
		directory, stageID = file.Path, file.ID
	}
	configDir, err := os.MkdirTemp("", "orbit-croc-config-")
	if err != nil {
		return nil, err
	}
	defer func() {
		if !ok {
			os.RemoveAll(configDir)
		}
	}()
	transfer.direction = "download"
	if command.Action == "croc-file" {
		transfer.direction = "upload"
	}
	listeners := make([]net.Listener, 0, 2)
	defer func() {
		if !ok {
			for _, listener := range listeners {
				listener.Close()
			}
		}
	}()
	if command.Relay == nil {
		for range 2 {
			listener, err := net.Listen("tcp4", net.JoinHostPort(s.Bind, "0"))
			if err != nil {
				return nil, err
			}
			listeners = append(listeners, listener)
			transfer.ports[strconv.Itoa(listener.Addr().(*net.TCPAddr).Port)] = true
		}
		transfer.Port = strconv.Itoa(listeners[0].Addr().(*net.TCPAddr).Port)
		dataPort := strconv.Itoa(listeners[1].Addr().(*net.TCPAddr).Port)
		transfer.DataPort = dataPort
		for _, listener := range listeners {
			go func() {
				if err := tcp.RunWithListener(listener, transfer.Password, tcp.WithCtx(ctx), tcp.WithBanner(dataPort), tcp.WithLogLevel("error")); err != nil && ctx.Err() == nil {
					cancel()
				}
			}()
		}
	} else {
		if command.Relay.Host != peer.IP {
			return nil, errors.New("invalid relay address")
		}
		transfer.Secret, transfer.Password = command.Relay.Secret, command.Relay.Password
		transfer.Host, transfer.Port, transfer.DataPort = command.Relay.Host, command.Relay.Port, command.Relay.DataPort
	}
	args := []string{"--croc", "--yes", "--ignore-stdin", "--disable-clipboard", "--relay", net.JoinHostPort(transfer.Host, transfer.Port), "--pass", transfer.Password}
	if !peer.native {
		args = append(args, "--quiet")
	}
	if command.Action == "croc-file" {
		args = append(args, "send", "--no-local", "--no-multi", "--transport", "relay", "--hash", "xxhash", "--", filename)
	}
	// Keep transfers on this core when an update replaces its executable.
	child := exec.CommandContext(ctx, "/proc/self/exe", args...)
	child.Dir = directory
	child.Env = append(os.Environ(), "CROC_SECRET="+transfer.Secret, "CROC_CONFIG_DIR="+configDir,
		"HTTP_PROXY=", "HTTPS_PROXY=", "ALL_PROXY=", "SOCKS5_PROXY=", "CROC_RELAY6=")
	child.Stdout = io.Discard
	output := &crocOutput{s: s, transfer: transfer}
	child.Stderr = output
	if err = child.Start(); err != nil {
		return nil, err
	}
	transfer.ready.Store(true)
	s.emitCrocProgress(transfer, false)
	go func() {
		if stageID == "" {
			defer os.RemoveAll(directory)
		}
		defer os.RemoveAll(configDir)
		defer func() {
			if lease != nil {
				lease.Close()
			}
		}()
		transfer.err = child.Wait()
		if transfer.err != nil && output.failure != "" {
			message := strings.ReplaceAll(output.failure, transfer.Secret, "[redacted]")
			message = strings.ReplaceAll(message, transfer.Password, "[redacted]")
			transfer.err = &crocFailure{message: fmt.Sprintf("Croc: %.200s", message)}
		}
		if transfer.err == nil && command.Action != "croc-file" {
			var checksum []byte
			if transfer.checksum != nil {
				select {
				case checksum = <-transfer.checksum:
				case <-ctx.Done():
					transfer.err = ctx.Err()
				case <-time.After(time.Minute):
					transfer.err = &crocFailure{message: "Upload verification timed out"}
				}
			}
			if transfer.err == nil {
				transfer.err = s.importCroc(ctx, directory, stageID, command, peer, checksum)
			}
		}
		if transfer.err != nil && ctx.Err() == nil {
			log.Printf("croc transfer failed: %v", transfer.err)
		}
		lease.Close()
		transfer.mu.Lock()
		transfer.finished = true
		s.emitCrocProgress(transfer, true)
		transfer.mu.Unlock()
		close(transfer.done)
		select {
		case <-ctx.Done():
		case <-time.After(60 * time.Second):
			cancel()
		}
	}()
	go func() {
		<-ctx.Done()
		for _, listener := range listeners {
			listener.Close()
		}
		s.crocMu.Lock()
		delete(s.croc, transfer.ID)
		s.crocMu.Unlock()
	}()
	ok = true
	return transfer, nil
}

func (s *server) emitCrocProgress(transfer *crocTransfer, finished bool) {
	s.emit(map[string]any{"crocProgress": map[string]any{
		"id": transfer.ID, "direction": transfer.direction, "done": transfer.progress, "total": transfer.Size, "finished": finished,
	}})
}

func (s *server) crocProgress(id, owner string, done int64) {
	s.crocMu.Lock()
	transfer := s.croc[id]
	s.crocMu.Unlock()
	if transfer == nil || transfer.owner != owner || !transfer.ready.Load() {
		return
	}
	transfer.mu.Lock()
	defer transfer.mu.Unlock()
	if transfer.finished || done < transfer.progress || done > transfer.Size || done < 0 {
		return
	}
	transfer.progress = done
	if time.Since(transfer.sampled) < 250*time.Millisecond && done < transfer.Size {
		return
	}
	transfer.sampled = time.Now()
	s.emitCrocProgress(transfer, false)
}

func (s *server) importCroc(ctx context.Context, directory, stageID string, command request, peer device, checksum []byte) error {
	entries, err := os.ReadDir(directory)
	if err != nil {
		return err
	}
	if len(entries) != 1 || !entries[0].Type().IsRegular() {
		return errors.New("incomplete transfer")
	}
	file, err := os.Open(filepath.Join(directory, entries[0].Name()))
	if err != nil {
		return err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil || info.Size() != command.Size {
		return errors.New("invalid received file size")
	}
	if checksum != nil {
		digest, err := utils.HashFileCtx(ctx, file.Name(), "xxhash")
		if err != nil {
			return err
		}
		if !bytes.Equal(digest, checksum) {
			return &crocFailure{message: "Upload checksum mismatch"}
		}
	}
	metadata, err := json.Marshal(map[string]any{"name": command.Name, "size": command.Size})
	if err != nil {
		return err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.URL+"croc/import/"+stageID, bytes.NewReader(metadata))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Orbit-Device", peer.PairID)
	req.Header.Set("X-Orbit-IP", peer.IP)
	if command.Source != "" {
		req.Header.Set("X-Orbit-Source", command.Source)
	}
	if command.Content {
		req.Header.Set("X-Orbit-Content", "true")
	}
	response, err := s.client.Do(req)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		message, _ := io.ReadAll(io.LimitReader(response.Body, 256))
		return &crocFailure{message: fmt.Sprintf("Could not add file (%d): %s", response.StatusCode, strings.TrimSpace(string(message)))}
	}
	_, err = io.Copy(io.Discard, response.Body)
	return err
}

func (s *server) crocChannel(parent context.Context, dc *webrtc.DataChannel, owner string) {
	parts := strings.Split(dc.Label(), ":")
	if len(parts) != 3 {
		dc.Close()
		return
	}
	s.crocMu.Lock()
	transfer := s.croc[parts[1]]
	s.crocMu.Unlock()
	if transfer == nil || transfer.owner != owner || !transfer.ready.Load() || !transfer.ports[parts[2]] {
		dc.Close()
		return
	}
	transfer.mu.Lock()
	if transfer.bridges >= 8 {
		transfer.mu.Unlock()
		dc.Close()
		return
	}
	transfer.bridges++
	transfer.mu.Unlock()
	ctx, cancel := context.WithCancel(transfer.ctx)
	conn, err := (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, "tcp4", net.JoinHostPort(s.Bind, parts[2]))
	if err != nil {
		cancel()
		dc.Close()
		return
	}
	dc.OnClose(cancel)
	dc.OnError(func(error) { cancel() })
	const window = 512 * 1024
	credits := make(chan int, 64)
	dc.OnMessage(func(message webrtc.DataChannelMessage) {
		if message.IsString {
			if len(message.Data) > 16 || !strings.HasPrefix(string(message.Data), "read:") {
				cancel()
				return
			}
			n, err := strconv.Atoi(string(message.Data[5:]))
			if err != nil || n <= 0 || n > window {
				cancel()
				return
			}
			select {
			case credits <- n:
			default:
				cancel()
			}
			return
		}
		if len(message.Data) > 16384 {
			cancel()
			return
		}
		if _, err := conn.Write(message.Data); err != nil {
			cancel()
		}
	})
	dc.OnOpen(func() {
		go func() {
			defer cancel()
			buffer := make([]byte, 16384)
			budget := window
			for {
				select {
				case <-ctx.Done():
					return
				case credit := <-credits:
					if credit > window-budget {
						return
					}
					budget += credit
					continue
				default:
				}
				if budget == 0 {
					select {
					case <-ctx.Done():
						return
					case credit := <-credits:
						budget += credit
					}
					continue
				}
				n, err := conn.Read(buffer[:min(len(buffer), budget)])
				budget -= n
				if n > 0 && send(ctx, dc, buffer[:n], false) != nil {
					return
				}
				if err != nil {
					for dc.BufferedAmount() > 0 && ctx.Err() == nil {
						time.Sleep(5 * time.Millisecond)
					}
					return
				}
			}
		}()
	})
	go func() {
		select {
		case <-ctx.Done():
		case <-parent.Done():
			cancel()
		}
		conn.Close()
		dc.Close()
		transfer.mu.Lock()
		transfer.bridges--
		transfer.mu.Unlock()
	}()
}

func (l *desktopLink) crocChannel(dc *webrtc.DataChannel) {
	if !l.ready.Load() {
		dc.Close()
		return
	}
	l.mu.Lock()
	s := l.local
	l.mu.Unlock()
	s.crocChannel(l.ctx, dc, l.nonce)
}
