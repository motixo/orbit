// SPDX-License-Identifier: GPL-3.0-only

package main

import (
	"testing"
	"time"
)

func TestPublishRetriesBackOffAndStop(t *testing.T) {
	now := time.Unix(1, 0)
	retry := publishRetry{}
	for _, delay := range []time.Duration{2 * time.Second, 4 * time.Second} {
		if !retry.ready(now) {
			t.Fatal("retry was not ready")
		}
		retry.failed(now)
		if retry.ready(now.Add(delay - time.Nanosecond)) {
			t.Fatal("retry ignored backoff")
		}
		now = now.Add(delay)
	}
	if !retry.ready(now) {
		t.Fatal("last attempt was skipped")
	}
	retry.failed(now)
	if retry.ready(now.Add(24 * time.Hour)) {
		t.Fatal("exhausted retry restarted")
	}
}
