// SPDX-License-Identifier: GPL-3.0-only

package main

import "testing"

func TestRequestsStayInsideTheShare(t *testing.T) {
	for _, command := range []request{
		{Action: "http://elsewhere"},
		{Action: "allowOpen"},
		{Action: "openOrbit"},
		{Action: "received"},
		{Action: "received", Source: "../7"},
		{Action: "received", Source: "12345678-1234-4123-8123-123456789abc/7", Size: 1},
		{Action: "../files"},
		{Action: "file", Index: 12},
		{Action: "upload", Name: "photo.jpg", Size: 1},
		{Action: "croc-upload", Name: "../photo.jpg", Size: 1},
		{Action: "croc-upload", Name: "photo\r\n.jpg", Size: 1},
		{Action: "croc-upload", Name: "photo.jpg", Size: -1},
		{Action: "text", Size: 1024*1024 + 1},
		{Action: "remove", Index: 12, Size: 1},
		{Action: "thumbnail", Index: 12, Size: 1},
		{Action: "croc-result", ID: "12345678-1234-4123-8123-123456789abc", Checksum: "invalid"},
		{Action: "croc-result", ID: "12345678-1234-4123-8123-123456789abc", Checksum: "gggggggggggggggg"},
		{Action: "list", Checksum: "ef46db3751d8e999"},
	} {
		if command.valid() {
			t.Fatalf("accepted invalid request: %+v", command)
		}
	}
	for _, command := range []request{
		{Action: "list"},
		{Action: "received", Source: "12345678-1234-4123-8123-123456789abc/7"},
		{Action: "croc-file", Index: 12},
		{Action: "remove", Index: 12},
		{Action: "thumbnail", Index: 12},
		{Action: "croc-upload", Name: "photo.jpg", Size: 0},
		{Action: "croc-upload", Name: "تصویر.jpg", Size: 4 * 1024 * 1024 * 1024},
		{Action: "text", Size: 1},
		{Action: "croc-result", ID: "12345678-1234-4123-8123-123456789abc", Checksum: "ef46db3751d8e999"},
	} {
		if !command.valid() {
			t.Fatalf("rejected valid request: %+v", command)
		}
	}
}

func TestNativeUploadsValidateTheirSourceAndContent(t *testing.T) {
	source := "12345678-1234-4123-8123-123456789abc/7"
	valid := request{Action: "croc-upload", Name: "note.html", Size: 100, Source: source, Content: true}
	if !valid.valid() {
		t.Fatal("valid native content upload rejected")
	}
	for _, source := range []string{"invalid/7", "12345678-1234-4123-8123-123456789abc/4294967296", "12345678-1234-4123-8123-123456789abc/-1", "12345678-1234-4123-8123-123456789abc/7/extra"} {
		invalid := valid
		invalid.Source = source
		if invalid.valid() {
			t.Fatalf("accepted invalid native source %q", source)
		}
	}
	valid.Size = 1024*1024 + 1
	if valid.valid() {
		t.Fatal("oversized inline text accepted")
	}
	valid.Content = false
	if !valid.valid() {
		t.Fatal("regular native file upload was limited")
	}
}
