// Package channel holds the per-channel settings yptd layers on top of an
// OpenIM group.
package channel

import "encoding/json"

// Policy is what a channel says about itself: can strangers find it, and can
// they walk in once they have.
//
// It lives in the group's `ex` field rather than in this service's database.
// Two reasons: it travels with the group, so a dismissed channel takes its
// settings with it instead of leaving a row behind; and OpenIM already only
// lets the owner and admins write that field, which is exactly the permission
// this setting needs and saves writing a second one here.
//
// The zero value is the closed one — both off. That matters twice over: a
// group whose `ex` is empty (every group created before this existed) stays
// private rather than silently becoming public, and an `ex` we cannot parse
// fails closed too.
type Policy struct {
	// Findable: shows up when someone searches the channel directory.
	Findable bool
	// Joinable: anyone who has found it can walk in. Off means invitation
	// only — this is enforced in the before-join webhook, not just by hiding
	// a button.
	Joinable bool
}

// Stored is the shape inside `ex`. The `yptd` tag follows what messages do,
// so anything else that ever wants a corner of this field can have one
// without the two of them fighting over the whole object.
type stored struct {
	YPTD string `json:"yptd"`
	Find int    `json:"find"`
	Join int    `json:"join"`
}

const kind = "channel"

// Parse reads a group's `ex`. Anything unexpected — empty, not JSON, somebody
// else's payload — reads as the closed policy.
func Parse(ex string) Policy {
	if ex == "" {
		return Policy{}
	}
	var s stored
	if err := json.Unmarshal([]byte(ex), &s); err != nil || s.YPTD != kind {
		return Policy{}
	}
	return Policy{Findable: s.Find == 1, Joinable: s.Join == 1}
}

// Encode writes the `ex` for a policy.
func Encode(p Policy) string {
	b, _ := json.Marshal(stored{YPTD: kind, Find: btoi(p.Findable), Join: btoi(p.Joinable)})
	return string(b)
}

func btoi(b bool) int {
	if b {
		return 1
	}
	return 0
}

// OpenIM's needVerification, which decides what a join request actually does.
const (
	// AllNeedVerification parks a join request for approval. Nobody approves
	// them here — there is no approval inbox — so this is the value a closed
	// channel carries, and the webhook refuses the request before it can
	// become one.
	AllNeedVerification = 1
	// Directly lets the request straight through.
	Directly = 2
)

// Verification is the OpenIM group setting that goes with a policy. It has to
// be kept in step: leaving a joinable channel on AllNeedVerification turns
// every allowed join into a pending application that nobody will ever look at.
func Verification(p Policy) int {
	if p.Joinable {
		return Directly
	}
	return AllNeedVerification
}
