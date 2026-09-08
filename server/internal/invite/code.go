// Package invite generates and parses invitation codes.
//
// A code is read aloud, typed by hand, and pasted from a chat message, so the
// alphabet matters more than the entropy budget: Crockford base32 drops I, L,
// O and U, which removes the 1/l/I and 0/O confusions and the only letter
// combination that reliably produces an unfortunate word.
package invite

import (
	"crypto/rand"
	"fmt"
	"strings"
)

// Crockford base32 without I, L, O, U.
const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

// Prefix marks a string as one of ours, so a mistyped paste fails fast with a
// clear message instead of a lookup miss.
const Prefix = "YPTD"

// groups × groupLen characters of payload. 8 characters of a 32-symbol
// alphabet is 40 bits: enough that guessing is hopeless, short enough to read
// over the phone.
const (
	groups   = 2
	groupLen = 4
)

// New returns a code like "YPTD-7K2M-9XQP".
func New() (string, error) {
	raw := make([]byte, groups*groupLen)
	if _, err := rand.Read(raw); err != nil {
		return "", fmt.Errorf("invite: read random: %w", err)
	}

	parts := make([]string, 0, groups+1)
	parts = append(parts, Prefix)
	for g := range groups {
		var b strings.Builder
		for i := range groupLen {
			b.WriteByte(alphabet[int(raw[g*groupLen+i])%len(alphabet)])
		}
		parts = append(parts, b.String())
	}
	return strings.Join(parts, "-"), nil
}

// Normalize canonicalizes a code the way a person is likely to have typed it:
// any case, with or without dashes, and with the characters Crockford treats
// as aliases folded in (O→0, I/L→1). An empty string means the input could not
// be a code of ours.
func Normalize(input string) string {
	var payload strings.Builder
	for _, r := range strings.ToUpper(strings.TrimSpace(input)) {
		switch r {
		case '-', ' ':
			continue
		case 'O':
			payload.WriteByte('0')
		case 'I', 'L':
			payload.WriteByte('1')
		default:
			if strings.ContainsRune(alphabet, r) || (r >= 'A' && r <= 'Z') {
				payload.WriteRune(r)
			} else {
				return ""
			}
		}
	}

	value := payload.String()
	value = strings.TrimPrefix(value, Prefix)
	if len(value) != groups*groupLen {
		return ""
	}
	for _, r := range value {
		if !strings.ContainsRune(alphabet, r) {
			return ""
		}
	}

	parts := make([]string, 0, groups+1)
	parts = append(parts, Prefix)
	for g := range groups {
		parts = append(parts, value[g*groupLen:(g+1)*groupLen])
	}
	return strings.Join(parts, "-")
}

// Valid reports whether input normalizes to a well-formed code.
func Valid(input string) bool {
	return Normalize(input) != ""
}
