// Command yptd-sidecar exposes openim-sdk-core over a Unix socket.
//
// It exists because the SDK is Go-only and the client is Rust, but the reason
// it is a *separate process* rather than an FFI call is narrower: the SDK and
// its dependencies write to stdout and stderr. Inside the TUI's process that
// output lands in the middle of the rendered frame. Here it goes to a log
// file, and the terminal stays the terminal's.
package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"sync"
	"syscall"

	"github.com/his1devil/yptd/sidecar/internal/bridge"
	"github.com/his1devil/yptd/sidecar/internal/protocol"
)

func main() {
	socket := flag.String("socket", "", "Unix socket path to listen on (required)")
	dataDir := flag.String("data-dir", "", "SDK data directory (required)")
	logPath := flag.String("log", "", "log file; defaults to <data-dir>/sidecar.log")
	flag.Parse()

	if *socket == "" || *dataDir == "" {
		fmt.Fprintln(os.Stderr, "用法: yptd-sidecar --socket <path> --data-dir <path> [--log <path>]")
		os.Exit(2)
	}
	if err := run(*socket, *dataDir, *logPath); err != nil {
		log.Printf("sidecar exited: %v", err)
		os.Exit(1)
	}
}

func run(socket, dataDir, logPath string) error {
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return fmt.Errorf("data dir: %w", err)
	}
	if logPath == "" {
		logPath = filepath.Join(dataDir, "sidecar.log")
	}
	restore, err := captureOutput(logPath)
	if err != nil {
		return err
	}
	defer restore()

	listener, err := listen(socket)
	if err != nil {
		return err
	}
	defer listener.Close()
	log.Printf("listening on %s, data dir %s", socket, dataDir)

	// A signal has to close the listener, or Accept blocks forever and the
	// deferred socket cleanup never runs.
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	go func() {
		<-stop
		log.Print("signal received, closing listener")
		_ = listener.Close()
	}()

	for {
		conn, err := listener.Accept()
		if err != nil {
			if errors.Is(err, net.ErrClosed) {
				return nil
			}
			return fmt.Errorf("accept: %w", err)
		}
		// One client at a time, on purpose: the SDK holds a single global
		// session, so a second connection would be sharing one login. Serving
		// it serially makes that obvious instead of subtly wrong.
		serve(conn)
		log.Print("client disconnected")
	}
}

// maxSocketPath is the sockaddr_un limit: 104 bytes on macOS, 108 on Linux.
// Binding a longer path fails with a bare "invalid argument", so the check is
// here to say what actually went wrong.
const maxSocketPath = 100

// listen creates the socket, clearing a stale one left by a crash.
func listen(path string) (net.Listener, error) {
	if len(path) > maxSocketPath {
		return nil, fmt.Errorf(
			"socket 路径 %d 字节，超过 %d 字节上限（sockaddr_un 的限制）: %s",
			len(path), maxSocketPath, path)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, fmt.Errorf("socket dir: %w", err)
	}
	if info, err := os.Stat(path); err == nil {
		if info.Mode()&os.ModeSocket == 0 {
			return nil, fmt.Errorf("%s exists and is not a socket", path)
		}
		// Probe before removing: a live sidecar answering here means this one
		// should refuse to start rather than steal its socket.
		if c, err := net.Dial("unix", path); err == nil {
			c.Close()
			return nil, fmt.Errorf("another sidecar is already listening on %s", path)
		}
		_ = os.Remove(path)
	}
	listener, err := net.Listen("unix", path)
	if err != nil {
		return nil, fmt.Errorf("listen: %w", err)
	}
	// The socket carries a logged-in session; only its owner may connect.
	if err := os.Chmod(path, 0o600); err != nil {
		listener.Close()
		return nil, fmt.Errorf("chmod socket: %w", err)
	}
	return listener, nil
}

func serve(conn net.Conn) {
	defer conn.Close()
	writer := protocol.NewWriter(conn)
	reader := protocol.NewReader(conn)
	handler := &bridge.Handler{Out: writer}

	// Requests are answered concurrently -- pulling history must not block an
	// outgoing message -- but ordering per id is preserved by the id itself.
	var wg sync.WaitGroup
	for {
		req, err := reader.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			log.Printf("decode: %v", err)
			_ = writer.Fail(0, 0, err.Error())
			continue
		}
		wg.Add(1)
		go func(req protocol.Request) {
			defer wg.Done()
			data, err := handler.Dispatch(req.Op, req.Args)
			if err != nil {
				var sdkErr *bridge.SDKError
				if errors.As(err, &sdkErr) {
					_ = writer.Fail(req.ID, sdkErr.Code, sdkErr.Msg)
					return
				}
				_ = writer.Fail(req.ID, 0, err.Error())
				return
			}
			_ = writer.Reply(req.ID, data)
		}(req)
	}
	wg.Wait()
}

// captureOutput redirects this process's stdout and stderr into a log file.
//
// This is the whole point of the sidecar. openim-sdk-core and several of its
// dependencies print diagnostics on stdout; sharing a terminal with a ratatui
// client would corrupt the frame. Redirecting at the file-descriptor level
// catches cgo and any library that writes to fd 1 directly, which reassigning
// os.Stdout would not.
func captureOutput(path string) (func(), error) {
	file, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return nil, fmt.Errorf("log file: %w", err)
	}
	savedOut, err := syscall.Dup(int(os.Stdout.Fd()))
	if err != nil {
		file.Close()
		return nil, fmt.Errorf("dup stdout: %w", err)
	}
	savedErr, err := syscall.Dup(int(os.Stderr.Fd()))
	if err != nil {
		syscall.Close(savedOut)
		file.Close()
		return nil, fmt.Errorf("dup stderr: %w", err)
	}
	_ = syscall.Dup2(int(file.Fd()), int(os.Stdout.Fd()))
	_ = syscall.Dup2(int(file.Fd()), int(os.Stderr.Fd()))
	log.SetOutput(file)
	log.SetFlags(log.LstdFlags | log.Lmicroseconds)

	return func() {
		_ = syscall.Dup2(savedOut, int(os.Stdout.Fd()))
		_ = syscall.Dup2(savedErr, int(os.Stderr.Fd()))
		syscall.Close(savedOut)
		syscall.Close(savedErr)
		// Point the logger back at the real stderr *before* closing the file,
		// or a fatal error on the way out is written to a closed descriptor
		// and silently lost -- which is exactly when you need to see it.
		log.SetOutput(os.Stderr)
		file.Close()
	}, nil
}
