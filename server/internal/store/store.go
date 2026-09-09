// Package store is yptd's own persistence: invitations, accounts, and the
// long-lived device credentials the TUI keeps in its keychain.
//
// It uses its own MongoDB database, separate from openim_v3, so a yptd schema
// change can never disturb OpenIM's collections.
package store

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"time"

	"go.mongodb.org/mongo-driver/bson"
	"go.mongodb.org/mongo-driver/mongo"
	"go.mongodb.org/mongo-driver/mongo/options"
)

var (
	ErrNotFound      = errors.New("store: not found")
	ErrInviteUsed    = errors.New("store: invitation already used")
	ErrInviteExpired = errors.New("store: invitation expired")
	ErrUserExists    = errors.New("store: user already exists")
)

type Store struct {
	db *mongo.Database
}

// Invite is a one-time registration ticket.
type Invite struct {
	Code      string     `bson:"_id"`
	Note      string     `bson:"note,omitempty"`
	CreatedAt time.Time  `bson:"created_at"`
	ExpiresAt time.Time  `bson:"expires_at"`
	UsedBy    string     `bson:"used_by,omitempty"`
	UsedAt    *time.Time `bson:"used_at,omitempty"`
}

func (i Invite) Used() bool { return i.UsedBy != "" }

// User is a yptd account. UserID doubles as the OpenIM userID.
type User struct {
	UserID    string    `bson:"_id"`
	Nickname  string    `bson:"nickname"`
	CreatedAt time.Time `bson:"created_at"`
	Disabled  bool      `bson:"disabled"`
	// PasswordHash is optional: it exists only for accounts that opted into
	// the password fallback. Invite-only accounts have none.
	PasswordHash string `bson:"password_hash,omitempty"`
	InviteCode   string `bson:"invite_code,omitempty"`
}

// Credential is a long-lived device token. Only its hash is stored, so a
// database leak does not hand over live sessions.
type Credential struct {
	TokenHash  string     `bson:"_id"`
	UserID     string     `bson:"user_id"`
	DeviceName string     `bson:"device_name,omitempty"`
	CreatedAt  time.Time  `bson:"created_at"`
	LastUsedAt *time.Time `bson:"last_used_at,omitempty"`
	Revoked    bool       `bson:"revoked"`
}

func Open(ctx context.Context, uri, dbName string) (*Store, error) {
	client, err := mongo.Connect(ctx, options.Client().ApplyURI(uri).SetConnectTimeout(10*time.Second))
	if err != nil {
		return nil, fmt.Errorf("store: connect: %w", err)
	}
	if err := client.Ping(ctx, nil); err != nil {
		return nil, fmt.Errorf("store: ping: %w", err)
	}
	s := &Store{db: client.Database(dbName)}
	if err := s.ensureIndexes(ctx); err != nil {
		return nil, err
	}
	return s, nil
}

func (s *Store) ensureIndexes(ctx context.Context) error {
	// Expired invitations are swept by Mongo itself rather than a cron we
	// would have to remember to run.
	if _, err := s.db.Collection("invites").Indexes().CreateOne(ctx, mongo.IndexModel{
		Keys:    bson.D{{Key: "expires_at", Value: 1}},
		Options: options.Index().SetExpireAfterSeconds(7 * 24 * 3600),
	}); err != nil {
		return fmt.Errorf("store: invites index: %w", err)
	}
	if _, err := s.db.Collection("credentials").Indexes().CreateOne(ctx, mongo.IndexModel{
		Keys: bson.D{{Key: "user_id", Value: 1}},
	}); err != nil {
		return fmt.Errorf("store: credentials index: %w", err)
	}
	return nil
}

func (s *Store) Close(ctx context.Context) error { return s.db.Client().Disconnect(ctx) }

// ---------------------------------------------------------------- invites ---

func (s *Store) CreateInvite(ctx context.Context, code, note string, ttl time.Duration) (Invite, error) {
	now := time.Now().UTC()
	inv := Invite{Code: code, Note: note, CreatedAt: now, ExpiresAt: now.Add(ttl)}
	if _, err := s.db.Collection("invites").InsertOne(ctx, inv); err != nil {
		return Invite{}, fmt.Errorf("store: create invite: %w", err)
	}
	return inv, nil
}

func (s *Store) ListInvites(ctx context.Context, includeUsed bool) ([]Invite, error) {
	filter := bson.M{}
	if !includeUsed {
		filter["used_by"] = bson.M{"$exists": false}
	}
	cur, err := s.db.Collection("invites").Find(ctx, filter,
		options.Find().SetSort(bson.D{{Key: "created_at", Value: -1}}))
	if err != nil {
		return nil, fmt.Errorf("store: list invites: %w", err)
	}
	var out []Invite
	if err := cur.All(ctx, &out); err != nil {
		return nil, fmt.Errorf("store: list invites: %w", err)
	}
	return out, nil
}

// RedeemInvite marks a code used, atomically. Two people pasting the same code
// at once must not both get in, so the guard is in the update filter rather
// than a read-then-write.
func (s *Store) RedeemInvite(ctx context.Context, code, userID string) error {
	var inv Invite
	err := s.db.Collection("invites").FindOne(ctx, bson.M{"_id": code}).Decode(&inv)
	if errors.Is(err, mongo.ErrNoDocuments) {
		return ErrNotFound
	}
	if err != nil {
		return fmt.Errorf("store: redeem: %w", err)
	}
	if inv.Used() {
		return ErrInviteUsed
	}
	if time.Now().After(inv.ExpiresAt) {
		return ErrInviteExpired
	}

	now := time.Now().UTC()
	res, err := s.db.Collection("invites").UpdateOne(ctx,
		bson.M{"_id": code, "used_by": bson.M{"$exists": false}},
		bson.M{"$set": bson.M{"used_by": userID, "used_at": now}})
	if err != nil {
		return fmt.Errorf("store: redeem: %w", err)
	}
	if res.ModifiedCount == 0 {
		return ErrInviteUsed
	}
	return nil
}

func (s *Store) DeleteInvite(ctx context.Context, code string) error {
	res, err := s.db.Collection("invites").DeleteOne(ctx, bson.M{"_id": code})
	if err != nil {
		return fmt.Errorf("store: delete invite: %w", err)
	}
	if res.DeletedCount == 0 {
		return ErrNotFound
	}
	return nil
}

// ------------------------------------------------------------------ users ---

func (s *Store) CreateUser(ctx context.Context, u User) error {
	u.CreatedAt = time.Now().UTC()
	_, err := s.db.Collection("users").InsertOne(ctx, u)
	if mongo.IsDuplicateKeyError(err) {
		return ErrUserExists
	}
	if err != nil {
		return fmt.Errorf("store: create user: %w", err)
	}
	return nil
}

func (s *Store) GetUser(ctx context.Context, userID string) (User, error) {
	var u User
	err := s.db.Collection("users").FindOne(ctx, bson.M{"_id": userID}).Decode(&u)
	if errors.Is(err, mongo.ErrNoDocuments) {
		return User{}, ErrNotFound
	}
	if err != nil {
		return User{}, fmt.Errorf("store: get user: %w", err)
	}
	return u, nil
}

func (s *Store) ListUsers(ctx context.Context) ([]User, error) {
	cur, err := s.db.Collection("users").Find(ctx, bson.M{},
		options.Find().SetSort(bson.D{{Key: "created_at", Value: 1}}))
	if err != nil {
		return nil, fmt.Errorf("store: list users: %w", err)
	}
	var out []User
	if err := cur.All(ctx, &out); err != nil {
		return nil, fmt.Errorf("store: list users: %w", err)
	}
	return out, nil
}

// DeleteUser removes the local row for an account.
//
// The roster the client's invite and direct-message pickers read comes from
// here, so this is what takes a retired test account out of everybody's view.
// The OpenIM account itself stays: OpenIM has no endpoint for deleting one.
// SetUserNickname renames an account in the roster.
func (s *Store) SetUserNickname(ctx context.Context, userID, nickname string) error {
	res, err := s.db.Collection("users").UpdateOne(ctx,
		bson.M{"_id": userID}, bson.M{"$set": bson.M{"nickname": nickname}})
	if err != nil {
		return fmt.Errorf("store: set nickname: %w", err)
	}
	if res.MatchedCount == 0 {
		return ErrNotFound
	}
	return nil
}

func (s *Store) DeleteUser(ctx context.Context, userID string) (bool, error) {
	res, err := s.db.Collection("users").DeleteOne(ctx, bson.M{"_id": userID})
	if err != nil {
		return false, fmt.Errorf("store: delete user: %w", err)
	}
	return res.DeletedCount > 0, nil
}

func (s *Store) SetUserDisabled(ctx context.Context, userID string, disabled bool) error {
	res, err := s.db.Collection("users").UpdateOne(ctx,
		bson.M{"_id": userID}, bson.M{"$set": bson.M{"disabled": disabled}})
	if err != nil {
		return fmt.Errorf("store: set disabled: %w", err)
	}
	if res.MatchedCount == 0 {
		return ErrNotFound
	}
	return nil
}

func (s *Store) SetPasswordHash(ctx context.Context, userID, hash string) error {
	res, err := s.db.Collection("users").UpdateOne(ctx,
		bson.M{"_id": userID}, bson.M{"$set": bson.M{"password_hash": hash}})
	if err != nil {
		return fmt.Errorf("store: set password: %w", err)
	}
	if res.MatchedCount == 0 {
		return ErrNotFound
	}
	return nil
}

// ------------------------------------------------------------ credentials ---

// NewToken returns a fresh device token and the hash to store. The plaintext
// is returned exactly once; nothing recoverable is persisted.
func NewToken() (plain, hash string, err error) {
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		return "", "", fmt.Errorf("store: read random: %w", err)
	}
	plain = "yptd_" + base64.RawURLEncoding.EncodeToString(raw)
	return plain, HashToken(plain), nil
}

func HashToken(plain string) string {
	sum := sha256.Sum256([]byte(plain))
	return hex.EncodeToString(sum[:])
}

func (s *Store) CreateCredential(ctx context.Context, userID, hash, deviceName string) error {
	c := Credential{
		TokenHash:  hash,
		UserID:     userID,
		DeviceName: deviceName,
		CreatedAt:  time.Now().UTC(),
	}
	if _, err := s.db.Collection("credentials").InsertOne(ctx, c); err != nil {
		return fmt.Errorf("store: create credential: %w", err)
	}
	return nil
}

// LookupCredential resolves a plaintext token, refreshing its last-used stamp.
func (s *Store) LookupCredential(ctx context.Context, plain string) (Credential, error) {
	var c Credential
	err := s.db.Collection("credentials").FindOne(ctx, bson.M{"_id": HashToken(plain)}).Decode(&c)
	if errors.Is(err, mongo.ErrNoDocuments) {
		return Credential{}, ErrNotFound
	}
	if err != nil {
		return Credential{}, fmt.Errorf("store: lookup credential: %w", err)
	}
	if c.Revoked {
		return Credential{}, ErrNotFound
	}
	now := time.Now().UTC()
	_, _ = s.db.Collection("credentials").UpdateOne(ctx,
		bson.M{"_id": c.TokenHash}, bson.M{"$set": bson.M{"last_used_at": now}})
	c.LastUsedAt = &now
	return c, nil
}

// RevokeUserCredentials invalidates every device token for a user and reports
// how many were live.
func (s *Store) RevokeUserCredentials(ctx context.Context, userID string) (int64, error) {
	res, err := s.db.Collection("credentials").UpdateMany(ctx,
		bson.M{"user_id": userID, "revoked": false},
		bson.M{"$set": bson.M{"revoked": true}})
	if err != nil {
		return 0, fmt.Errorf("store: revoke: %w", err)
	}
	return res.ModifiedCount, nil
}

func (s *Store) ListCredentials(ctx context.Context, userID string) ([]Credential, error) {
	cur, err := s.db.Collection("credentials").Find(ctx, bson.M{"user_id": userID, "revoked": false})
	if err != nil {
		return nil, fmt.Errorf("store: list credentials: %w", err)
	}
	var out []Credential
	if err := cur.All(ctx, &out); err != nil {
		return nil, fmt.Errorf("store: list credentials: %w", err)
	}
	return out, nil
}

// ---------------------------------------------------------------- the bot ---

// BotSession is the opencode session that continues this chat, or "" when
// the chat has not asked the bot anything yet.
func (s *Store) BotSession(ctx context.Context, conversationID string) (string, error) {
	var row struct {
		SessionID string `bson:"session_id"`
	}
	err := s.db.Collection("bot_sessions").FindOne(ctx, bson.M{"_id": conversationID}).Decode(&row)
	if errors.Is(err, mongo.ErrNoDocuments) {
		return "", nil
	}
	if err != nil {
		return "", err
	}
	return row.SessionID, nil
}

func (s *Store) SetBotSession(ctx context.Context, conversationID, sessionID string) error {
	_, err := s.db.Collection("bot_sessions").UpdateOne(ctx,
		bson.M{"_id": conversationID},
		bson.M{"$set": bson.M{"session_id": sessionID, "updated_at": time.Now()}},
		options.Update().SetUpsert(true),
	)
	return err
}

// BotBlocked reports whether this user has been barred from the bot.
//
// Everyone with an account may use it: getting an account already costs an
// invitation code, so a second list of who is welcome would only be the same
// gate written twice. This collection holds the exceptions.
func (s *Store) BotBlocked(ctx context.Context, userID string) (bool, error) {
	err := s.db.Collection("bot_block").FindOne(ctx, bson.M{"_id": userID}).Err()
	if errors.Is(err, mongo.ErrNoDocuments) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	return true, nil
}

func (s *Store) BotBlock(ctx context.Context, userID string) error {
	_, err := s.db.Collection("bot_block").UpdateOne(ctx,
		bson.M{"_id": userID},
		bson.M{"$set": bson.M{"added_at": time.Now()}},
		options.Update().SetUpsert(true),
	)
	return err
}

func (s *Store) BotUnblock(ctx context.Context, userID string) (bool, error) {
	res, err := s.db.Collection("bot_block").DeleteOne(ctx, bson.M{"_id": userID})
	if err != nil {
		return false, err
	}
	return res.DeletedCount > 0, nil
}

func (s *Store) BotBlockList(ctx context.Context) ([]string, error) {
	cur, err := s.db.Collection("bot_block").Find(ctx, bson.M{})
	if err != nil {
		return nil, err
	}
	defer cur.Close(ctx)
	var out []string
	for cur.Next(ctx) {
		var row struct {
			UserID string `bson:"_id"`
		}
		if err := cur.Decode(&row); err != nil {
			return nil, err
		}
		out = append(out, row.UserID)
	}
	return out, cur.Err()
}
