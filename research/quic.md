# QUIC Edge Connection Transport — Technical Brief

Reference checkout: `/home/aperso/libcfd/research/cloudflared` at commit `61a0b0b3` (2026-08-11).
IMPORTANT LAYOUT NOTE: this checkout is newer than the file names in the task. There is NO `connection/edge.go`, `connection/dial.go`, `connection/control*.go` (except `control.go`), or `connection/request*.go`, and no `quic/request.go`. The QUIC transport logic lives in:
- `connection/quic.go` (UDP socket + `quic.Dial`)
- `connection/quic_connection.go` (connection serve loop, stream handling, HTTP request reconstruction)
- `connection/control.go` (registration / control stream logic)
- `tunnelrpc/quic/protocol.go` (stream protocol signatures / versioning)
- `tunnelrpc/quic/request_client_stream.go`, `request_server_stream.go` (request framing)
- `tunnelrpc/quic/cloudflared_server.go`, `cloudflared_client.go`, `session_client.go`, `session_server.go` (capnp RPC on QUIC streams)
- `tunnelrpc/proto/quic_metadata_protocol.capnp`, `tunnelrpc/proto/tunnelrpc.capnp` (schemas)
- `tunnelrpc/pogs/*.go` (capnp <-> Go conversions)
- `quic/constants.go`, `quic/safe_stream.go`, `quic/datagram.go`, `quic/datagramv2.go`, `quic/param_unix.go`
- `quic/v3/*.go` (datagram v3)
- `supervisor/tunnel.go` (QUIC config + dial orchestration)
- `connection/protocol.go` (ALPN/SNI constants)

quic-go is a FORK: `replace github.com/quic-go/quic-go => github.com/chungthuang/quic-go v0.45.1-0.20250428085412-43229ad201fd` (based on quic-go v0.45), declared in `go.mod` alongside `github.com/quic-go/quic-go v0.52.0` (the fork wins). Vendored under `vendor/github.com/quic-go/quic-go/`.

---

## 1. ALPN values and QUIC version negotiation

### ALPN
- ALPN string is exactly `"argotunnel"`: `quicProtos = "argotunnel"` (`connection/protocol.go:38`).
- `Protocol.TLSSettings()` for QUIC returns `{ServerName: "quic.cftunnel.com", NextProtos: []string{"argotunnel"}}` (`connection/protocol.go:74-78`).
- Applied in `prepareTunnelConfig` (`cmd/cloudflared/tunnel/configuration.go:157-168`): for each protocol builds `tlsconfig.CreateTunnelConfig(flags.CACert, tlsSettings.ServerName)` and, if `NextProtos` non-empty, sets `edgeTLSConfig.NextProtos = tlsSettings.NextProtos`.
- Tests use `[]string{"argotunnel"}` too (`connection/quic_connection_test.go:752`, `quic/safe_stream_test.go:64`). There is no `h2`/`http/1.1` on the QUIC config.
- HTTP/2 edge transport uses SNI `h2.cftunnel.com` and NO NextProtos override (`connection/protocol.go:33,70-73`); probes use `probe.cftunnel.com`.

### QUIC version negotiation
- cloudflared does NOT pin QUIC versions: `quic.Config.Versions` is never set (`supervisor/tunnel.go:584-594`). quic-go's `populateConfig` defaults `Versions` to `protocol.SupportedVersions` (`vendor/.../quic-go/config.go:64-67`).
- `SupportedVersions = {Version1, Version2}` where `Version1 = 0x1`, `Version2 = 0x6b3343cf` (`vendor/.../quic-go/internal/protocol/version.go:24-31`).
- The client sends its Initial packet with `conf.Versions[0]`, i.e. QUIC v1 (`vendor/.../quic-go/transport.go:260`, `doDial(..., conf.Versions[0], ...)` at `transport.go:250-251`). Version negotiation is the standard QUIC Version Negotiation packet mechanism handled by quic-go; cloudflared adds no custom transport parameters, no `qc_metadata`, and no application-level version exchange for the connection itself.
- The only application-level "version" is the per-stream `protocolV1 = "01"` (see §4). Handshake idle timeout 5s (`quic/constants.go:5`).

---

## 2. TLS client config used to dial the edge

Built by `tlsconfig.CreateTunnelConfig(caCert, serverName)` (`tlsconfig/certreloader.go:130-151`) → `GetConfig` (`tlsconfig/tlsconfig.go:40-88`):
- `ServerName`: `"quic.cftunnel.com"` (SNI).
- Root CAs: `x509.SystemCertPool()` PLUS the built-in Cloudflare origin roots from `cloudflareRootCA` PEM block (`tlsconfig/cloudflare_ca.go:9-105`, 3 certs: "CloudFlare Origin SSL ECC Certificate Authority", "CloudFlare Origin SSL Certificate Authority", CN=`origin-pull.cloudflare.net`). If `--ca-cert` is given, that file is appended to the pool instead of the system pool being used alone (system pool + CF roots + custom cert).
- `MinVersion`/`MaxVersion` left zero; quic-go enforces TLS 1.3. No client certs, no `InsecureSkipVerify`.
- Curve preferences: default `[tls.CurveP256]` (`tlsconfig/tlsconfig.go:76-79`) but overridden per connection by `cfdcrypto.TLSConfigWithCurvePreferences(config, pqMode)` (`crypto/curves.go:67-88`, clones the shared config):
  - `PostQuantumPrefer` (default): `[tls.X25519MLKEM768 (0x11ec), P256Kyber768Draft00 (0xfe32 = 65074), tls.CurveP256]` (`crypto/curves.go:37-40`).
  - `PostQuantumStrict` (with `--post-quantum`): `[tls.X25519MLKEM768, P256Kyber768Draft00]` only (`crypto/curves.go:31-34`).
- Dial entry point: `connection.DialQuic(ctx, quicConfig, tlsConfig, edgeAddr netip.AddrPort, localAddr, connIndex, logger, opts)` (`connection/quic.go:28-46`) → `quic.Dial(ctx, udpConn, net.UDPAddrFromAddrPort(edgeAddr), tlsConfig, quicConfig)`.
- UDP socket: `createUDPConnForConnIndex` (`connection/quic.go:50-96`): per-connIndex port reuse (first socket's ephemeral port is cached in `portForConnIndex`, so HA connections share a local port); binds `localIP` if set; network `"udp"` except macOS where `udp4`/`udp6` is used to set the DF bit (quic-go#3793); `dialopts.SkipPortReuse` → ephemeral port, used by probes (`connection/dialopts/dialopts.go:5-10`).

---

## 3. Control stream / registration flow

### Stream identity
- cloudflared (client) opens the FIRST stream of the QUIC connection and uses it as the control/registration stream: `controlStream, err := q.conn.OpenStream()` (`connection/quic_connection.go:121-125`). Comment at `connection/quic_connection.go:119-120`: "The edge assumes the first stream is used for the control plane".
- The control stream carries **capnp RPC** (not HTTP/3, not raw capnp): `tunnelrpc.NewRegistrationClient(ctx, stream, timeout)` (`tunnelrpc/registration_client.go:36-41`) wraps the stream in `rpc.StreamTransport` (`tunnelrpc/utils.go:33-50`, via `SafeTransport`) and does `conn.Bootstrap(ctx)` then `pogs.NewRegistrationServer_PogsClient`.
- CRITICAL ASYMMETRY: **no protocol signature bytes are written on the registration stream.** Unlike session/configuration RPC streams, `NewRegistrationClient` writes nothing before the capnp bootstrap message. The edge-side server `tunnelrpc/registration_server.go:14-33` (`RegistrationServer.Serve`) also does no signature check — it just serves capnp RPC directly.
- capnp RPC framing over the stream: standard unpacked Cap'n Proto stream framing, each message = `[4-byte LE (numSegments-1)] [4-byte LE segment size in words] [pad to 8-byte alignment] [segment data]` (`vendor/.../capnproto2/mem.go:693-733`, header sizes `msgHeaderSize=4`, `segHeaderSize=4` at `mem.go:798-801`). All cloudflared messages are single-segment (`capnp.NewMessage(capnp.SingleSegment(nil))`, e.g. `tunnelrpc/pogs/quic_metadata_protocol.go:93-97`). `rpc.StreamTransport` buffers each whole message and writes it in one `Write` (`vendor/.../capnproto2/rpc/transport.go:48-56`).

### Message order (client → edge)
1. `RegisterConnection(ctx, auth, tunnelID, connIndex, options)` — `tunnelrpc/registration_client.go:47-69`. capnp method `registerConnection @0 (auth :TunnelAuth, tunnelId :Data, connIndex :UInt8, options :ConnectionOptions) -> (result :ConnectionResponse)` (`tunnelrpc/proto/tunnelrpc.capnp:114-118`).
   - `TunnelAuth` = `{accountTag :Text, tunnelSecret :Data}` (16-byte secret) (`tunnelrpc.capnp:109-113`).
   - `tunnelId` = 16 raw bytes of the tunnel UUID (`pogs/registration_server.go:101-107`).
   - `ConnectionOptions` = `{client :ClientInfo, originLocalIp :Data, replaceExisting :Bool, compressionQuality :UInt8, numPreviousAttempts :UInt8}` (`tunnelrpc.capnp:64-77`). `ClientInfo` = `{clientId :Data (16-byte connector UUID), features :List(Text), version :Text, arch :Text}` (`tunnelrpc.capnp:53-62`, populated at `client/config.go:47-60`).
   - Features list sent at registration (default set): `["allow_remote_config", "serialized_headers", "support_datagram_v2", "support_quic_eof", "management_logs"]` (`features/features.go:5-26`), plus `"postquantum"` and `"support_datagram_v3_2"` when enabled via feature selector.
   - Response: `ConnectionResponse` union of `error :ConnectionError` (`{cause :Text, retryAfter :Int64, shouldRetry :Bool}`) or `connectionDetails :ConnectionDetails` (`{uuid :Data, locationName :Text, tunnelIsRemotelyManaged :Bool}`) (`tunnelrpc.capnp:79-107`; conversion `pogs/registration_server.go:130-187`). `shouldRetry`/`retryAfter` are only set when error is `*RetryableError` (`pogs/registration_server.go:201-207`).
2. Duplicate-connection detection: if the error cause string equals `DuplicateConnectionError = "EDUPCONN"` (`connection/errors.go:7`), cloudflared returns `errDuplicationConnection` and the supervisor tries a different edge address (`connection/control.go:93-99`; `supervisor/tunnel.go:196-204`).
3. If `connIndex == 0` and `!tunnelIsRemotelyManaged`: `SendLocalConfiguration(ctx, configJSON)` → `updateLocalConfiguration @2 (config :Data)` (`connection/control.go:101-113`).
4. Then block on `waitForUnregister` (`connection/control.go:128-155`): waits for ctx.Done or the graceful-shutdown signal; then calls `GracefulShutdown` → `UnregisterConnection @1 ()` (`tunnelrpc/registration_client.go:77-94`) with timeout = grace period (default 30s, `cmd/cloudflared/tunnel/cmd.go` `GracePeriod` default `time.Second*30`).

### Timeouts
- RPC timeout default: 5s (`--rpc-timeout` flag default `5 * time.Second`, `cmd/cloudflared/tunnel/cmd.go:785-789`). Every registration RPC wraps ctx with `context.WithTimeout(ctx, r.requestTimeout)` (`tunnelrpc/registration_client.go:49-52, 79-81`).
- After unregistration (control stream returns nil), `quicConnection.Serve` waits the full grace period before canceling the group (`connection/quic_connection.go:130-141`).
- The read-side retry wrapper: `readWriterSafeTemporaryErrorCloser` retries temporary read errors with `defaultSleepBetweenTemporaryError = 500ms`, `defaultMaxRetries = 3` (`tunnelrpc/utils.go:12-32`).

---

## 4. Request delivery (per-QUIC-stream application protocol)

### Stream dispatch
- After registration, cloudflared runs `acceptStream` (`connection/quic_connection.go:166-183`): loops `q.conn.AcceptStream(ctx)`, spawning `go q.runStream(stream)` per stream. `MaxIncomingStreams = 2^60` means the edge is allowed to open effectively unlimited streams.
- `runStream` (`connection/quic_connection.go:185-208`): wraps the stream in `cfdquic.NewSafeStreamCloser` (write deadline = `streamWriteTimeout`, default 0 = disabled), then `rpcquic.NewCloudflaredServer(q.handleDataStream, q.datagramHandler, q, q.rpcTimeout).Serve(ctx, noCloseStream)`.

### Stream type negotiation: 6-byte magic signature + 2-byte version
`tunnelrpc/quic/protocol.go`:
- `dataStreamProtocolSignature = {0x0A, 0x36, 0xCD, 0x12, 0xA1, 0x3E}` (protocol.go:13)
- `rpcStreamProtocolSignature = {0x52, 0xBB, 0x82, 0x5C, 0xDB, 0x65}` (protocol.go:17)
- `protocolV1 = "01"` (2 ASCII bytes `0x30 0x31`) (protocol.go:25-26)
- `CloudflaredServer.Serve` reads exactly the first 6 bytes (`determineProtocol`, protocol.go:33-47) and branches:
  - data signature → `RequestServerStream` handler (HTTP/WS/TCP request)
  - rpc signature → capnp RPC server for `SessionManager` + `ConfigurationManager` (`cloudflared_server.go:42-68`, with `context.WithTimeout(ctx, s.responseTimeout)` — "Every new quic.Stream request aligns to a new RPC request")

### Request stream wire format (edge → cloudflared)
Written by edge via `RequestClientStream.WriteConnectRequestData` (`tunnelrpc/quic/request_client_stream.go:19-41`), read by cloudflared via `RequestServerStream.ReadConnectRequestData` (`tunnelrpc/quic/request_server_stream.go:26-43`):

```
[6 bytes dataStreamProtocolSignature 0A 36 CD 12 A1 3E]
[2 bytes version "01" = 0x30 0x31]
[raw capnp message: ConnectRequest]     <- standard capnp stream framing (see §3)
[raw request body bytes ...]            <- unframed, streamed until EOF
```

- `ConnectRequest` schema (`tunnelrpc/proto/quic_metadata_protocol.capnp:9-20`):
  - `dest @0 :Text` — full URL for HTTP/WS ("http://host/path..."), `host:port` for TCP
  - `type @1 :ConnectionType` — enum `http @0`, `websocket @1`, `tcp @2`
  - `metadata @2 :List(Metadata)` — `Metadata { key @0 :Text; val @1 :Text }`
- Metadata keys for HTTP (constants at `connection/quic_connection.go:32-42`):
  - `"HttpMethod"` — HTTP method
  - `"HttpHost"` — value of the `Host` header
  - `"HttpHeader:<name>"` — one entry per header value (response side likewise); keys are `HTTPHeaderKey = "HttpHeader"` prefixed with `:` separator, i.e. metadata key string is `"HttpHeader:" + headerName` (see `WriteRespHeaders` at `connection/quic_connection.go:262-272` and parsing at `connection/quic_connection.go:315-325`).
  - `"FlowID"` — `QUICMetadataFlowID = "FlowID"` for TCP (connection/quic_connection.go:42)
  - `"cf-trace-id"` — `TracerContextName` (`tracing/tracing.go:28`), the trace context string
- Response (cloudflared → edge) written by `httpResponseAdapter`:
  - `WriteRespHeaders(status, header)` → `WriteConnectResponseData(nil, metadata...)` with metadata `"HttpStatus"` = decimal status string and `"HttpHeader:<name>"` per header value (`connection/quic_connection.go:255-272`).
  - `ConnectResponse` schema: `{error @0 :Text; metadata @1 :List(Metadata)}` (`quic_metadata_protocol.capnp:22-26`).
  - Preamble on response is identical: signature + version + capnp `ConnectResponse`, then raw body bytes. Written by `RequestServerStream.WriteConnectResponseData` (`request_server_stream.go:46-66`). The preamble is written once; subsequent body writes go straight to the stream.
  - Errors before ack: `WriteErrorResponse` sends `WriteConnectResponseData(err, {"HttpStatus":"502"})` (`connection/quic_connection.go:293-297`). `ErrorFlowConnectRateLimitedMetadata = {"FlowConnectRateLimited", "true"}` appended on flow-limiter rejection (`connection/quic_connection.go:229-232`, `tunnelrpc/pogs/quic_metadata_protocol.go:16-18`).
  - If the connect response was already sent and a later error occurs, `runStream` does `quicStream.CancelWrite(0)` → RST_STREAM (`connection/quic_connection.go:203-206`).
- Request body: NOT chunked/framed. The `RequestServerStream` itself is the body `io.ReadCloser` (`buildHTTPRequest(ctx, request, stream, ...)` at `connection/quic_connection.go:298`); body ends at stream EOF. Content-Length / Transfer-Encoding handling is only done to configure Go's `http.Request` client semantics (`connection/quic_connection.go:327-352`).
- No per-stream control messages (no ping/backpressure frames on data streams). Backpressure is pure QUIC stream flow control (see §6). Trailers are not supported over QUIC (`AddTrailer` is a no-op, `connection/quic_connection.go:251-253`).
- TCP streams (`ConnectionTypeTCP`): same ConnectRequest handshake (dest=`host:port`), then raw bidirectional bytes; `AckConnection(tracePropagation)` sends a ConnectResponse (nil error, optional `cf-int-cloudflared-tracing` metadata) after the origin accepts (`streamReadWriteAcker`, `connection/quic_connection.go:269-278`).
- HTTP upgrades: websocket via `ConnectionTypeWebsocket`, `stripWebsocketUpgradeHeader` removes Upgrade/Connection headers before origin dispatch (`connection/quic_connection.go:341`).

### RPC streams (edge → cloudflared), for UDP session mgmt and remote config
- Client (edge side, in cloudflared the datagram v2 handler): `NewSessionClient`/`NewCloudflaredClient` write the 6-byte `rpcStreamProtocolSignature` FIRST (`tunnelrpc/quic/session_client.go:25-31`, `cloudflared_client.go:26-32`), then capnp RPC (no version bytes). `SessionManagerServer.Serve`/`CloudflaredServer.handleRPC` verify the signature via `determineProtocol` (`session_server.go:27-43`, `cloudflared_server.go:42-49`).
- `SessionManager` RPCs: `registerUdpSession @0 (sessionId :Data, dstIp :Data, dstPort :UInt16, closeAfterIdleHint :Int64, traceContext :Text) -> (RegisterUdpSessionResponse {err :Text, spans :Data})`, `unregisterUdpSession @1 (sessionId :Data, message :Text)` (`tunnelrpc.capnp:132-142`).
- `ConfigurationManager`: `updateConfiguration @0 (version :Int32, config :Data) -> (UpdateConfigurationResponse {latestAppliedVersion :Int32, err :Text})` (`tunnelrpc.capnp:158-169`). Handled by `quicConnection.UpdateConfiguration` → `orchestrator.UpdateConfig` (`connection/quic_connection.go:270-273`).

---

## 5. Connection-level control messages

- `registerConnection`, `unregisterConnection`, `updateLocalConfiguration` — capnp RPC on stream 0 (see §3).
- Graceful shutdown: SIGINT/SIGTERM → `gracefulShutdownC` fires → `waitForUnregister` calls `UnregisterConnection` RPC (timeout = grace period). No dedicated QUIC control frame.
- Ping/keepalive: no application-level ping. QUIC keepalive via `quic.Config.KeepAlivePeriod = 1s` (`MaxIdlePingPeriod`, `quic/constants.go:7`, applied `supervisor/tunnel.go:588`) and `MaxIdleTimeout = 5s` (`quic/constants.go:6`). `quic.IdleTimeoutError` after 5s idle triggers connection teardown and (in the supervisor) edge-address rotation (`supervisor/tunnel.go:196-204`).
- New connection / HA: `HAConnections` (default 4, `cmd.go:782-785`) separate connections, each with its own connIndex and control stream; edge assigns one UDP session/stream per connection. No QUIC-level "new connection" message.
- Connection close: `quicConnection.Close()` → `CloseWithError(0, "")` (`connection/quic_connection.go:150-152`). Error path: `quicStream.CancelWrite(0)` for failed streams.
- UDP/ICMP datagrams (connection-level, see §7) and per-connection datagram session registration are the remaining control-plane messages, transported as QUIC DATAGRAM frames.

---

## 6. QUIC connection configuration

From `serveQUIC` (`supervisor/tunnel.go:584-600`), using `quic.Config` (quic-go fork v0.45):

| Field | Value | Source |
|---|---|---|
| `HandshakeIdleTimeout` | 5s | `quic/constants.go:5` |
| `MaxIdleTimeout` | 5s | `quic/constants.go:6` |
| `KeepAlivePeriod` | 1s | `quic/constants.go:7` |
| `MaxIncomingStreams` | `1 << 60` (2^60, quic-go max) | `quic/constants.go:10` |
| `MaxIncomingUniStreams` | `1 << 60` | same constant |
| `EnableDatagrams` | `true` (required for UDP/ICMP) | `supervisor/tunnel.go:590` |
| `DisablePathMTUDiscovery` | flag `--quic-disable-pmtu-discovery`, default false | `supervisor/tunnel.go:591` |
| `MaxConnectionReceiveWindow` | default 30 MiB (`30*(1<<20)`), flag `--quic-connection-level-flow-control-limit` | `cmd/cloudflared/tunnel/cmd.go:800-805` |
| `MaxStreamReceiveWindow` | default 6 MiB (`6*(1<<20)`), flag `--quic-stream-level-flow-control-limit` | `cmd/cloudflared/tunnel/cmd.go:807-813` |
| `InitialPacketSize` | 1252 (IPv6 edge) / 1232 (IPv4 edge); reduced from quic-go's 1280 default for WARP MTU 1280 | `supervisor/tunnel.go:578-583` |
| `Allow0RTT` | not set → 0-RTT disabled (no `TokenStore` either) | — |
| `Versions` | not set → quic-go defaults `{Version1 0x1, Version2 0x6b3343cf}`; client sends v1 | §1 |

- Not set: `InitialStreamReceiveWindow`, `InitialConnectionReceiveWindow`, `AllowConnectionWindowIncrease`, `MaxIncomingUniStreams` default overrides — quic-go defaults apply.
- `quic.Config.Tracer`: `quicpogs.NewClientTracer(logger, connIndex)` for Prometheus frame/packet metrics (`quic/metrics.go`).
- Datagram sizing constants (`quic/param_unix.go:4-8`): `MaxDatagramFrameSize = 1350`, `maxDatagramPayloadSize = 1280` (Windows: `MaxDatagramFrameSize = 1220`, payload `1220-3-16-1`).
- UDP dial: local socket per connIndex with port reuse; `net.ListenUDP` bound to `edgeBindAddr` if configured.

---

## 7. Connection metadata transmission

- The hostname/tunnel identity is NOT carried in QUIC transport parameters and NOT in a first frame on the control stream. It is carried in the `registerConnection` capnp RPC params: `TunnelAuth{accountTag, tunnelSecret}` + `tunnelId` (16-byte UUID) + `connIndex` (`tunnelrpc.capnp:114-118`; `connection/connection.go:34-41` `Credentials.Auth()`).
- Per-connection client info rides in `ConnectionOptions.client` (`ClientInfo{clientId, features, version, arch}`) (`tunnelrpc.capnp:53-62`; `client/config.go:47-60`). `originLocalIp` is the resolved local edge-facing IP (`supervisor/tunnel.go:42-45`). `numPreviousAttempts` from backoff counter.
- Per-request metadata (host, method, headers, flow id, trace id) rides in the `ConnectRequest.metadata` list on each data stream (§4). This is what the schema file `quic_metadata_protocol.capnp` is for.
- There is no QUIC transport-parameter extension. The only transport-level knob is the 6-byte stream signature choosing data vs RPC protocol.

---

## 8. Datagram framing (UDP/ICMP over QUIC DATAGRAM) — summary

Datagram version selected by feature negotiation (`features/features.go:61-68`): `support_datagram_v2` (default) or `support_datagram_v3_2` (newer).

### v2 (`quic/datagramv2.go`, `connection/quic_datagram_v2.go`)
- Outgoing UDP payload: `[payload][16-byte session UUID suffix][1-byte type]` (`SuffixSessionID` + `SuffixType`; `quic/datagram.go:105-123`, `quic/datagramv2.go:30-46`). `DatagramTypeUDP = 0`, `DatagramTypeIP = 1`, `DatagramTypeIPWithTrace = 2`, `DatagramTypeTracingSpan = 3` (`quic/datagramv2.go:9-22`). Max datagram payload 1280.
- UDP sessions are registered via the `SessionManager` RPC stream (edge → cloudflared `registerUdpSession`) plus a datagram registration handshake (v2 uses RPC only; v3 uses datagrams).

### v3 (`quic/v3/*`, `connection/quic_datagram_v3.go`)
- First byte is `DatagramType` (`quic/v3/datagram.go:7-14`): `0x0` UDP session registration, `0x1` UDP session payload, `0x2` ICMP, `0x3` UDP session registration response.
- `UDPSessionPayloadDatagram`: `[type 0x1][16-byte RequestID BE][payload]`; header `DatagramPayloadHeaderLen = 17`, max payload+header 1297 (`quic/v3/datagram.go:145-147`, `request.go:5-8`).
- `UDPSessionRegistrationDatagram`: `[type 0x0][flags 1B][dst port u16 BE][idle seconds u16 BE][RequestID 16B][dst IP 4 or 16B][optional bundled payload]`; flags: bit0 IPv6, bit1 traced, bit2 bundled (`quic/v3/datagram.go:31-49, 53-142`).
- Registration response: `[type 0x3][resp type 1B][RequestID 16B][errMsgLen u16 BE][errMsg]`; resp types `0x00 ok, 0x01 dst unreachable, 0x02 unable to bind, 0x03 too many flows, 0xff error with msg` (`quic/v3/datagram.go:204-287`).
- v3 UDP sessions are registered purely by datagram (RPC `registerUdpSession` returns `ErrUnsupportedRPCUDPRegistration`, `connection/quic_datagram_v3.go:11-17`).

---

## 9. Edge discovery (for completeness)

- SRV lookup `_v2-origintunneld._tcp.argotunnel.com` (`edgediscovery/allregions/discovery.go:16-20`), regional variant `"<region>-v2-origintunneld"` (`regions.go:135-144`), fallback to DoT `cloudflare-dns.com`. Each SRV target resolves to an `EdgeAddr{TCP, UDP}` with the SAME port (production UDP port 7844) (`discovery.go:172-193`). HA conns are spread across 2 regions; per-connIndex UDP socket port reuse enables consistent edge binding.
- Protocol selection: `auto` (default) starts with QUIC for token-based tunnels (`connection/protocol.go:196-201`); on `quic.IdleTimeoutError`/transport "operation not permitted" (`isQuicBroken`, `supervisor/tunnel.go:301-313`) fallback to HTTP2.

---

## Key files another agent should open first

1. `connection/quic_connection.go` — the whole client-side QUIC connection: control stream, stream accept loop, request dispatch, HTTP request/response reconstruction. Read this first.
2. `tunnelrpc/quic/protocol.go` + `request_server_stream.go` + `request_client_stream.go` — exact wire framing to reimplement.
3. `tunnelrpc/proto/tunnelrpc.capnp` + `quic_metadata_protocol.capnp` — schemas (TypeIDs: ConnectRequest `0xc47116a1045e4061`, ConnectResponse `0xb1032ec91cef8727`, ConnectionType `0xc52e1bac26d379c8`, Metadata `0xe1446b97bfd1cd37`).
4. `supervisor/tunnel.go` (serveQUIC, 560-660) — QUIC config and dial wiring.
5. `connection/control.go` + `tunnelrpc/registration_client.go` — registration RPC order and timeouts.
6. `connection/protocol.go`, `tlsconfig/*.go`, `crypto/curves.go` — ALPN/SNI/TLS.

## Residual risks / open questions for the Rust reimplementation

- The edge-side behavior (origintunneld) is not in this repo; the exact expectation for the control stream's first bytes (no signature) and whether the edge tolerates a data-stream preamble on stream 0 is inferred from cloudflared's client code only.
- capnp RPC implementation is required for the control stream (bootstrap + registerConnection + unregisterConnection + updateLocalConfiguration) and for session/configuration RPC streams. A Rust capnp implementation must reproduce the exact segment framing and RPC wire protocol of `capnproto2 v2.18.0`.
- The quic-go fork is v0.45-based; `MaxIncomingStreams = 1<<60` is a quic-go limit, not an RFC value — quic-go caps it. A Rust QUIC stack must negotiate a compatible `max_streams_bidi` transport parameter (edge sets its own limits; cloudflared only advertises its receive-side limits).
- Datagram framing sizes are platform-dependent (1350/1220 MTU). Only v2 is required by default; v3 is negotiated via the `support_datagram_v3_2` feature string sent in ConnectionOptions.
- 0-RTT is not used by cloudflared; a Rust client should not enable it either.

