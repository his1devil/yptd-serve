package openim

import (
	"bytes"
	"context"
	"crypto/md5"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"net/url"
)

// Upload puts one file into OpenIM's object storage and returns the permanent
// /object/ URL clients use. name is the object name, `<userID>/<file>`.
//
// 走管理员 token：它不受「对象名必须以自己的 userID 开头」的限制，所以能替 agent 传
// 头像而不必给每个 agent 取一个用户 token（取 token 还会把同平台的旧会话顶掉）。
//
// 头像放进对象存储而不是 nginx 的静态目录，是为了走 /object/ 那条路：客户端能按槽位
// 要 64/128 档的缩图（几 KB），不必每次下 270 KB 的原图；缓存头也在那条路上。
//
// 同样内容传第二次是「秒传」：OpenIM 按 MD5 去重，initiate 直接回地址，一个字节不传。
func (c *Client) Upload(ctx context.Context, name, contentType, cause string, data []byte) (string, error) {
	if c.PublicAPI == "" {
		return "", fmt.Errorf("openim upload: PublicAPI is not set, the returned URL would point at loopback")
	}
	admin, err := c.AdminToken(ctx)
	if err != nil {
		return "", err
	}
	// OpenIM 的 hash 不是文件的 MD5，而是「各分片 MD5 的十六进制用逗号连起来，再做一次
	// MD5」（tools/s3/cont/controller.go 的 CompleteUpload）。单片就是 md5(md5hex(data))。
	// 三端的 SDK 都这么算，所以同一个文件从哪端传都能秒传；直接拿文件 MD5 当 hash，PUT 会
	// 成功，到 complete 才报 "md5 mismatching"。
	partSum := md5.Sum(data)
	part := hex.EncodeToString(partSum[:])
	sum := md5.Sum([]byte(part))
	hash := hex.EncodeToString(sum[:])

	type kv struct {
		Key    string   `json:"key"`
		Values []string `json:"values"`
	}
	type signed struct {
		PartNumber int    `json:"partNumber"`
		URL        string `json:"url"`
		Query      []kv   `json:"query"`
		Header     []kv   `json:"header"`
	}
	var started struct {
		URL    string `json:"url"`
		Upload *struct {
			UploadID string `json:"uploadID"`
			Sign     struct {
				URL    string   `json:"url"`
				Query  []kv     `json:"query"`
				Header []kv     `json:"header"`
				Parts  []signed `json:"parts"`
			} `json:"sign"`
		} `json:"upload"`
	}
	// partSize 给到文件大小以上，就是单片直传。头像几百 KB，远在 5 MB 的最小分片之下。
	err = c.post(ctx, "/object/initiate_multipart_upload", map[string]any{
		"hash": hash, "size": len(data), "partSize": 5 << 20, "maxParts": 1,
		"cause": cause, "name": name, "contentType": contentType,
	}, admin, &started)
	if err != nil {
		return "", err
	}
	if started.Upload == nil {
		return started.URL, nil // 秒传
	}
	if len(started.Upload.Sign.Parts) != 1 {
		return "", fmt.Errorf("openim upload: expected one part, got %d", len(started.Upload.Sign.Parts))
	}

	signedPart := started.Upload.Sign.Parts[0]
	target := signedPart.URL
	if target == "" {
		target = started.Upload.Sign.URL
	}
	u, err := url.Parse(target)
	if err != nil {
		return "", fmt.Errorf("openim upload: bad signed url: %w", err)
	}
	q := u.Query()
	for _, set := range [][]kv{started.Upload.Sign.Query, signedPart.Query} {
		for _, item := range set {
			for _, v := range item.Values {
				q.Add(item.Key, v)
			}
		}
	}
	u.RawQuery = q.Encode()
	req, err := http.NewRequestWithContext(ctx, http.MethodPut, u.String(), bytes.NewReader(data))
	if err != nil {
		return "", err
	}
	for _, set := range [][]kv{started.Upload.Sign.Header, signedPart.Header} {
		for _, item := range set {
			for _, v := range item.Values {
				req.Header.Add(item.Key, v)
			}
		}
	}
	req.ContentLength = int64(len(data))
	resp, err := c.http.Do(req)
	if err != nil {
		return "", fmt.Errorf("openim upload: put: %w", err)
	}
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 512))
	resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		return "", fmt.Errorf("openim upload: put: http %d: %s", resp.StatusCode, body)
	}

	var done struct {
		URL string `json:"url"`
	}
	err = c.post(ctx, "/object/complete_multipart_upload", map[string]any{
		"uploadID": started.Upload.UploadID, "parts": []string{part},
		"name": name, "contentType": contentType, "cause": cause,
	}, admin, &done)
	if err != nil {
		return "", err
	}
	return done.URL, nil
}
