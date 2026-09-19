// Package art decides which portrait an agent wears.
//
// 头像是服务端数据，不是客户端资源。以前 iOS 把每个 agent 的脸写死在 App 里（加一个
// agent 要发版才有专属头像），桌面端画的是另一套东西，同一个 HALX 在两台设备上长得
// 不一样。现在 agents.json 的 avatar 是唯一的真相，`bot setup` 把它传进对象存储、写进
// OpenIM 的 faceURL，三端（和推送通知）显示同一张。
package art

import (
	"fmt"
	"mime"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// Pool lists the portraits available to agents that name none, sorted by file
// name. The order is part of the contract — see Pick.
func Pool(dir string) ([]string, error) {
	entries, err := os.ReadDir(filepath.Join(dir, "pool"))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}
	var out []string
	for _, e := range entries {
		if e.IsDir() || strings.HasPrefix(e.Name(), ".") {
			continue
		}
		switch strings.ToLower(filepath.Ext(e.Name())) {
		case ".png", ".jpg", ".jpeg":
			out = append(out, filepath.Join(dir, "pool", e.Name()))
		}
	}
	sort.Strings(out)
	return out, nil
}

// Pick chooses a portrait for an agent nobody assigned one to: FNV-1a over the
// id, modulo the pool.
//
// 算法和 iOS 旧版 `AgentArt.name(for:)` 一字不差（同样的 FNV-1a、同样按 UTF-8 字节），
// 池子的文件按 1-indigo、2-pink、3-sky、4-royal、5-lavender、6-mint 排序也和它的数组
// 同序——所以还没升级的 iOS 客户端按自己的兜底算出来的脸，和服务端挑的是同一张。
func Pick(id string, pool []string) string {
	if len(pool) == 0 {
		return ""
	}
	hash := uint32(2166136261)
	for _, b := range []byte(id) {
		hash ^= uint32(b)
		hash *= 16777619
	}
	return pool[int(hash%uint32(len(pool)))]
}

// Source is where an agent's avatar comes from.
type Source struct {
	// URL is set when agents.json already names a public address: used as is.
	URL string
	// File is a local image to upload; empty when URL is set or nothing fits.
	File string
}

// Resolve reads the `avatar` field of an agent. It may be a public URL, a file
// (absolute, or relative to the art directory), or empty — then the pool decides.
func Resolve(avatar, id, dir string, pool []string) Source {
	switch {
	case strings.HasPrefix(avatar, "https://"), strings.HasPrefix(avatar, "http://"):
		return Source{URL: avatar}
	case avatar == "":
		return Source{File: Pick(id, pool)}
	case filepath.IsAbs(avatar):
		return Source{File: avatar}
	}
	return Source{File: filepath.Join(dir, avatar)}
}

// ObjectName is what the uploaded portrait is called in object storage. The
// content hash is in the name: a changed picture is a changed URL, so no client
// keeps showing the old face out of its cache.
func ObjectName(id, file, md5hex string) string {
	short := md5hex
	if len(short) > 12 {
		short = short[:12]
	}
	return fmt.Sprintf("%s/avatar-%s%s", id, short, strings.ToLower(filepath.Ext(file)))
}

// ContentType guesses from the extension; portraits are only ever png or jpeg.
func ContentType(file string) string {
	if t := mime.TypeByExtension(strings.ToLower(filepath.Ext(file))); t != "" {
		return t
	}
	return "application/octet-stream"
}
