# Cloudflare Tunnel Edge Protocol Digest

Condensed from `docs/research/{quic,quicktunnel,rpc,h2origins}.md`, all over the same
`cloudflared/` checkout @ commit `61a0b0b3` (tag `2026.7.3-3-g61a0b0b3`). Purpose: sequence the
libcfd implementation and serve as the fact base for worker briefs and review checklists.
Everything below is an exact constant, byte sequence, field name, or behavior from the checkout
unless marked *discrepancy* or *risk*.

---

## 0. File map (open these first)

| Concern | File(s) in checkout |
|---|---|
| QUIC client connection (control stream, stream loop, request reconstruction) | `connection/quic_connection.go` |
| Control-stream orchestration (register → config → wait → unregister) | `connection/control.go` |
| Registration RPC client | `tunnelrpc/registration_client.go`, `tunnelrpc/registration_server.go` |
| Stream signatures / version byte | `tunnelrpc/quic/protocol.go` |
| QUIC request wire framing | `tunnelrpc/quic/request_client_stream.go`, `request_server_stream.go` |
| Schemas (registration) | `tunnelrpc/proto/tunnelrpc.capnp` (+ generated `.capnp.go` = authoritative IDs/offsets) |
| Schemas (per-stream metadata) | `tunnelrpc/proto/quic_metadata_protocol.capnp` |
| capnp→Go conversions | `tunnelrpc/pogs/*.go` |
| QUIC config / dial wiring | `supervisor/tunnel.go` (serveQUIC ~560-660), `connection/quic.go`, `quic/constants.go` |
| ALPN/SNI constants | `connection/protocol.go` |
| TLS build / CF root CAs | `tlsconfig/tlsconfig.go`, `tlsconfig/cloudflare_ca.go` |
| H2 edge (server side) | `connection/http2.go`, `edgediscovery/dial.go` |
| Edge discovery | `edgediscovery/allregions/{discovery,regions}.go`, `edgediscovery/edgediscovery.go` |
| Quick tunnel creation | `cmd/cloudflared/tunnel/quick_tunnel.go` |
| Origin proxy abstraction | `connection/connection.go` (interfaces), `proxy/proxy.go`, `ingress/origin_{proxy,connection,service}.go` |
| Datagrams | `quic/datagram.go`, `quic/datagramv2.go`, `quic/v3/*.go`, `connection/quic_datagram_{v2,v3}.go` |
| capnp-rpc internals (wire behavior) | `vendor/zombiezen.com/go/capnproto2/rpc/{rpc,transport,question,answer,tables}.go`, `.../mem.go`, `.../std/capnp/rpc/rpc.capnp.go` |

Notable absences in this checkout: no `connection/edge.go`, `dial.go`, `control_stream.go`,
`request*.go`, no `quic/request.go`, no `origin/` package, no `rpc/` directory, no
`/cdn-cgi/trace` usage, no `TryCloudflare` header, no `NamedTunnel`/`QuickTunnel` structs
(refactored into `TunnelProperties` + `Credentials`). h2mux is gone (flag maps to HTTP/2).
quic-go is a fork: `replace github.com/quic-go/quic-go => github.com/chungthuang/quic-go v0.45.1-0.20250428085412-43229ad201fd`.

---

## 1. QUIC transport

### 1.1 ALPN, SNI, TLS

- ALPN string is exactly **`"argotunnel"`** (`quicProtos`, `connection/protocol.go:38`); tests use `[]string{"argotunnel"}`.
- TLS settings for QUIC: `ServerName: "quic.cftunnel.com"`, `NextProtos: ["argotunnel"]` (`protocol.go:74-78`).
- Root CAs = `x509.SystemCertPool()` + built-in Cloudflare origin roots (`tlsconfig/cloudflare_ca.go`, 3 certs: "CloudFlare Origin SSL ECC Certificate Authority", "CloudFlare Origin SSL Certificate Authority", CN=`origin-pull.cloudflare.net`); `--ca-cert` appends. No client cert, no `InsecureSkipVerify`. quic-go enforces TLS 1.3.
- Curve preferences per connection (`crypto/curves.go`):
  - `PostQuantumPrefer` (default): `[tls.X25519MLKEM768 (0x11ec), P256Kyber768Draft00 (0xfe32 = 65074), tls.CurveP256]`
  - `PostQuantumStrict` (`--post-quantum`): `[X25519MLKEM768, P256Kyber768Draft00]` only.
- UDP socket (`connection/quic.go`): per-`connIndex` socket with port reuse (first socket's ephemeral port cached in `portForConnIndex`, so HA connections share a local port); binds `localIP` if set; macOS uses `udp4`/`udp6` to set the DF bit; probes use `dialopts.SkipPortReuse` → fresh ephemeral port.

### 1.2 QUIC version negotiation

- cloudflared does **not** pin versions: `quic.Config.Versions` unset → quic-go defaults `SupportedVersions = {Version1 = 0x1, Version2 = 0x6b3343cf}`; client's Initial uses `Versions[0]` = **v1**.
- Negotiation = standard QUIC Version Negotiation packet (handled by the stack). **No custom transport parameters, no `qc_metadata`, no app-level version exchange.**
- Only application-level "version" is the per-stream `protocolV1 = "01"` (2 ASCII bytes `0x30 0x31`) on data streams.
- Hostname/tunnel identity is **not** in transport parameters nor a first frame; it rides in the `registerConnection` RPC params.

### 1.3 QUIC connection config (`quic.Config`, from `quic/constants.go` + `supervisor/tunnel.go:578-600`)

| Field | Value |
|---|---|
| `HandshakeIdleTimeout` | 5s |
| `MaxIdleTimeout` | 5s |
| `KeepAlivePeriod` | 1s |
| `MaxIncomingStreams` | `1 << 60` (quic-go max; edge may open ~unlimited streams) |
| `MaxIncomingUniStreams` | `1 << 60` |
| `EnableDatagrams` | `true` (required for UDP/ICMP) |
| `MaxConnectionReceiveWindow` | 30 MiB (`30*(1<<20)`), flag `--quic-connection-level-flow-control-limit` |
| `MaxStreamReceiveWindow` | 6 MiB (`6*(1<<20)`), flag `--quic-stream-level-flow-control-limit` |
| `InitialPacketSize` | 1252 (IPv6 edge) / 1232 (IPv4 edge); reduced from 1280 for WARP MTU 1280 |
| 0-RTT | off (no `Allow0RTT`, no `TokenStore`) — do not enable in Rust |

Unset (quic-go defaults): `InitialStreamReceiveWindow`, `InitialConnectionReceiveWindow`,
`AllowConnectionWindowIncrease`. Datagram sizing (`quic/param_unix.go`): `MaxDatagramFrameSize =
1350`, `maxDatagramPayloadSize = 1280` (Windows: 1220 / `1220-3-16-1`).

### 1.4 Control stream setup

- Client opens the **FIRST bidi stream** of the QUIC connection and uses it as the control plane: `q.conn.OpenStream()` (`quic_connection.go:121-125`). Comment: *"The edge assumes the first stream is used for the control plane."*
- The control stream carries **capnp RPC** (full RPC: bootstrap + calls; see §2), not raw capnp, not HTTP/3.
- **Critical asymmetry:** NO protocol signature bytes are written on the registration stream — `NewRegistrationClient` writes nothing before the capnp bootstrap message, and the edge's `RegistrationServer.Serve` does no signature check. Capnp frames start at byte 0.
- Client wiring: `SafeTransport(stream)` (wraps in `readWriterSafeTemporaryErrorCloser`: retries temporary read errors, sleep 500ms, max 3 retries) → `rpc.NewConn` → `conn.Bootstrap(ctx)` → `pogs.NewRegistrationServer_PogsClient`.
- After registration: `acceptStream` loop (`conn.AcceptStream`, spawn `runStream` per stream); `runStream` wraps stream in `NewSafeStreamCloser` (write deadline = `streamWriteTimeout`, default 0 = disabled) and serves `NewCloudflaredServer(...).Serve(ctx, noCloseStream)`.

### 1.5 Request stream wire format (edge → cloudflared)

Signature bytes (`tunnelrpc/quic/protocol.go`):
- `dataStreamProtocolSignature = {0x0A, 0x36, 0xCD, 0x12, 0xA1, 0x3E}` (6 bytes)
- `rpcStreamProtocolSignature = {0x52, 0xBB, 0x82, 0x5C, 0xDB, 0x65}` (6 bytes)
- `protocolV1 = "01"` = `0x30 0x31`

`CloudflaredServer.Serve` reads exactly the first 6 bytes (`determineProtocol`), then:
- data signature → HTTP/WS/TCP request stream handler
- rpc signature → capnp RPC server for `SessionManager` + `ConfigurationManager` (each new stream = one RPC "request", `context.WithTimeout(ctx, s.responseTimeout)`)

**Request layout (edge → cloudflared), `RequestClientStream.WriteConnectRequestData`:**
```
[6 bytes dataStreamProtocolSignature 0A 36 CD 12 A1 3E]
[2 bytes version "01" = 0x30 0x31]
[raw capnp message: ConnectRequest]      <- standard capnp stream framing (§2.1)
[raw request body bytes ...]             <- UNFRAMED, streamed until EOF
```

**Response layout (cloudflared → edge):** identical preamble (signature + version + capnp
`ConnectResponse`), then raw body bytes. Preamble written once; subsequent body writes go
straight to the stream.

**Capnp stream framing** (unpacked): each message =
`[u32 LE (numSegments-1)] [per segment: u32 LE word count] [zero u32 pad to 8-byte alignment] [segment data]`.
All cloudflared messages are single-segment; the transport buffers each whole message and writes
it in one `Write`.

**`ConnectRequest`** (`quic_metadata_protocol.capnp:9-20`, TypeID `0xc47116a1045e4061`):
- `dest @0 :Text` — full URL for HTTP/WS (`"http://host/path..."`), `host:port` for TCP
- `type @1 :ConnectionType` — enum `http @0`, `websocket @1`, `tcp @2` (TypeID `0xc52e1bac26d379c8`)
- `metadata @2 :List(Metadata)` — `Metadata { key @0 :Text; val @1 :Text }` (TypeID `0xe1446b97bfd1cd37`)

**Metadata keys** (constants in `connection/quic_connection.go:32-42`):
- `"HttpMethod"` — HTTP method
- `"HttpHost"` — value of the `Host` header
- `"HttpHeader:<name>"` — one entry per header value (`HTTPHeaderKey = "HttpHeader"` + `:` separator)
- `"FlowID"` — `QUICMetadataFlowID`, for TCP
- `"cf-trace-id"` — `TracerContextName`, trace context string

**`ConnectResponse`** (`quic_metadata_protocol.capnp:22-26`, TypeID `0xb1032ec91cef8727`):
- `error @0 :Text` (empty on success)
- `metadata @1 :List(Metadata)` — cloudflared sends `"HttpStatus"` = decimal status string, plus `"HttpHeader:<name>"` per response header value (`WriteRespHeaders`).

**Error paths:**
- Before ack: `WriteErrorResponse` → `ConnectResponse(err, {"HttpStatus":"502"})`.
- Flow-limiter rejection appends metadata `{"FlowConnectRateLimited", "true"}`.
- After ack, later error → `quicStream.CancelWrite(0)` = RST_STREAM.
- TCP: `AckConnection` sends a `ConnectResponse` (nil error, optional `cf-int-cloudflared-tracing` metadata) once the origin accepts (`streamReadWriteAcker`).
- WebSocket: `ConnectionTypeWebsocket`; `stripWebsocketUpgradeHeader` removes Upgrade/Connection headers before origin dispatch.

**Body semantics:** body is NOT chunked/framed; the stream itself is the `io.ReadCloser`; body
ends at stream EOF. Content-Length/Transfer-Encoding handling only configures Go's http.Request
client semantics. **No per-stream control messages** (no ping/backpressure frames); backpressure
is pure QUIC stream flow control. **Trailers not supported over QUIC** (`AddTrailer` no-op).

### 1.6 Edge-initiated RPC streams (QUIC)

- Written by edge: 6-byte `rpcStreamProtocolSignature` FIRST, then capnp RPC (no version bytes). Server verifies via `determineProtocol`.
- `SessionManager` (TypeID `0x839445a59fb01686`): `registerUdpSession @0 (sessionId:Data, dstIp:Data, dstPort:UInt16, closeAfterIdleHint:Int64, traceContext:Text="") -> (RegisterUdpSessionResponse {err:Text, spans:Data})`, `unregisterUdpSession @1 (sessionId:Data, message:Text)`.
- `ConfigurationManager` (TypeID `0xb48edfbdaa25db04`): `updateConfiguration @0 (version:Int32, config:Data) -> (UpdateConfigurationResponse {latestAppliedVersion:Int32, err:Text})` → `orchestrator.UpdateConfig`.
- `CloudflaredServer` (TypeID `0xf548cef9dea2a4a1`) extends both.

### 1.7 Datagrams (UDP/ICMP over QUIC DATAGRAM) — summary

Version negotiated via feature strings in `ConnectionOptions.client.features`: `support_datagram_v2` (default) or `support_datagram_v3_2`.
- **v2** (`quic/datagramv2.go`): outgoing UDP payload `[payload][16-byte session UUID suffix][1-byte type]`. Types: `DatagramTypeUDP = 0`, `DatagramTypeIP = 1`, `DatagramTypeIPWithTrace = 2`, `DatagramTypeTracingSpan = 3`. Max payload 1280. Sessions registered via `SessionManager` RPC only.
- **v3** (`quic/v3/*`): first byte is datagram type: `0x0` session registration, `0x1` payload, `0x2` ICMP, `0x3` registration response.
  - Payload: `[0x1][16-byte RequestID BE][payload]`; header len 17; max payload+header 1297.
  - Registration: `[0x0][flags 1B][dst port u16 BE][idle seconds u16 BE][RequestID 16B][dst IP 4|16B][optional bundled payload]`; flags: bit0 IPv6, bit1 traced, bit2 bundled.
  - Response: `[0x3][resp type 1B][RequestID 16B][errMsgLen u16 BE][errMsg]`; resp types `0x00 ok, 0x01 dst unreachable, 0x02 unable to bind, 0x03 too many flows, 0xff error with msg`.
  - v3 sessions registered purely by datagram; RPC `registerUdpSession` returns `ErrUnsupportedRPCUDPRegistration`.

### 1.8 Ping, keepalive, graceful shutdown

- **No application-level ping.** QUIC keepalive via `KeepAlivePeriod = 1s`; `MaxIdleTimeout = 5s`; `quic.IdleTimeoutError` after 5s idle → connection teardown + edge-address rotation in supervisor.
- Graceful shutdown: SIGINT/SIGTERM → `gracefulShutdownC` → `waitForUnregister` calls capnp `UnregisterConnection` (timeout = grace period, default 30s). No dedicated QUIC control frame.
- After unregister RPC returns, QUIC `Serve` waits out the full grace period before cancelling the group.
- Connection close: `CloseWithError(0, "")`.

---

## 2. Registration RPC

### 2.1 Nature

**Full Cap'n Proto RPC** (zombiezen `capnproto2` v2.18.0 `rpc` package: bootstrap, call/return,
finish, capability tables, question/answer ids) — **not** raw per-request capnp marshaling. This
holds for BOTH QUIC and HTTP/2. Wire transport: `rpc.StreamTransport` over the stream, unpacked
capnp stream framing (§1.5), one RPC `Message` per frame, back-to-back, no magic bytes on the
registration stream.

### 2.2 Per-transport differences

- **QUIC:** first bidi stream opened by the client; capnp RPC from byte 0 (no signature). `NewRegistrationClient(ctx, stream, timeout)`.
- **H2:** the edge dials INTO cloudflared's HTTP/2 server. Control stream = an edge-initiated HTTP/2 stream whose request carries header `Cf-Cloudflared-Proxy-Connection-Upgrade: control-stream` (`InternalUpgradeHeader`, `ControlStreamUpgrade`, `connection/http2.go:27-30`). `isControlStreamUpgrade` = `r.Header.Get(InternalUpgradeHeader) == ControlStreamUpgrade`. The request/response bodies are the RPC transport (`http2RespWriter`: Read = `r.Body`, Write = response body, flush per write). Registration completes while the control stream is still open, before serving traffic. No reserved stream number.
- Edge-initiated QUIC RPC streams (SessionManager/ConfigurationManager): 6-byte `rpcStreamProtocolSignature` first, then RPC.

### 2.3 Exact message sequence (client, one control stream)

Question ids are per-connection monotonic from 0 (bootstrap=0, register=1, unregister=2, updateConfig next); server echoes questionId as answerId.

1. **Bootstrap**: write `Message::bootstrap { questionId: 0 }`. Server replies `Message::return { answerId: 0, results: Payload { content: interface ptr, capTable: [CapDescriptor { senderHosted, id: 0 }] } }` — main interface exported as export 0 → client import id 0.
2. **registerConnection**: write `Message::call { questionId: 1, target: MessageTarget{importedCap(0)}, interfaceId: 0xf71695ec7fe85497, methodId: 0, sendResultsTo: caller, params: Payload { content: RegistrationServer_registerConnection_Params, capTable: [] } }`. Params: `auth` (TunnelAuth{AccountTag, TunnelSecret}), `tunnelId` (16 raw UUID bytes), `connIndex` (UInt8, 0..N-1), `options` (ConnectionOptions).
3. **Read return**: `Message::return { answerId: 1, results: Payload { content: registerConnection_Results { result: ConnectionResponse }, capTable: [] } }`.
4. **Finish**: write `Message::finish { questionId: 1, releaseResultCaps: false }` (on exception returns: `releaseResultCaps: true`).
5. **Post-registration** (`connection/control.go:101-113`): if `connIndex == 0 && !tunnelIsRemotelyManaged` → `updateLocalConfiguration` (methodId 2, params `{config: Data}` = JSON ingress config), await empty return.
6. **Steady state**: control stream stays open; `waitForUnregister` blocks on ctx / graceful-shutdown signal.
7. **Unregister**: `Message::call { questionId: 2, target: importedCap(0), interfaceId: 0xf71695ec7fe85497, methodId: 1, params: empty }` → return → finish. `Close()` sends `release` for the bootstrap import and closes the stream.

Per-call timeout: `RPCTimeout` default **5s** (`--rpc-timeout`); unregister timeout = grace period (default 30s; `MaxGracePeriod = 3m`).

### 2.4 Registration schema (`tunnelrpc.capnp`) — TypeIDs, sizes, ordinals

| Struct | TypeID | ObjectSize {data bits, ptrs} | Fields |
|---|---|---|---|
| `ClientInfo` | `0x83ced0145b2f114b` | {0, 4} | `clientId @0 :Data` (16-byte connector UUID), `features @1 :List(Text)`, `version @2 :Text`, `arch @3 :Text` |
| `ConnectionOptions` | `0xb4bf9861fe035d04` | {8, 2} | `client @0`, `originLocalIp @1 :Data` (raw IP bytes), `replaceExisting @2 :Bool` (Bit0), `compressionQuality @3 :UInt8` (@1, 0..3), `numPreviousAttempts @4 :UInt8` (@2) |
| `ConnectionResponse` | `0xdbaa9d03d52b62dc` | {8, 1} | union, discriminator UInt16@0: `error=0`, `connectionDetails=1`; value in Ptr 0 |
| `ConnectionError` | `0xf5f383d2785edb86` | {16, 1} | `cause @0 :Text`, `retryAfter @1 :Int64` (ns), `shouldRetry @2 :Bool` |
| `ConnectionDetails` | `0xb5f39f082b9ac18a` | {8, 2} | `uuid @0 :Data` (16B per-connection), `locationName @1 :Text` (colo code), `tunnelIsRemotelyManaged @2 :Bool` (Bit0) |
| `TunnelAuth` | `0x9496331ab9cd463f` | {0, 2} | `accountTag @0 :Text`, `tunnelSecret @1 :Data` |
| `RegistrationServer` (interface) | `0xf71695ec7fe85497` | — | `registerConnection @0`, `unregisterConnection @1`, `updateLocalConfiguration @2` |
| `registerConnection_Params` | `0xe6646dec8feaa6ee` | {8, 3} | `auth`=Ptr0, `tunnelId`=Ptr1, `connIndex`=UInt8 @ data offset 0, `options`=Ptr2 |
| `registerConnection_Results` | `0xea50d822450d1f17` | {0, 1} | `result`=Ptr0 |

*Discrepancy (unresolved):* quic.md says tunnelSecret is 16 bytes; rpc.md says 32 bytes. The Go
code enforces no length anywhere; the quick-tunnel API supplies it. Treat server acceptance as
authoritative; verify against a live edge before hard-coding length checks. (libcfd should send
whatever the API returned verbatim.)

**ConnectionOptions population** (`client/config.go`): `Client{ClientID: connector UUID, Version, Arch, Features}`, `OriginLocalIP` from the local address of the edge socket (QUIC: `addr.UDP.String()`; H2: `edgeConn.LocalAddr().String()`), `ReplaceExisting: false`, `CompressionQuality: 0`, `NumPreviousAttempts = uint8(backoff.Retries())`.

**Default feature list** (`features/features.go:5-26`):
`["allow_remote_config", "serialized_headers", "support_datagram_v2", "support_quic_eof", "management_logs"]`
plus `"postquantum"` and `"support_datagram_v3_2"` when enabled by feature selector.

**Legacy/deprecated** (do NOT use): `TunnelServer @0xea58385c65416035` (`registerTunnel@0`, `getServerInfo@1`, `unregisterTunnel@2`, `obsoleteDeclarativeTunnelConnect@3`, `authenticate@4`, `reconnectTunnel@5`), `TunnelRegistration @0xf41a0f001ad49e46`, `Authentication @0xc082ef6e0d42ed1d`, `RegistrationOptions @0xc793e50592935b4a`, `ExistingTunnelPolicy @0x84cb9536a2cf6d3c`, `ServerInfo @0xf2c68e2547ec3866`, `AuthenticateResponse @0x82c325a07ad22a65`. `TunnelServer extends RegistrationServer` but wire calls always carry interfaceId `0xf71695ec7fe85497`.

### 2.5 Standard rpc.capnp wire layout facts

- `Message` union (TypeID `0x91b79f1f808db032`): `unimplemented=0, abort=1, call=2, return=3, finish=4, resolve=5, release=6, bootstrap=8, disembargo=13`. Sub-TypeIDs: `Call=0x836a53ce789d4cd4`, `Return=0x9e19b28d3db3573a`, `Finish=0xd37d2eb2c2f80e63`, `Bootstrap=0xe94ccf8031176ec4`, `Release=0xad1a6c0d7dd07497`, `Payload=0x9a0e61223d96743b`, `Exception=0xd625b7063acf691a`.
- **Call**: data = `questionId UInt32@0`, `methodId UInt16@2`, `sendResultsTo` union UInt16@3 (**0=caller** for client calls), `interfaceId UInt64@8`; ptrs: 0=`target` (MessageTarget), 1=`params` (Payload), 2=`thirdParty`.
- **MessageTarget**: union UInt16@4, `importedCap=0` (UInt32@0) / `promisedAnswer=1`. Registration uses `importedCap`.
- **Return**: data = `answerId UInt32@0`, `releaseParamCaps` bit32 (inverted in capnp-go), union UInt16@6 (`results=0, exception=1, canceled=2, resultsSentElsewhere=3, takeFromOtherQuestion=4, acceptFromThirdParty=5`); ptr 0 = results/exception Payload. `results` arm = `Payload { content (root pointer), capTable: List(CapDescriptor) }`.
- **Bootstrap**: `{ questionId }`; server answers `return { answerId, results: { content: interface ptr, capTable: [senderHosted(exportId 0)] } }`. Registration params/results carry no capabilities (cap tables empty) but the bootstrap return must still carry the interface capability so the client can target `importedCap(0)`.
- Dispatch is strictly by `(interfaceId, methodId)`: `(0xf71695ec7fe85497, 0|1|2)` for register/unregister/updateLocalConfiguration.
- Server should answer every call with a return; client sends `finish { questionId, releaseResultCaps }` after each return. `release` (decrement export refcounts) is sent on client `Close()`. Tolerate `unimplemented`; responding `unimplemented` to unknown messages avoids feedback loops. A minimal client-only implementation may ignore inbound `finish`.
- Question/answer ids are local per connection and direction-independent. Registration is strictly request/response on one dedicated stream → a sequential implementation suffices.

### 2.6 Edge responses and error policy

- Success → `ConnectionDetails { uuid, locationName, tunnelIsRemotelyManaged }`. Gates: `connectedFuse.Connected()`; `locationName` logged/metricked; `tunnelIsRemotelyManaged=false` (and connIndex 0) triggers the config push.
- Error arm → `ConnectionError { cause, retryAfter(ns), shouldRetry }`. `shouldRetry`/`retryAfter` set only for `*RetryableError`.
- `cause == "EDUPCONN"` (`connection/errors.go`) → duplicate-connection; supervisor does NOT retry that edge address, picks a new one.
- Other errors → `ServerRegisterTunnelError{Cause, Permanent: !retryable}`: retryable → retry; permanent → stop. Transport drop/timeout = recoverable connection error.

---

## 3. Quick tunnel API

### 3.1 Creation request

- `POST` to `https://api.trycloudflare.com/tunnel` (default of hidden flag `--quick-service`, `cmd.go:873-875`); no query string, path exactly `/tunnel`.
- Headers: `Content-Type: application/json`, `User-Agent: cloudflared/<version>`.
- Body: `nil` (empty). **No `TryCloudflare` header in this checkout** (argo-tunnel era removed); account binding comes entirely from the response.
- Client timeouts: `TLSHandshakeTimeout`, `ResponseHeaderTimeout`, `Timeout` all `15s`.
- No account-id fetch by the client — `account_tag` comes back in the response JSON.

### 3.2 Response JSON shape

```json
{ "success": bool,
  "result": { "id": "<uuid string>", "name": string, "hostname": "<rand>.trycloudflare.com",
              "account_tag": string, "secret": <base64 bytes> },
  "errors": [ { "code": int, "message": string } ] }
```
Struct tags: `id`, `name`, `hostname`, `account_tag`, `secret` (all lowercase snake). Parsed into:
`Credentials{AccountTag: result.account_tag, TunnelSecret: result.secret, TunnelID: uuid.Parse(result.id)}`
and `TunnelProperties{Credentials, QuickTunnelUrl: result.hostname}`. `name` parsed but unused.
Hostname is prefixed with `"https://"` if missing.

Forced defaults for quick tunnels: `--protocol` → `quic` if unset; `--ha-connections` → `1`.

### 3.3 Token format

- `TunnelToken` JSON: `{"a": accountTag, "s": secret, "t": tunnelID, "e": endpoint(omitempty)}`, then `base64.StdEncoding` of that JSON = the token string. `ParseToken` decodes base64 + unmarshal. Quick tunnels don't write a token file; Credentials are built directly from the HTTP response.
- The tunnel secret is used ONLY as `TunnelAuth.tunnelSecret` in the post-TLS registration RPC — never as a TLS client cert, never logged.

### 3.4 Edge discovery (DNS SRV only — no HTTP config endpoint, no `/cdn-cgi/trace` in this checkout)

- SRV query: `net.LookupSRV("v2-origintunneld", "tcp", "argotunnel.com")` → `_v2-origintunneld._tcp.argotunnel.com` (`edgediscovery/allregions/discovery.go`). Region override prefixes the service: `<region>-v2-origintunneld` (e.g. `us-v2-origintunneld`).
- SRV record's `port` field becomes the port for BOTH TCP and UDP addrs; production port **7844** (hardcoded in prechecks).
- Fallback when the system resolver fails: DoT lookup against `1.1.1.1:853` (`cloudflare-dns.com`), timeout 15s.
- Each SRV target is a CNAME hostname resolved to `EdgeAddr{TCP, UDP, IPVersion}` per IP. Known CNAMEs: `region1.v2.argotunnel.com`, `region2.v2.argotunnel.com`, `us-region1/2...`, `fed-region1/2...` (canonical source is the SRV record).
- `ResolveEdge` requires ≥2 SRV targets; `edgeAddrs[0]` → region1, `edgeAddrs[1]` → region2.
- Static override: hidden `--edge` / `TUNNEL_EDGE` (bypasses DNS). No default static list.
- Address selection: `GetAddrForRPC` prefers region1; per-conn `GetAddr(connIndex)` reuses that conn's address; `GetUnusedAddr` picks the region with more free addrs (randomized if tie), preferring a different region than last used; `GetDifferentAddr` rotates on connectivity errors bounded by `--max-edge-addr-retries`; exhaustion → protocol fallback QUIC→HTTP2.
- IP version: `--edge-ip-version` 4|6|auto (default auto → system preference), fallback to secondary set after 10 min.
- Protocol selection for `auto`: TXT `protocol-v2.argotunnel.com` → JSON `[{"protocol":"quic","percentage":N},...]`. Feature flags: TXT `cfd-features.argotunnel.com`.
- `auto` starts with QUIC for token-based tunnels; falls back to HTTP2 on `quic.IdleTimeoutError`/transport "operation not permitted" (`isQuicBroken`).
- Other hostnames: `quic.cftunnel.com` (QUIC SNI), `h2.cftunnel.com` (H2 SNI), `probe.cftunnel.com` (preflight probes), `management.argotunnel.com` / `management.fed.argotunnel.com`, `update.argotunnel.com`. `api.cloudflare.com/client/v4` = named-tunnel CRUD (not needed for Phase A).

---

## 4. HTTP/2 edge

### 4.1 Dialing

- cloudflared runs the **HTTP/2 server side** (`http2.Server.ServeConn` from x/net/http2) over a raw TLS conn and waits for the edge to send the HTTP/2 client preface. The edge is the H2 client. **Architectural constraint: the Rust port must serve h2, not dial an h2 client.**
- TLS: SNI `h2.cftunnel.com`; **no ALPN set by cloudflared** (`NextProtos` nil for HTTP2 — corrects the "ALPN h2" assumption). The edge's own client uses ALPN `"h2"` (`NextProtoTLS`), edge-side only.
- `DialEdge`: plain TCP dial + `tls.Client` + handshake deadline (`dialTimeout = 15s`), then clear deadline. Edge TCP port 7844 (from SRV).
- `MaxConcurrentStreams: math.MaxUint32`.

### 4.2 Registration over H2

- Edge opens a new HTTP/2 stream (any edge-initiated stream id) whose request carries `Cf-Cloudflared-Proxy-Connection-Upgrade: control-stream`; routed to `controlStreamHandler.ServeControlStream` with an `io.ReadWriteCloser` facade over the request/response bodies. Capnp RPC frames flow raw through the bodies — no signature, no upgrade protocol, just the header marker. Same `registerConnection → (config) → wait → unregister` sequence as QUIC (§2.3).

### 4.3 Request/response mapping (differences vs QUIC)

- Request classification (`determineHTTP2Type`): `...: update-configuration` → config update; `...: websocket` → websocket; presence of `Cf-Cloudflared-Proxy-Src` header → TCP (`IsTCPStream`); `...: control-stream` → control; else plain HTTP.
- Plain HTTP/WebSocket: `originProxy.ProxyHTTP(respWriter, tr, isWebsocket)`; the upgrade header is stripped before proxying.
- TCP (warp routing): `TCPRequest{Dest, CFRay, LBProbe, CfTraceID, ConnIndex}` → `originProxy.ProxyTCP(ctx, rws, ...)` with `rws = NewHTTPResponseReadWriteAcker(respWriter, respWriter, r)`.
- Response writing (`http2RespWriter.WriteRespHeaders`):
  - `content-length` passes through as a real H2 header;
  - `cf-int-cloudflared-tracing` promoted to canonical `Cf-Int-Cloudflared-Tracing-*` header;
  - **all other origin headers are base64-serialized into the single header `Cf-Cloudflared-Response-Headers`** (`SerializeHeaders`, `connection/header.go:113-146`) to dodge H2 header validation — wire-compat requires reproducing this exactly (request direction symmetric: user headers travel base64 in `cf-cloudflared-request-headers`);
  - sets `Cf-Cloudflared-Response-Meta: {"src":"origin"}`;
  - `101 Switching Protocols` remapped to **200** (H2 has no 101);
  - trailers via `http2.TrailerPrefix + name`.
- Errors before status written: 502 + `Cf-Cloudflared-Response-Meta: {"src":"cloudflared",...}`; if status already sent, abort handler.
- Streaming: `TypeWebsocket|TypeTCP|TypeControlStream` set `shouldFlush`; `Write()` flushes after every write. `shouldFlush(headers)` also enables flush-on-write for missing content-length / chunked / SSE / gRPC / ndjson.
- `Hijack()` returns a `localProxyConnection` (net.Conn facade over respWriter) + `bufio.ReadWriter` so websocket/TCP streams pipe bidirectionally over the H2 stream body.
- QUIC framing differences recap: QUIC uses signature+version+capnp ConnectRequest/ConnectResponse and metadata headers; H2 uses real H2 request/response with internal headers; the capnp registration RPC itself is byte-identical framing on both.

---

## 5. Tunnel credentials

No `NamedTunnel`/`QuickTunnel` structs in this checkout; current shape:

- **Credentials file** (`connection/connection.go:64-77`) — **no JSON tags**, so Go field names (PascalCase) are the JSON keys; `TunnelSecret []byte` is standard-base64 by `encoding/json`:
  ```go
  type Credentials struct {
      AccountTag   string      // "AccountTag"
      TunnelSecret []byte      // "TunnelSecret"
      TunnelID     uuid.UUID   // "TunnelID"
      Endpoint     string      // "Endpoint" (region override, optional)
  }
  ```
  Confirmed by `writeTunnelCredentials` and test fixture `{"AccountTag":..., "TunnelSecret":..., "TunnelID":..., "TunnelName":...}` (`TunnelName` ignored by unmarshal).
- **Token form** (`TunnelToken`, `connection.go:78-97`):
  ```go
  type TunnelToken struct {
      AccountTag   string    `json:"a"`
      TunnelSecret []byte    `json:"s"`
      TunnelID     uuid.UUID `json:"t"`
      Endpoint     string    `json:"e,omitempty"`
  }
  ```
  Token string = `base64.StdEncoding(json)`; `Credentials()` converts back; `Endpoint` drives region selection for edge discovery.
- **QuickTunnel API result** JSON keys: `id`, `name`, `hostname`, `account_tag`, `secret` (snake_case, §3.2).
- `TunnelProperties { Credentials Credentials; QuickTunnelUrl string }` is the single tunnel object passed to connection code.
- Named-tunnel credentials file: `json.Unmarshal` into `Credentials`; backwards-compat enriches missing `TunnelID` from the CLI arg; token-based run = `ParseToken` → `Credentials()`.
- Registration consumes `Credentials.Auth()` (→ `TunnelAuth{AccountTag, TunnelSecret}`) + `Credentials.TunnelID` (16 raw UUID bytes) on both transports.

---

## 6. Origin handler interface (cloudflared's shape)

Transport-neutral contract (`connection/connection.go:151-171`):

```go
type OriginProxy interface {
    ProxyHTTP(w ResponseWriter, tr *tracing.TracedHTTPRequest, isWebsocket bool) error
    ProxyTCP(ctx context.Context, rwa ReadWriteAcker, req *TCPRequest) error
}
type TCPRequest struct { Dest, CFRay, LBProbe, FlowID, CfTraceID string; ConnIndex uint8 }
type ReadWriteAcker interface { io.ReadWriter; AckConnection(tracePropagation string) error }
type ResponseWriter interface {
    WriteRespHeaders(status int, header http.Header) error
    AddTrailer(trailerName, trailerValue string)
    http.ResponseWriter; http.Hijacker; io.Writer
}
type Orchestrator interface {
    UpdateConfig(version int32, config []byte) *pogs.UpdateConfigurationResponse
    GetConfigJSON() ([]byte, error)
    GetOriginProxy() (OriginProxy, error)
}
```

- QUIC: `dispatchRequest` switches on `ConnectRequest.Type` (`ConnectionTypeHTTP/Websocket/TCP`), rebuilds `http.Request` from metadata (`HttpHeader:<name>`/`HttpMethod`/`HttpHost`/`dest`), wraps stream in `httpResponseAdapter` (ResponseWriter) or `streamReadWriteAcker` (ReadWriteAcker); `AckConnection` writes the capnp `ConnectResponse`.
- H2: `ServeHTTP` calls `ProxyHTTP`/`ProxyTCP` with `http2RespWriter`.

Concrete dispatcher `proxy.Proxy` matches ingress rules, which select among three **service capability interfaces** (`ingress/origin_proxy.go:12-31`):

```go
type HTTPOriginProxy interface { http.RoundTripper }          // http/https/ws/wss, unix:, http_status:
type StreamBasedOriginProxy interface {
    EstablishConnection(ctx, dest string, log) (OriginConnection, error)
}
type HTTPLocalProxy interface { http.Handler }                // management service
type OriginConnection interface {
    Stream(ctx context.Context, tunnelConn io.ReadWriter, log *zerolog.Logger)
    Close() error
}
```

- **HTTP**: `RoundTrip`; for websocket re-sets `Connection: Upgrade`, `Upgrade: websocket`, `Sec-Websocket-Version: 13`, zeroes content-length/body; copies headers; `WriteRespHeaders(resp.StatusCode, headers)`; on **101** pipes the response body with a bidirectional stream; otherwise copies body + trailers. Adds `Cf-Warp-Tag-*` headers.
- **WebSocket (cloudflared-originated, wss service)**: rewrites `ws→http`, `wss→https`; after `RoundTrip` returns 101, `bidirectionalStream{writer: w, reader: tr.Body}`.
- **TCP (warp-routing)**: `ProxyTCP` parses `req.Dest` with `netip.ParseAddrPort`, dials via `ingress.OriginTCPDialer` (`originDialer.DialTCP(ctx, dest)`), `AckConnection`, then `stream.Pipe` — raw bidirectional bytes, no framing.
- **TCP-over-WSS** (ssh/rdp/smb/tcp/bastion): `EstablishConnection` wraps the TCP conn in a websocket and runs `DefaultStreamHandler` or `socks.StreamHandler` (SOCKS server lives in an `OriginConnection.Stream`).
- Ingress rule matching: `FindMatchingRule(hostname, path)`, wildcard `*.` host + path regex; last rule must be catch-all; no rules → `http_status:503`. Proxy swapped atomically on config updates.
- Quick-tunnel HTTP path = single origin (`<rand>.trycloudflare.com`) → the consumer's origin handler; libcfd's public API will expose an origin-handler seam equivalent to `OriginProxy`/`ResponseWriter`/`ReadWriteAcker`.

---

## 7. Implementation sequencing (for worker briefs)

Recommended order, each step independently testable against the facts above:

1. **capnp layer**: codegen `tunnelrpc.capnp` + `quic_metadata_protocol.capnp` (+ std `rpc.capnp`); implement unpacked stream framing exactly (4-byte LE segment count, per-segment LE word sizes, 8-byte header padding, single-segment messages, whole-message writes).
2. **Minimal capnp RPC client**: bootstrap → return → call → return → finish, sequential, ids from 0, interfaceId `0xf71695ec7fe85497`. Must also decode `ConnectionResponse` union, `ConnectionError`, `ConnectionDetails`.
3. **Quick tunnel API client**: POST `/tunnel` per §3.1-3.2; parse fields; build Credentials; token encode/decode per §5.
4. **Edge discovery**: SRV `_v2-origintunneld._tcp.argotunnel.com` (with region prefix + DoT fallback), port from SRV (7844), region1-first selection, ≥2 targets requirement.
5. **QUIC connection (quiche)**: TLS with SNI `quic.cftunnel.com`, ALPN `["argotunnel"]`; config per §1.3 (idle 5s, keepalive 1s, datagrams on, generous flow-control windows, no 0-RTT); open first bidi stream → registration RPC (§2.3); then accept streams, dispatch on 6-byte signature; data path per §1.5; keepalive/idle → retry + edge rotation semantics per §2.6.
6. **Origin seam**: public `OriginProxy`-equivalent (HTTP handler + ReadWriteAcker for TCP/WS), transport-neutral, runtime-agnostic.
7. **H2 edge**: serve h2 on TLS (SNI `h2.cftunnel.com`, no ALPN), classify streams by `Cf-Cloudflared-Proxy-Connection-Upgrade` + `Cf-Cloudflared-Proxy-Src`, control-stream body transport for RPC, response header base64 serialization per §4.3.
8. **Graceful shutdown/unregister + retry/backoff**: unregister RPC (methodId 1), grace period 30s, `EDUPCONN` → new address, QUIC→H2 fallback.

Review checkpoints (wire-compat): capnp frame bytes of a registerConnection call; the 6+2 byte preamble + ConnectRequest on data streams; metadata key spelling (`HttpHeader:` prefix, `HttpStatus`); H2 control-stream header string; response header base64 scheme; `Cf-Cloudflared-Response-Meta` JSON.

## 8. Open questions / risks carried into implementation

- Edge-side (origintunneld) code is not in this repo; control-stream byte-0 = bootstrap and tolerance of the data-stream preamble are inferred from cloudflared's client only.
- Tunnel secret byte length unenforced in Go (16 vs 32 per briefs) — verify against live edge.
- H2 ALPN: cloudflared sets none; if the edge requires ALPN `h2` to select h2, the Rust server must still offer/accept it correctly when the edge presents it (x/net/http2 uses `"h2"`).
- `MaxIncomingStreams = 1<<60` is a quic-go cap, not an RFC value — negotiate a compatible `max_streams_bidi` on the Rust side.
- Datagram MTU sizes are platform-dependent (1350/1280 unix vs 1220/1201 Windows); v2 only for the default feature set.
- 0-RTT must stay off.
- Full capnp RPC (bootstrap/capability tables/release/finish) is required on the control stream — a raw-marshal shortcut will not interoperate with the edge's capnproto2 RPC peer.
