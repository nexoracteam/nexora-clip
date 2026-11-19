// nexora-tunnel: a tiny helper that puts Nexora Clip's remote-access page on the internet through Tailscale Funnel,
// without the Tailscale app. It joins the user's own (free) tailnet with Tailscale's embedded `tsnet` library, so no
// driver and no admin rights are needed. The app starts it, reads one JSON object per line from its output, and closes
// its input to stop it.
//
//	{"event":"login","url":"https://login.tailscale.com/a/..."}   the user must sign in once
//	{"event":"state","state":"Running"}                             Tailscale's own state
//	{"event":"ready","url":"https://nexora-clip.<tailnet>.ts.net"}  the public address
//	{"event":"action","message":"...","url":"https://..."}          a one-time step in the Tailscale account (we keep retrying)
//	{"event":"error","message":"..."}                               something the user can act on
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"os"
	"strings"
	"sync"
	"time"

	"tailscale.com/client/local"
	"tailscale.com/ipn"
	"tailscale.com/tsnet"
)

var outMu sync.Mutex

func emit(m map[string]any) {
	outMu.Lock()
	defer outMu.Unlock()
	b, _ := json.Marshal(m)
	fmt.Println(string(b))
}

func fail(msg string) {
	emit(map[string]any{"event": "error", "message": msg})
	os.Exit(1)
}

func watch(ctx context.Context, lc *local.Client) {
	w, err := lc.WatchIPNBus(ctx, ipn.NotifyInitialState|ipn.NotifyNoPrivateKeys)
	if err != nil {
		return
	}
	defer w.Close()
	for {
		n, err := w.Next()
		if err != nil {
			return
		}
		if n.BrowseToURL != nil && *n.BrowseToURL != "" {
			emit(map[string]any{"event": "login", "url": *n.BrowseToURL})
		}
		if n.State != nil {
			emit(map[string]any{"event": "state", "state": n.State.String()})
		}
	}
}

// funnelHelp turns Tailscale's error into one sentence and the page where the person fixes it.
func funnelHelp(msg, node string) (string, string) {
	l := strings.ToLower(msg)
	switch {
	case strings.Contains(l, "node attribute") || strings.Contains(l, "funnel not available"):
		return "Allow Funnel for this PC in your Tailscale account (one click). It starts by itself afterwards.", "https://login.tailscale.com/f/funnel?node=" + node
	case strings.Contains(l, "https"):
		return "Turn on HTTPS certificates in your Tailscale account (DNS page). It starts by itself afterwards.", "https://login.tailscale.com/admin/dns"
	}
	return "Tailscale says: " + msg, ""
}

func main() {
	dir := flag.String("dir", "", "folder that keeps this PC's Tailscale identity")
	target := flag.String("target", "127.0.0.1:8765", "local address to expose")
	host := flag.String("hostname", "nexora-clip", "name of this PC on the tailnet")
	flag.Parse()
	if *dir == "" {
		fail("missing --dir")
	}
	if err := os.MkdirAll(*dir, 0o700); err != nil {
		fail("could not create the state folder: " + err.Error())
	}

	// The app holds our input open; when it ends (or the app dies) we stop.
	go func() {
		r := bufio.NewReader(os.Stdin)
		for {
			if _, err := r.ReadByte(); err != nil {
				os.Exit(0)
			}
		}
	}()

	quiet := func(string, ...any) {}
	srv := &tsnet.Server{Dir: *dir, Hostname: *host, Logf: quiet, UserLogf: quiet}
	defer srv.Close()
	if err := srv.Start(); err != nil {
		fail("could not start Tailscale: " + err.Error())
	}
	lc, err := srv.LocalClient()
	if err != nil {
		fail("could not reach Tailscale: " + err.Error())
	}
	ctx := context.Background()
	go watch(ctx, lc)

	// Up waits for the sign-in too, so give the person time.
	upCtx, cancel := context.WithTimeout(ctx, 20*time.Minute)
	st, err := srv.Up(upCtx)
	cancel()
	if err != nil {
		fail("Tailscale did not connect: " + err.Error())
	}

	node := ""
	domain := ""
	if st != nil && st.Self != nil {
		node = string(st.Self.ID)
		domain = strings.TrimSuffix(st.Self.DNSName, ".")
	}

	// Funnel needs a one-time permission in the person's Tailscale account (and HTTPS certificates). Until it is
	// given, say exactly where, and keep trying so it just starts working once they have done it.
	var ln net.Listener
	last := ""
	deadline := time.Now().Add(20 * time.Minute)
	for {
		ln, err = srv.ListenFunnel("tcp", ":443")
		if err == nil {
			break
		}
		text, link := funnelHelp(err.Error(), node)
		if text != last {
			last = text
			emit(map[string]any{"event": "action", "message": text, "url": link})
		}
		if time.Now().After(deadline) {
			fail("Funnel was not allowed in time. Press Connect to try again.")
		}
		time.Sleep(4 * time.Second)
	}
	if domain == "" {
		fail("Tailscale gave this PC no address. Turn on MagicDNS in the Tailscale admin page.")
	}

	tgt, err := url.Parse("http://" + *target)
	if err != nil {
		fail("bad --target")
	}
	proxy := &httputil.ReverseProxy{
		Rewrite: func(pr *httputil.ProxyRequest) {
			pr.SetURL(tgt)
			pr.SetXForwarded() // the client address as seen here; the app trusts only this one
			pr.Out.Header.Set("X-Forwarded-Proto", "https")
		},
		FlushInterval: -1, // pass the live event stream straight through
		ErrorHandler: func(w http.ResponseWriter, r *http.Request, err error) {
			http.Error(w, "Nexora Clip is not answering on the PC.", http.StatusBadGateway)
		},
	}
	emit(map[string]any{"event": "ready", "url": "https://" + domain})
	if err := http.Serve(ln, proxy); err != nil {
		fail("the tunnel stopped: " + err.Error())
	}
}
