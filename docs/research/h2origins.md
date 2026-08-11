# cloudflared Reference Brief: HTTP/2 Edge Transport, Origin Handlers, Tunnel Credentials, Reconnect

Checkout: `/home/aperso/libcfd/cloudflared` (vendor/ present; Go). No `origin/` package exists — origin handling lives in `connection/`, `ingress/`, `proxy/`. h2mux is gone from this checkout: only a legacy reference (protocol.go:16) and an upgrade shim that maps the `h2mux` flag to HTTP/2 (protocol.go:243-246). `vendor/golang.org/x/net/http2` is the HTTP/2 implementation used for edge connections.

## 1. HTTP/2 edge connection

### Dialing the edge
- Entry: `supervisor/tunnel.go:478-531` (`serveConnection`, `connection.HTTP2` branch). Builds TLS config with `cfdcrypto.TLSConfigWithCurvePreferences(e.config.EdgeTLSConfigs[protocol], ...)` then calls `edgediscovery.DialEdge(ctx, dialTimeout, tlsConfig, addr.TCP, e.edgeBindAddr)`.
- `edgediscovery/dial.go:15-46` — `DialEdge` does a plain `net.Dialer.DialContext(ctx, "tcp", edgeTCPAddr.String())`, wraps in `tls.Client(edgeConn, tlsConfig)`, sets a handshake deadline (`dialTimeout = 15s`, supervisor/tunnel.go:55), calls `tlsEdgeConn.Handshake()`, then clears the deadline. Returns the raw `*tls.Conn`.
- Port: edge addresses come from SRV `_v2-origintunneld._tcp.argotunnel.com` (`edgediscovery/allregions/discovery.go:20-25`); `resolveSRV` uses `srv.Port` for both TCP and UDP (`discovery.go:185-189`). Preflight probes hard-code `"Port 7844 (HTTP/2)"` / `"Port 7844 (QUIC)"` (`prechecks/probes.go:31-44`). So the edge TCP port is 7844 (same port is used for UDP/QUIC).
- TLS: SNI/ServerName is `h2.cftunnel.com` — `edgeH2TLSServerName = "h2.cftunnel.com"` (`connection/protocol.go:19`), applied via `Protocol.TLSSettings()` (`protocol.go:64-69`, HTTP2 has `NextProtos: nil`). QUIC uses `edgeQUICServerName = "quic.cftunnel.com"` with `NextProtos: []string{"argotunnel"}` (`quicProtos = "argotunnel"`, protocol.go:21-24, 74-75).
- Important nuance (corrects the "ALPN h2" assumption): cloudflared's HTTP/2 TLS config sets **no ALPN** (`TLSSettings.NextProtos` is nil for HTTP2, and `tlsconfig.CreateTunnelConfig`/`GetConfig` never append `h2`; grep confirms no `NextProtos`/`h2` in `tlsconfig/*.go`). cloudflared does not run the x/net/http2 *client* toward the edge; it runs the x/net/http2 **server** side (`http2.Server.ServeConn`, http2.go:83) over the raw TLS conn and waits for the edge to send the HTTP/2 client preface. The edge's own client uses ALPN `"h2"` (`vendor/golang.org/x/net/http2/http2.go:78` `NextProtoTLS = "h2"`), but that is on the edge side, not negotiated by cloudflared. The precheck `probeHTTP2` likewise sends no ALPN (prechecks/probes.go:240-272).

### Registration over HTTP/2
- cloudflared is the HTTP/2 server; the edge opens a new HTTP/2 stream (any edge-initiated stream id, there is no reserved stream 1 in the HTTP/2 variant) whose request carries `Cf-Cloudflared-Proxy-Connection-Upgrade: control-stream`:
  - `InternalUpgradeHeader = "Cf-Cloudflared-Proxy-Connection-Upgrade"` and `ControlStreamUpgrade = "control-stream"` (http2.go:27-30).
  - `determineHTTP2Type` / `isControlStreamUpgrade(r)` returns `r.Header.Get(InternalUpgradeHeader) == ControlStreamUpgrade` (http2.go:383-386, 405-407).
  - `ServeHTTP` routes `TypeControlStream` to `c.controlStreamHandler.ServeControlStream(r.Context(), respWriter, c.connOptions.ConnectionOptions(), c.orchestrator)` (http2.go:99-126).
- The RPC is capnp over an io.ReadWriteCloser where `respWriter` (`http2RespWriter`) maps `Read()` → request body `r.Body` and `Write()` → the HTTP/2 response stream (http2.go:345-363). So registration rides the body of that single control-stream HTTP/2 request/response.
- `connection/control.go:78-135` — `controlStream.ServeControlStream` builds the client via `c.registerClientFunc(ctx, rw, registerTimeout)` (default `tunnelrpc.NewRegistrationClient`, control.go:47-50) and calls:
  - `registrationClient.RegisterConnection(ctx, c.tunnelProperties.Credentials.Auth(), c.tunnelProperties.Credentials.TunnelID, connOptions, c.connIndex, c.edgeAddress)` (control.go:86-102).
  - On success: `regSuccess` metric, `logConnected`, `sendConnectedEvent`, `connectedFuse.Connected()`. If `connIndex == 0` and `!registrationDetails.TunnelIsRemotelyManaged`, sends local ingress config via `SendLocalConfiguration(ctx, tunnelConfig)` (control.go:103-127).
- `tunnelrpc/registration_client.go:36-40` — `NewRegistrationClient` = `SafeTransport(stream)` + `NewClientConn(stream)` + `conn.Bootstrap(ctx)` + `pogs.NewRegistrationServer_PogsClient`. `RegisterConnection` (registration_client.go:47-65) is a capnp call with `RegisterConnection(ctx, auth, tunnelID, connIndex, options)`.
- Server-side interface: `tunnelrpc/pogs/registration_server.go:19-27` — `RegistrationServer` with `RegisterConnection(ctx, auth TunnelAuth, tunnelID uuid.UUID, connIndex byte, options *ConnectionOptions) (*ConnectionDetails, error)`, `UnregisterConnection(ctx)`, `UpdateLocalConfiguration(ctx, config []byte)`. `TunnelAuth{AccountTag string, TunnelSecret []byte}` (registration_server.go:127-129); `ConnectionDetails{UUID uuid.UUID, Location string, TunnelIsRemotelyManaged bool}` (registration_server.go:161-165). Errors can be `RetryableError` with `RetryAfter` delay (registration_server.go:277-282).
- QUIC contrast (relevant context): the QUIC transport *does* reserve the first stream for control — `q.conn.OpenStream()` with comment "The edge assumes the first stream is used for the control plane" (quic_connection.go:87-90); same `controlStreamHandler.ServeControlStream` is used (quic_connection.go:179-181).

### Incoming request routing (HTTP/2)
- `ServeHTTP` (http2.go:99-170) classifies each new edge-initiated HTTP/2 stream via `determineHTTP2Type` (http2.go:380-388): configuration update (`Cf-Cloudflared-Proxy-Connection-Upgrade: update-configuration`), websocket (`...: websocket`), TCP (`Cf-Cloudflared-Proxy-Src` header present, `IsTCPStream`, http2.go:410-412), control-stream, else plain HTTP.
- HTTP/WebSocket: `originProxy.ProxyHTTP(respWriter, tr, connType == TypeWebsocket)` (http2.go:130-135). The `Cf-Cloudflared-Proxy-Connection-Upgrade` header is stripped before proxying (`stripWebsocketUpgradeHeader`, http2.go:414-416).
- TCP (warp routing): builds `TCPRequest{Dest: host, CFRay: FindCfRayHeader(r), LBProbe: IsLBProbeRequest(r), CfTraceID: r.Header.Get(tracing.TracerContextName), ConnIndex: c.connIndex}` and calls `originProxy.ProxyTCP(r.Context(), rws, ...)` where `rws = NewHTTPResponseReadWriteAcker(respWriter, respWriter, r)` (http2.go:137-150).
- `MaxConcurrentStreams: math.MaxUint32` (http2.go:65-69, connection/connection.go:33).
- Errors: if the handler fails before status written, `respWriter.WriteErrorResponse(err)` writes 502 with `Cf-Cloudflared-Response-Meta: {"src":"cloudflared",...}`; if status was already sent the handler panics `http.ErrAbortHandler` (http2.go:151-162, 283-302).

### Writing responses
- `http2RespWriter.WriteRespHeaders(status, header)` (http2.go:175-224): `content-length` is passed through as a real HTTP/2 header; `cf-int-cloudflared-tracing` is promoted to the canonical `Cf-Int-Cloudflared-Tracing-*` header; all other origin headers are base64-serialized into the single header `Cf-Cloudflared-Response-Headers` (`CanonicalResponseUserHeaders`, header.go:22-24; `SerializeHeaders`, header.go:113-146) to avoid HTTP/2 header validation on HTTP/1 values. Sets `Cf-Cloudflared-Response-Meta: {"src":"origin"}` (header.go:36-58, http2.go:199-203). 101 Switching Protocols is remapped to 200 (HTTP/2 has no 101, http2.go:206-211).
- Trailers via `http2.TrailerPrefix + name` (http2.go:154-162).
- Streaming flush: `TypeWebsocket|TypeTCP|TypeControlStream` set `shouldFlush` (connection/connection.go:121-130); `Write()` flushes after every write (http2.go:328-341). `shouldFlush(headers)` also enables flush-on-write for missing content-length / chunked / SSE / gRPC / ndjson (connection/connection.go:224-259).
- `Hijack()` (http2.go:228-268) returns a `localProxyConnection` (a net.Conn facade over the respWriter, connection/connection.go:185-212) plus a `bufio.ReadWriter` so websocket/TCP origin streams can be piped bidirectionally over the HTTP/2 stream body.

## 2. Origin handler abstraction

### The interface cloudflared's connection layer requires
`connection.OriginProxy` (connection/connection.go:151-154):
```go
type OriginProxy interface {
    ProxyHTTP(w ResponseWriter, tr *tracing.TracedHTTPRequest, isWebsocket bool) error
    ProxyTCP(ctx context.Context, rwa ReadWriteAcker, req *TCPRequest) error
}
```
Supporting types (connection/connection.go:157-171, 214-221):
```go
type TCPRequest struct { Dest, CFRay, LBProbe, FlowID, CfTraceID string; ConnIndex uint8 }
type ReadWriteAcker interface { io.ReadWriter; AckConnection(tracePropagation string) error }
type ResponseWriter interface {
    WriteRespHeaders(status int, header http.Header) error
    AddTrailer(trailerName, trailerValue string)
    http.ResponseWriter; http.Hijacker; io.Writer
}
```
Both transports get the proxy through the same `Orchestrator` (connection/connection.go:52-57):
```go
type Orchestrator interface {
    UpdateConfig(version int32, config []byte) *pogs.UpdateConfigurationResponse
    GetConfigJSON() ([]byte, error)
    GetOriginProxy() (OriginProxy, error)
}
```
- HTTP/2 invocation: `c.orchestrator.GetOriginProxy()` then `ProxyHTTP`/`ProxyTCP` (http2.go:112-150).
- QUIC invocation: `q.orchestrator.GetOriginProxy()`; `dispatchRequest` (quic_connection.go:221-245) switches on `pogs.ConnectRequest.Type` (`ConnectionTypeHTTP/Websocket/TCP`, quic_metadata_protocol.go:16-22), reconstructs an `http.Request` from metadata keys `HttpHeader:<name>` / `HttpMethod` / `HttpHost` / `dest` (quic_connection.go:348-400), and wraps the QUIC stream in `httpResponseAdapter` (ResponseWriter) or `streamReadWriteAcker` (ReadWriteAcker). `AckConnection` writes a capnp `ConnectResponse` on the stream (quic_connection.go:250-300).

### Concrete implementation: `proxy.Proxy`
`proxy/proxy.go:25-61` implements `OriginProxy` and dispatches per matched ingress rule (proxy.go:80-146). The ingress rules select between three *service capability interfaces* (ingress/origin_proxy.go:12-31):
```go
type HTTPOriginProxy interface { http.RoundTripper }
type StreamBasedOriginProxy interface {
    EstablishConnection(ctx context.Context, dest string, log *zerolog.Logger) (OriginConnection, error)
}
type HTTPLocalProxy interface { http.Handler }
```
- `HTTPOriginProxy` (RoundTrip) → `proxyHTTPRequest` (proxy.go:146-213): for websocket re-sets `Connection: Upgrade`, `Upgrade: websocket`, `Sec-Websocket-Version: 13`, zeroes content-length/body; `RoundTrip`; copies headers; `w.WriteRespHeaders(resp.StatusCode, headers)`; on `101 Switching Protocols`, pipes the response `io.ReadWriteCloser` body with a `bidirectionalStream{writer: w, reader: tr.Body}`; otherwise `cfio.Copy(w, resp.Body)` and copies trailers. Adds `Cf-Warp-Tag-*` headers (proxy.go:135-138).
- `StreamBasedOriginProxy` → `proxyStream` (proxy.go:213-249): `EstablishConnection(ctx, dest, logger)` → `rwa.AckConnection(encodedSpans)` → `originConn.Stream(ctx, rwa, logger)`. `dest` for bastion comes from `carrier.ResolveBastionDest(req)` (proxy.go:276-281).
- `HTTPLocalProxy` → `proxyLocalRequest` (proxy.go:259-266): re-adds websocket upgrade headers then `proxy.ServeHTTP(w, req)`.
- `ProxyTCP` (proxy.go:149-181) is the warp-routing path: parses `req.Dest` with `netip.ParseAddrPort`, dials via `ingress.OriginTCPDialer` (`originDialer.DialTCP(ctx, dest)`), `AckConnection`, `stream.Pipe`.

### Origin connection types
`ingress.OriginConnection` (ingress/origin_connection.go:16-21):
```go
type OriginConnection interface {
    Stream(ctx context.Context, tunnelConn io.ReadWriter, log *zerolog.Logger)
    Close() error
}
```
Concrete `OriginService` implementations (ingress/origin_service.go):
- `httpService` (http/https/ws/wss origins) — HTTPOriginProxy; rewrites `ws→http`, `wss→https` (origin_proxy.go:56-80).
- `unixSocketPath` (unix:/unix+tls:) — RoundTrip over unix DialContext (origin_proxy.go:48-54).
- `statusCode` (http_status:N) — fixed status response (origin_proxy.go:82-92).
- `helloWorld` — built-in HTTPS test server (origin_service.go:160-198).
- `rawTCPService` (warp-routing) — StreamBasedOriginProxy; `DialContext(ctx, "tcp", dest)` → `tcpConnection` (origin_proxy.go:95-105; origin_connection.go:37-55).
- `tcpOverWSService` (ssh/rdp/smb/tcp/bastion) — StreamBasedOriginProxy; wraps TCP conn in `websocket.NewConn` and runs `DefaultStreamHandler` or `socks.StreamHandler` (origin_proxy.go:108-121; origin_connection.go:57-79).
- `socksProxyOverWSService` (socks-proxy) — OriginConnection whose Stream runs the SOCKS server (origin_connection.go:82-96).
- `ManagementService` — HTTPLocalProxy (origin_service.go:229-258).
- Rule matching: `Ingress.FindMatchingRule(hostname, path)` with wildcard `*.` host and path regex (ingress/ingress.go:47-73); last rule must be catch-all; default no-rules → `http_status:503` (ingress.go:295-302).

`Orchestrator.GetOriginProxy` returns `proxy.NewOriginProxy(ingressRules, originDialerService, tags, flowLimiter, log)` (orchestration/orchestrator.go:190, 245-250); it is swapped atomically (`o.proxy.Store(proxy)`) on configuration updates (orchestrator.go:171-193).

## 3. Tunnel abstraction (NamedTunnel vs QuickTunnel)

There are no `NamedTunnel`/`QuickTunnel` structs in this checkout anymore (they were refactored into `TunnelProperties` + `Credentials`). This is the current shape:

- `connection.TunnelProperties` (connection/connection.go:58-61):
  ```go
  type TunnelProperties struct {
      Credentials    Credentials
      QuickTunnelUrl string
  }
  ```
- `connection.Credentials` (connection/connection.go:64-77) — **no JSON tags**, so the credentials file JSON is exactly the Go field names:
  ```go
  type Credentials struct {
      AccountTag   string      // JSON key "AccountTag"
      TunnelSecret []byte      // JSON key "TunnelSecret" (base64-encoded by encoding/json)
      TunnelID     uuid.UUID   // JSON key "TunnelID"
      Endpoint     string      // JSON key "Endpoint" (region override, optional)
  }
  func (c *Credentials) Auth() pogs.TunnelAuth { // {AccountTag, TunnelSecret}
  ```
  This matches the task's expected credentials-file shape (`AccountTag`, `TunnelID`, `TunnelSecret`) and is confirmed by `writeTunnelCredentials` (`json.Marshal(credentials)`, subcommands.go:301-315) and by the test fixture JSON `{"AccountTag":"...","TunnelSecret":"...","TunnelID":"...","TunnelName":"..."}` (subcommand_context_test.go:57-62, note `TunnelName` is ignored by unmarshal).
- `connection.TunnelToken` (connection/connection.go:78-97) — the token form, base64(std json):
  ```go
  type TunnelToken struct {
      AccountTag   string    `json:"a"`
      TunnelSecret []byte    `json:"s"`
      TunnelID     uuid.UUID `json:"t"`
      Endpoint     string    `json:"e,omitempty"`
  }
  func (t TunnelToken) Credentials() Credentials
  func (t TunnelToken) Encode() (string, error) // base64.StdEncoding of json
  ```
  `ParseToken` decodes base64 and unmarshals (subcommands.go:800-809).
- Quick tunnel: `RunQuickTunnel` (cmd/cloudflared/tunnel/quick_tunnel.go:26-97) POSTs `https://api.trycloudflare.com/tunnel` (default of `--quick-service`, cmd.go:873-875) and reads:
  ```go
  type QuickTunnel struct {
      ID         string `json:"id"`
      Name       string `json:"name"`
      Hostname   string `json:"hostname"`
      AccountTag string `json:"account_tag"`
      Secret     []byte `json:"secret"`
  }
  ```
  (quick_tunnel.go:111-117). It then builds `connection.Credentials{AccountTag: data.Result.AccountTag, TunnelSecret: data.Result.Secret, TunnelID: tunnelID}` and `&connection.TunnelProperties{Credentials: credentials, QuickTunnelUrl: data.Result.Hostname}` (quick_tunnel.go:83-97). Quick tunnels force protocol `quic` and `HAConnections=1` (quick_tunnel.go:90-96).
- Named tunnels: credentials file loaded by `readTunnelCredentials` (subcommand_context.go:103-126, `json.Unmarshal` into `connection.Credentials`); backwards-compat enriches missing `TunnelID` from the CLI arg (subcommand_context.go:244-263). Token-based run path: `runCommand` → `ParseToken` → `token.Credentials()` → `sc.runWithCredentials` (subcommands.go:767-791).
- Registration uses `c.tunnelProperties.Credentials.Auth()` and `Credentials.TunnelID` (control.go:86-102). `Endpoint`/region is used for edge discovery region selection (configuration.go:192-203).

## 4. Unregister / reconnect logic

### Unregister (graceful shutdown)
`controlStream.waitForUnregister` (connection/control.go:137-170): after registration, block on `ctx.Done()` or `gracefulShutdownC`; then `registrationClient.GracefulShutdown(ctx, c.gracePeriod)` → capnp `UnregisterConnection` RPC (registration_client.go:82-94). `MaxGracePeriod = 3m` (connection/connection.go:32). On HTTP/2, `HTTP2Connection.Serve` returns nil when `controlStreamHandler.IsStopped()` (graceful) vs `errEdgeConnectionClosed` otherwise (http2.go:88-98). On QUIC, a nil control-stream exit waits out `gracePeriod` before tearing down (quic_connection.go:94-107).

### Reconnect / backoff
- Supervisor run loop (supervisor/supervisor.go:142-221): on per-connection error, if the error is a `ReconnectSignal` reconnect immediately; otherwise queue the connection index, arm a shared `backoff = retry.NewBackoff(config.Retries, tunnelRetryDuration=10s, retryForever=true)`, and on timer expiry restart all waiting tunnels. After a successful reconnect (`nextConnectedSignal`), `backoff.SetGracePeriod()` resets retry count.
- `retry.BackoffHandler` (retry/backoffhandler.go:20-120): exponential `maxTimeToWait = baseTime * 2^retries`, jittered to a random value within, optional `retryForever`; `SetGracePeriod()` sets a reset deadline (base 2s doubling) after which retries reset to 0; `ResetNow()`.
- Per-connection loop `EdgeTunnelServer.Serve` (supervisor/tunnel.go:225-356): fetches an edge address, runs `serveTunnel`, then on error:
  - `IpAddrFallback.ShouldGetNewAddress` (tunnel.go:180-218) rotates edge IP for `DupConnRegisterTunnelError` / `*quic.IdleTimeoutError` (immediately) and for `edgediscovery.DialError` / `*connection.EdgeQuicDialError` (up to `MaxEdgeAddrRetries`, then `ConnectivityError`).
  - Logs "Retrying connection in up to <duration>", `Observer.SendReconnect(connIndex)` event, then waits on `protocolFallback.BackoffTimer()` (tunnel.go:255-291).
  - Protocol fallback: QUIC → HTTP2 (`Protocol.fallback()`, protocol.go:42-51). `selectNextProtocol` (tunnel.go:312-357) switches protocol after backoff max retries or when QUIC is deemed broken (quic IdleTimeout/TransportError, `isQuicBroken` tunnel.go:359-378). `edgeH2mux` flag is silently upgraded to http2 (protocol.go:243-246).
- Registration error policy (tunnel.go:380-408): `DupConnRegisterTunnelError` ("EDUPCONN", errors.go:9-13) → no retry on same address, supervisor picks a new address; `ServerRegisterTunnelError` retries iff server error was `RetryableError` (non-permanent); `EdgeQuicDialError` → no retry; `ReconnectSignal` → immediate reconnect after `err.DelayBeforeReconnect()`.
- First-connection loop `startFirstTunnel` (supervisor.go:245-303) retries specific error types (DupConn, quic Idle/Application, DialError, EdgeQuicDial, ControlStream/StreamListener/DatagramManager errors), bails on unknown errors.
- HA: `HAConnections` tunnels (default 4); each has a `connIndex uint8` (0..N-1); quick tunnels force 1.

### Metrics reported
- Local Prometheus: `cloudflared_tunnel_register_success` (`regSuccess`), `cloudflared_tunnel_register_fail` (`regFail`), `cloudflared_tunnel_rpc_fail`, `cloudflared_tunnel_server_locations`, `tunnel_ids` (HA gauge) — connection/metrics.go:18-27, 76-124; increments in control.go:95-106.
- To the edge at registration: `pogs.ConnectionOptions{Client: ClientInfo{ClientID [16]byte, Features, Version, Arch}, OriginLocalIP, ReplaceExisting: false, CompressionQuality: 0, NumPreviousAttempts}` (pogs/registration_server.go:110-129; client/config.go:38-70). `NumPreviousAttempts` comes from `uint8(backoff.Retries())` (tunnel.go:503, 543).
- Observer events (`connection/event.go:8-37`): Disconnected / Connected / Reconnecting / SetURL (quick tunnel URL) / RegisteringTunnel / Unregistering; sinks via `Observer.dispatchEvents` (observer.go:100-118).

## Files likely to need changes in a Rust port
- Edge HTTP/2 server: `connection/http2.go` (dial side is `supervisor/tunnel.go` + `edgediscovery/dial.go`).
- Transport-neutral origin proxy contract: `connection/connection.go` (`OriginProxy`, `ResponseWriter`, `ReadWriteAcker`, `TCPRequest`).
- Origin dispatch: `proxy/proxy.go`, `ingress/origin_proxy.go`, `ingress/origin_connection.go`, `ingress/origin_service.go`.
- Registration RPC: `connection/control.go`, `tunnelrpc/registration_client.go`, `tunnelrpc/pogs/registration_server.go`, `tunnelrpc/pogs/quic_metadata_protocol.go`.
- Credentials: `connection/connection.go` (Credentials/TunnelToken), `cmd/cloudflared/tunnel/quick_tunnel.go`, `cmd/cloudflared/tunnel/subcommand_context.go`.
- Reconnect/backoff: `supervisor/supervisor.go`, `supervisor/tunnel.go`, `retry/backoffhandler.go`.

## Constraints / risks for the port
- `libcfd` must be async-runtime agnostic; the Go http2 handler model (ServeHTTP per stream + Hijack) maps naturally to Hyper's `h2` server with `SendResponse`/trait objects, but websocket/TCP upgrade-over-h2 needs the raw-stream model (`http2::server::SendStream`/`Upgrade`) — plan for it.
- Registration RPC rides the control-stream request *body* over HTTP/2; in Rust/Hyper the edge-initiated request stream can be upgraded to a raw bidirectional stream, which mirrors `http2RespWriter.Read/Write`.
- User headers are base64-serialized into `cf-cloudflared-response-headers` (resp) — must reproduce `SerializeHeaders`/`DeserializeHeaders` (header.go:113-175) exactly for wire compatibility; same for `cf-cloudflared-request-headers` on the request path (edge→cloudflared user headers).
- `TunnelSecret` is `[]byte`; JSON marshaling of credentials uses Go field names (PascalCase) with standard-base64 secret. Keep that shape in the Rust serde structs if aiming for file/token compatibility.
- QUIC uses ALPN `argotunnel`; HTTP/2 uses no ALPN from cloudflared's side. If the Rust port uses an HTTP/2 client (rather than server) toward the edge it would be architecturally wrong — cloudflared serves h2 and the edge is the client.

