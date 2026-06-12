package store

import (
	"context"
	"errors"
	"testing"
	"time"
)

// newManager opens a temp store and returns a started LockManager plus a cancel
// to stop its reaper.
func newManager(t *testing.T) *LockManager {
	t.Helper()
	s, _ := openTemp(t)
	m := NewLockManager(s)
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	m.Start(ctx)
	return m
}

func TestLock_AcquireFreeThenHeld(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	res, err := m.Lock(ctx, "res", "agentA", 30, 0, "")
	if err != nil {
		t.Fatalf("Lock A: %v", err)
	}
	if !res.Locked {
		t.Fatalf("agentA should acquire free lock")
	}
	if res.LockToken == "" {
		t.Fatalf("expected non-empty lock token")
	}
	if res.ExpiresInSeconds <= 0 {
		t.Fatalf("expected positive ExpiresInSeconds, got %d", res.ExpiresInSeconds)
	}

	res2, err := m.Lock(ctx, "res", "agentB", 30, 0, "")
	if err != nil {
		t.Fatalf("Lock B: %v", err)
	}
	if res2.Locked {
		t.Fatalf("agentB should NOT acquire a held lock")
	}
	if res2.HeldBy != "agentA" {
		t.Fatalf("HeldBy = %q, want agentA", res2.HeldBy)
	}
	if res2.WakeToken != "" {
		t.Fatalf("non-blocking attempt should not get a wake token, got %q", res2.WakeToken)
	}
}

func TestUnlock_TokenAndAgentCompat(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	res, _ := m.Lock(ctx, "r", "agentA", 30, 0, "")
	tok := res.LockToken

	// Wrong token: no-op.
	released, err := m.Unlock("r", "wrong-token", "agentB")
	if err != nil {
		t.Fatalf("unlock wrong token err: %v", err)
	}
	if released {
		t.Fatalf("wrong token should NOT release")
	}
	if locks, _ := m.ListLocks(); len(locks) != 1 {
		t.Fatalf("lock should still be held after wrong-token unlock")
	}

	// Correct token: releases.
	released, err = m.Unlock("r", tok, "")
	if err != nil {
		t.Fatalf("unlock correct token err: %v", err)
	}
	if !released {
		t.Fatalf("correct token should release")
	}

	// v1-compat: unlock by matching agent_id with empty token.
	res2, _ := m.Lock(ctx, "r2", "agentX", 30, 0, "")
	_ = res2
	// Non-owner agent_id is a no-op.
	released, _ = m.Unlock("r2", "", "agentY")
	if released {
		t.Fatalf("non-owner agent_id unlock should be a no-op")
	}
	// Owner agent_id with empty token releases.
	released, _ = m.Unlock("r2", "", "agentX")
	if !released {
		t.Fatalf("owner agent_id (v1 compat) should release")
	}
}

func TestRenew_TokenRequired(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	res, _ := m.Lock(ctx, "r", "agentA", 2, 0, "")
	tok := res.LockToken

	// Wrong token.
	if _, err := m.Renew("r", "nope", 30); !errors.Is(err, ErrNotOwned) {
		t.Fatalf("renew wrong token err = %v, want ErrNotOwned", err)
	}

	// Correct token extends expiry.
	rr, err := m.Renew("r", tok, 30)
	if err != nil {
		t.Fatalf("renew correct token: %v", err)
	}
	if !rr.Locked || rr.LockToken != tok {
		t.Fatalf("renew should return Locked with same token")
	}
	if rr.ExpiresInSeconds < 25 {
		t.Fatalf("renew should extend expiry, got %d", rr.ExpiresInSeconds)
	}

	// Renew nonexistent.
	if _, err := m.Renew("ghost", tok, 30); !errors.Is(err, ErrNotFound) {
		t.Fatalf("renew ghost err = %v, want ErrNotFound", err)
	}

	// Bad ttl.
	if _, err := m.Renew("r", tok, 0); !errors.Is(err, ErrInvalidTTL) {
		t.Fatalf("renew ttl=0 err = %v, want ErrInvalidTTL", err)
	}
}

func TestLock_TTLExpiry(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	if _, err := m.Lock(ctx, "r", "agentA", 1, 0, ""); err != nil {
		t.Fatalf("Lock A: %v", err)
	}
	// Held immediately.
	if r, _ := m.Lock(ctx, "r", "agentB", 30, 0, ""); r.Locked {
		t.Fatalf("agentB should not acquire while A's lock is live")
	}

	// Wait past the 1s TTL (+ slack for whole-second rounding).
	time.Sleep(2100 * time.Millisecond)

	r, err := m.Lock(ctx, "r", "agentB", 30, 0, "")
	if err != nil {
		t.Fatalf("Lock B after expiry: %v", err)
	}
	if !r.Locked {
		t.Fatalf("agentB should acquire after A's lock expires")
	}
}

func TestLock_InvalidTTL(t *testing.T) {
	m := newManager(t)
	if _, err := m.Lock(context.Background(), "r", "a", 0, 0, ""); !errors.Is(err, ErrInvalidTTL) {
		t.Fatalf("ttl=0 err = %v, want ErrInvalidTTL", err)
	}
}

func TestLock_BlockingWakesOnRelease(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	res, _ := m.Lock(ctx, "r", "agentA", 30, 0, "")
	tok := res.LockToken

	done := make(chan LockResult, 1)
	go func() {
		r, err := m.Lock(ctx, "r", "agentB", 30, 3, "")
		if err != nil {
			t.Errorf("B Lock: %v", err)
		}
		done <- r
	}()

	// Give B time to enqueue, then release A.
	time.Sleep(200 * time.Millisecond)
	if rel, _ := m.Unlock("r", tok, ""); !rel {
		t.Fatalf("A unlock should release")
	}

	select {
	case r := <-done:
		if !r.Locked {
			t.Fatalf("B should acquire after release, got %+v", r)
		}
		if r.HeldBy != "" {
			t.Fatalf("B acquired, HeldBy should be empty")
		}
	case <-time.After(2 * time.Second):
		t.Fatalf("B did not wake within 2s of release")
	}
}

func TestLock_FIFOFairness(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	res, _ := m.Lock(ctx, "r", "agentA", 30, 0, "")
	tok := res.LockToken

	bGot := make(chan LockResult, 1)
	cGot := make(chan LockResult, 1)

	// B enqueues first.
	go func() {
		r, _ := m.Lock(ctx, "r", "agentB", 30, 5, "")
		bGot <- r
	}()
	time.Sleep(150 * time.Millisecond)
	// C enqueues behind B.
	go func() {
		r, _ := m.Lock(ctx, "r", "agentC", 30, 5, "")
		cGot <- r
	}()
	time.Sleep(150 * time.Millisecond)

	// Release A → B (head) should win, not C.
	m.Unlock("r", tok, "")

	select {
	case r := <-bGot:
		if !r.Locked {
			t.Fatalf("B (FIFO head) should acquire first, got %+v", r)
		}
	case <-time.After(2 * time.Second):
		t.Fatalf("B did not acquire after release")
	}

	// C must still be blocked (B holds now).
	select {
	case r := <-cGot:
		t.Fatalf("C should still be waiting, got %+v", r)
	case <-time.After(300 * time.Millisecond):
		// expected
	}
}

func TestLock_IdempotentRePollRetainsSlot(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	res, _ := m.Lock(ctx, "r", "agentA", 30, 0, "")
	tok := res.LockToken

	// B blocks briefly while A holds → times out, gets wake_token, pos 1.
	bRes, err := m.Lock(ctx, "r", "agentB", 30, 1, "")
	if err != nil {
		t.Fatalf("B initial: %v", err)
	}
	if bRes.Locked {
		t.Fatalf("B should not acquire while A holds")
	}
	if bRes.WakeToken == "" {
		t.Fatalf("B should get a wake token")
	}
	if bRes.QueuePosition != 1 {
		t.Fatalf("B QueuePosition = %d, want 1", bRes.QueuePosition)
	}
	bWake := bRes.WakeToken

	// C enqueues behind B (blocks in a goroutine).
	cGot := make(chan LockResult, 1)
	go func() {
		r, _ := m.Lock(ctx, "r", "agentC", 30, 5, "")
		cGot <- r
	}()
	time.Sleep(200 * time.Millisecond)

	// B re-polls with its wake token → must STILL be position 1 (not behind C).
	bRes2, err := m.Lock(ctx, "r", "agentB", 30, 1, bWake)
	if err != nil {
		t.Fatalf("B re-poll: %v", err)
	}
	if bRes2.Locked {
		t.Fatalf("B should still not acquire (A holds)")
	}
	if bRes2.QueuePosition != 1 {
		t.Fatalf("B re-poll QueuePosition = %d, want 1 (kept place ahead of C)", bRes2.QueuePosition)
	}
	if bRes2.WakeToken != bWake {
		t.Fatalf("re-poll should reuse same wake token")
	}

	// Release A → B should win, not C.
	m.Unlock("r", tok, "")
	bWin := make(chan LockResult, 1)
	go func() {
		r, _ := m.Lock(ctx, "r", "agentB", 30, 5, bWake)
		bWin <- r
	}()

	select {
	case r := <-bWin:
		if !r.Locked {
			t.Fatalf("B should win after release, got %+v", r)
		}
	case <-time.After(2 * time.Second):
		t.Fatalf("B did not win after release")
	}

	// C still blocked.
	select {
	case r := <-cGot:
		t.Fatalf("C should still be waiting after B wins, got %+v", r)
	case <-time.After(300 * time.Millisecond):
	}
}

func TestLockMany_AllOrNothing(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	// Pre-hold one of two names.
	if _, err := m.Lock(ctx, "alpha", "agentA", 30, 0, ""); err != nil {
		t.Fatalf("pre-hold: %v", err)
	}

	acquired, tokens, heldBy, err := m.LockMany(ctx, []string{"beta", "alpha"}, "agentB", 30, 0)
	if err != nil {
		t.Fatalf("LockMany: %v", err)
	}
	if acquired {
		t.Fatalf("LockMany should fail (alpha held)")
	}
	if heldBy != "agentA" {
		t.Fatalf("heldBy = %q, want agentA", heldBy)
	}
	if tokens != nil {
		t.Fatalf("no tokens on failure")
	}

	// beta must NOT have been acquired.
	locks, _ := m.ListLocks()
	if len(locks) != 1 || locks[0].Name != "alpha" {
		t.Fatalf("only alpha should be held, got %+v", locks)
	}

	// Release alpha, then LockMany both.
	// (Unlock by agent compat.)
	if rel, _ := m.Unlock("alpha", "", "agentA"); !rel {
		t.Fatalf("release alpha")
	}

	acquired, tokens, _, err = m.LockMany(ctx, []string{"beta", "alpha"}, "agentB", 30, 0)
	if err != nil {
		t.Fatalf("LockMany 2: %v", err)
	}
	if !acquired {
		t.Fatalf("LockMany should succeed once alpha free")
	}
	if len(tokens) != 2 || tokens["alpha"] == "" || tokens["beta"] == "" {
		t.Fatalf("expected tokens for both, got %+v", tokens)
	}
	locks, _ = m.ListLocks()
	if len(locks) != 2 {
		t.Fatalf("both should be held, got %+v", locks)
	}
}

func TestReleaseAgentLocks(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	m.Lock(ctx, "x", "agentA", 30, 0, "")
	m.Lock(ctx, "y", "agentA", 30, 0, "")
	m.Lock(ctx, "z", "agentOther", 30, 0, "")

	// B waits on x.
	bGot := make(chan LockResult, 1)
	go func() {
		r, _ := m.Lock(ctx, "x", "agentB", 30, 5, "")
		bGot <- r
	}()
	time.Sleep(200 * time.Millisecond)

	count, err := m.ReleaseAgentLocks("agentA")
	if err != nil {
		t.Fatalf("ReleaseAgentLocks: %v", err)
	}
	if count != 2 {
		t.Fatalf("count = %d, want 2", count)
	}

	// B should now acquire x.
	select {
	case r := <-bGot:
		if !r.Locked {
			t.Fatalf("B should acquire x after release, got %+v", r)
		}
	case <-time.After(2 * time.Second):
		t.Fatalf("B did not acquire after ReleaseAgentLocks")
	}

	// z (agentOther) still held.
	locks, _ := m.ListLocks()
	foundZ := false
	for _, l := range locks {
		if l.Name == "z" {
			foundZ = true
		}
	}
	if !foundZ {
		t.Fatalf("agentOther's lock z should remain")
	}
}

// TestConcurrentAcquireSingleWinner stresses the atomic-acquire guarantee: many
// goroutines race for one free lock; exactly one must win.
func TestConcurrentAcquireSingleWinner(t *testing.T) {
	m := newManager(t)
	ctx := context.Background()

	const n = 25
	results := make(chan bool, n)
	start := make(chan struct{})
	for i := 0; i < n; i++ {
		go func() {
			<-start
			r, err := m.Lock(ctx, "hot", "agent", 30, 0, "")
			if err != nil {
				t.Errorf("Lock: %v", err)
			}
			results <- r.Locked
		}()
	}
	close(start)

	wins := 0
	for i := 0; i < n; i++ {
		if <-results {
			wins++
		}
	}
	if wins != 1 {
		t.Fatalf("exactly one goroutine should acquire, got %d winners", wins)
	}
}
