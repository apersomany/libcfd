# Tunnel Registration RPC Protocol — cloudflared Reference Brief

Research scope: `/home/aperso/libcfd/research/cloudflared`. Note: there is **no `rpc/` directory** in this checkout; the equivalent code lives in `tunnelrpc/` (client, server, pogs, quic, proto) plus `connection/control.go` (the control-stream glue). The edge-side (`origintunneld`) implementation is NOT in this repo; only the cloudflared (client) side and the shared schema/protocol are available.

---

## 1. Cap'n Proto schema files

There are exactly 3 `.capnp` files (non-vendor):

| File | Contents |
|---|---|
| `tunnelrpc/proto/tunnelrpc.capnp` | All tunnel registration types + interfaces. This is the one that matters. |
| `tunnelrpc/proto/quic_metadata_protocol.capnp` | `ConnectRequest`/`ConnectResponse`/`ConnectionType`/`Metadata` — QUIC per-stream application handshake, **not** registration. |
| `tunnelrpc/proto/go.capnp` | Go codegen annotations only (`package`, `import`, `name`, ...). No wire types. |

There is **no metrics `.capnp`**. "cloudflared metrics" are Prometheus counters in `tunnelrpc/metrics/metrics.go` (namespaces `cloudflared_rpc_client_operations` etc.), used only to instrument calls.

All quotes below cite `tunnelrpc/proto/tunnelrpc.capnp` (line numbers as read) and the generated `tunnelrpc/proto/tunnelrpc.capnp.go` (which is authoritative for numeric IDs).

### 1a. Modern registration types (active path)

```capnp
// tunnelrpc.capnp (top level, after the DEPRECATED block)
struct ClientInfo @0x83ced0145b2f114b {
    clientId @0 :Data;      # 16-byte connector UUID (bytes)
    features @1 :List(Text);
    version  @2 :Text;
    arch     @3 :Text;
}
struct ConnectionOptions @0xb4bf9861fe035d04 {
    client @0 :ClientInfo;
    originLocalIp @1 :Data;          # raw IP bytes (net.IP)
    replaceExisting @2 :Bool;
    compressionQuality @3 :UInt8;    # 0 off ... 3 high
    numPreviousAttempts @4 :UInt8;
}
struct ConnectionResponse @0xdbaa9d03d52b62dc {
    result :union {
        error @0 :ConnectionError;
        connectionDetails @1 :ConnectionDetails;
    }
}
struct ConnectionError @0xf5f383d2785edb86 {
    cause @0 :Text;                  # e.g. "EDUPCONN" for duplicate connection
    retryAfter @1 :Int64;            # nanoseconds to wait before retrying
    shouldRetry @2 :Bool;
}
struct ConnectionDetails @0xb5f39f082b9ac18a {
    uuid @0 :Data;                   # 16-byte per-connection UUID
    locationName @1 :Text;           # airport code of edge colo
    tunnelIsRemotelyManaged @2 :Bool;
}
struct TunnelAuth @0x9496331ab9cd463f {
    accountTag @0 :Text;
    tunnelSecret @1 :Data;           # 32-byte tunnel secret from credentials/token
}
interface RegistrationServer @0xf71695ec7fe85497 {
    registerConnection @0 (auth :TunnelAuth, tunnelId :Data, connIndex :UInt8,
                           options :ConnectionOptions) -> (result :ConnectionResponse);
    unregisterConnection @1 () -> ();
    updateLocalConfiguration @2 (config :Data) -> ();
}
```

Method IDs are the `@N` ordinals: `registerConnection`=0, `unregisterConnection`=1, `updateLocalConfiguration`=2. Dispatch on the wire is strictly by `(interfaceId, methodId)` pair (`vendor/.../capnproto2/server/server.go:178-201`).

### 1b. Auth-related messages (all DEPRECATED — do NOT use)

```capnp
struct Authentication @0xc082ef6e0d42ed1d { key/email/originCAKey :Text }
struct TunnelRegistration @0xf41a0f001ad49e46 { err,url,logLines,permanentFailure,tunnelID,retryAfterSeconds,eventDigest,connDigest }
struct RegistrationOptions @0xc793e50592935b4a { clientId,version,os,existingTunnelPolicy,poolName,tags,connectionId,originLocalIp,isAutoupdated,runFromTerminal,compressionQuality,uuid,numPreviousAttempts,features }
enum ExistingTunnelPolicy @0x84cb9536a2cf6d3c { ignore,disconnect,balance }
struct ServerInfo @0xf2c68e2547ec3866 { locationName }
struct AuthenticateResponse @0x82c325a07ad22a65 { permanentErr,retryableErr,jwt,hoursUntilRefresh }
interface TunnelServer @0xea58385c65416035 extends (RegistrationServer) {
    registerTunnel @0 (originCert:Data, hostname:Text, options:RegistrationOptions) -> (result:TunnelRegistration);
    getServerInfo @1 () -> (result:ServerInfo);
    unregisterTunnel @2 (gracePeriodNanoSec:Int64) -> ();
    obsoleteDeclarativeTunnelConnect @3 () -> ();
    authenticate @4 (originCert:Data, hostname:Text, options:RegistrationOptions) -> (result:AuthenticateResponse);
    reconnectTunnel @5 (jwt:Data, eventDigest:Data, connDigest:Data, hostname:Text, options:RegistrationOptions) -> (result:TunnelRegistration);
}
struct Tag @0xcbd96442ae3bb01a { name:Text, value:Text }
```

The file header comment (`tunnelrpc.capnp:4-7`) says these exist only for backward compatibility of the RPC protocol. `registerConnection`/`unregisterConnection`/`updateLocalConfiguration` are *also* exposed through the generated `TunnelServer` wrapper (`tunnelrpc.capnp.go:950-1000`) because `TunnelServer extends RegistrationServer`, but they always carry the **RegistrationServer interface ID** `0xf71695ec7fe85497` on the wire — the cloudflared client code actually constructs `proto.TunnelServer{Client: c.Client}` and calls the inherited `RegisterConnection` method (`pogs/registration_server.go:201-266`).

### 1c. Post-registration / edge-initiated interfaces (QUIC only)

```capnp
interface SessionManager @0x839445a59fb01686 {
    registerUdpSession @0 (sessionId:Data, dstIp:Data, dstPort:UInt16, closeAfterIdleHint:Int64, traceContext:Text="") -> (result:RegisterUdpSessionResponse);
    unregisterUdpSession @1 (sessionId:Data, message:Text) -> ();
}
struct RegisterUdpSessionResponse @0xab6d5210c1f26687 { err:Text, spans:Data }
interface ConfigurationManager @0xb48edfbdaa25db04 {
    updateConfiguration @0 (version:Int32, config:Data) -> (result:UpdateConfigurationResponse);
}
struct UpdateConfigurationResponse @0xdb58ff694ba05cf9 { latestAppliedVersion:Int32, err:Text }
interface CloudflaredServer @0xf548cef9dea2a4a1 extends(SessionManager, ConfigurationManager) {}
```

These run on **separate, edge-initiated QUIC streams** (see §2), not on the registration stream.

### 1d. Generated struct layouts (capnp-go `ObjectSize` = {data words bits, pointer count})

From `tunnelrpc/proto/tunnelrpc.capnp.go`:

- `RegistrationServer_registerConnection_Params`: `ObjectSize{DataSize: 8, PointerCount: 3}`; TypeID `0xe6646dec8feaa6ee` (line 3140-3149). Fields: `auth` = Ptr 0, `tunnelId` = Data Ptr 1, `connIndex` = UInt8 @ data offset 0, `options` = Ptr 2.
- `RegistrationServer_registerConnection_Results`: `ObjectSize{DataSize: 0, PointerCount: 1}`; TypeID `0xea50d822450d1f17`; field `result` = Ptr 0.
- `ConnectionResponse`: `{DataSize: 8, PointerCount: 1}`; union discriminator = UInt16 at data offset 0 (`error`=0, `connectionDetails`=1); the union value is stored in Ptr 0 (line 2550-2600).
- `ConnectionOptions`: `{8, 2}` (line 2430): `client`=Ptr0, `originLocalIp`=Data Ptr1, `replaceExisting`=Bit0, `compressionQuality`=UInt8@1, `numPreviousAttempts`=UInt8@2.
- `ClientInfo`: `{0, 4}` (line 2302): `clientId`=Data Ptr0, `features`=TextList Ptr1, `version`=Text Ptr2, `arch`=Text Ptr3.
- `ConnectionDetails`: `{8, 2}` (line 2805): `uuid`=Data Ptr0, `locationName`=Text Ptr1, `tunnelIsRemotelyManaged`=Bit0.
- `ConnectionError`: `{16, 1}` (line 2717): `cause`=Text Ptr0, `retryAfter`=Int64@0, `shouldRetry`=Bit64.
- `TunnelAuth`: `{0, 2}` (line 2901): `accountTag`=Text Ptr0, `tunnelSecret`=Data Ptr1.

---

## 2. How registration is transported over a stream

**It is full Cap'n Proto RPC** (`zombiezen.com/go/capnproto2/rpc`), including bootstrap, call/return, finish, and capability tables. It is **not** raw per-request capnp marshaling. Both the registration stream and the QUIC session/configuration streams use `rpc.StreamTransport` → `rpc.NewConn` → `conn.Bootstrap(ctx)` → method calls.

### 2a. The transport and framing

`tunnelrpc/utils.go:64-66`:
```go
func SafeTransport(rw io.ReadWriteCloser) rpc.Transport {
	return rpc.StreamTransport(&readWriterSafeTemporaryErrorCloser{...})
}
```
`rpc.StreamTransport` (`vendor/zombiezen.com/go/capnproto2/rpc/transport.go:33-70`) wraps the stream with a `capnp.Encoder` / `capnp.Decoder` (unpacked framing, one Cap'n Proto message per RPC packet, back-to-back, no other header/footer). Framing per `vendor/.../capnproto2/mem.go`:

- **Encoder (`mem.go:672-726`)**: header = LE uint32 `maxSeg` (= numSegments−1), then per segment an LE uint32 word count, padded with a zero uint32 to 8-byte alignment, then each segment's raw 8-byte-word bytes. Writes the whole thing as one `Write`.
- **Decoder (`mem.go:500-600`)**: reads the 4-byte segment-count header, then `maxSeg+1` size words, then segment data. Limits: `maxStreamSegments` and default decode limit (`defaultDecodeLimit`) apply.
- Transport sends each message by serializing the whole RPC `Message` (root struct of the rpc.capnp `Message` union) via `Enc.Encode`, then one `rwc.Write` (`transport.go:44-56`). `RecvMessage` decodes one message per call (`transport.go:58-77`).
- No packing, no compression, no magic bytes on the registration stream.

### 2b. Per-transport differences

**QUIC registration (control) stream** — `connection/quic_connection.go:87-92`:
```go
// The edge assumes the first stream is used for the control plane
controlStream, err := q.conn.OpenStream()
```
cloudflared opens the **first** QUIC bidi stream and serves the control plane on it (`serveControlStream` → `controlStreamHandler.ServeControlStream` → `tunnelrpc.NewRegistrationClient(ctx, stream, timeout)`). No protocol signature is written; capnp RPC frames start at byte 0 of the stream. `NewRegistrationClient` (`tunnelrpc/registration_client.go:36-45`) does `SafeTransport(stream)` then `conn.Bootstrap(ctx)`.

**QUIC edge-initiated RPC streams** (SessionManager / ConfigurationManager / CloudflaredServer) — `tunnelrpc/quic/cloudflared_client.go:26-40` writes a 6-byte magic first:
```go
rpcStreamProtocolSignature = protocolSignature{0x52, 0xBB, 0x82, 0x5C, 0xDB, 0x65}  // protocol.go:27
n, err := stream.Write(rpcStreamProtocolSignature[:])
```
then capnp RPC. The edge side dispatches on the signature (`quic/cloudflared_server.go:37-50`, `quic/session_server.go:26-35`). Data streams use `dataStreamProtocolSignature {0x0A, 0x36, 0xCD, 0x12, 0xA1, 0x3E}` + 2-byte version `"01"` + a `ConnectRequest` capnp message (`quic/protocol.go:25-26`, `quic/request_server_stream.go:15-38`).

**HTTP/2 control stream** — the edge opens an HTTP/2 stream with header `Cf-Cloudflared-Proxy-Connection-Upgrade: control-stream` (`connection/http2.go:22-23`, `isControlStreamUpgrade` at `http2.go:320-321`). `HTTP2Connection.ServeHTTP` routes `TypeControlStream` (`http2.go:120-124`) to `controlStreamHandler.ServeControlStream(r.Context(), respWriter, ...)`. `respWriter` is an `*http2RespWriter` that implements `io.ReadWriteCloser` (Read = request body, Write = response body, flush per write, `http2.go:290-330`). Capnp RPC frames flow raw through the HTTP/2 request/response bodies — **no signature, no upgrade protocol**, just the header marker. Registration happens *while the control stream is still open* (client reads the register response from the same stream before serving traffic).

### 2c. Edge vs client roles

- Registration: cloudflared is the **client**, edge is the **server** (edge's bootstrap main interface is the `RegistrationServer`). This is the classic `tunnel run` direction.
- UDP session / remote config: edge is the **client**, cloudflared is the **server** (`SessionManager`/`ConfigurationManager` via `CloudflaredServer`), on separate QUIC streams (`connection/quic_connection.go:179` `rpcquic.NewCloudflaredServer(...)`; server impl `tunnelrpc/pogs/session_manager.go`, `configuration_manager.go`).

---

## 3. Exact RPC sequence when establishing a connection

Client side flow, one QUIC or HTTP/2 control stream:

1. **Open transport** (`tunnelrpc/registration_client.go:36-45`): `SafeTransport(stream)`, `rpc.NewConn(transport, rpc.ConnLog(noop))`, then `client := pogs.NewRegistrationServer_PogsClient(conn.Bootstrap(ctx), conn)`.
2. **Bootstrap** — `Conn.Bootstrap` (`vendor/.../rpc/rpc.go:272-307`): allocates question id (idgen starts at 0, monotonic), sends `Message::bootstrap { questionId }`. Server replies `Message::return { answerId, results: Payload{ content: interface ptr, capTable: [CapDescriptor{senderHosted, id:0}] } }` (server main interface exported as export 0; `rpc.go:611-635`, `answer.go:63-113`). Client turns that into an import id (first import = 0).
3. **registerConnection call** — `RegistrationServer_PogsClient.RegisterConnection` (`pogs/registration_server.go:201-266`). `lockedCall` (`vendor/.../rpc/tables.go:91-130`) sends:
   - `Message::call { questionId: 1, target: importedCap(0), interfaceId: 0xf71695ec7fe85497, methodId: 0, params: Payload{ content: RegistrationServer_registerConnection_Params, capTable: [] } }`.
   - Params are populated from credentials + options (`pogs/registration_server.go:204-231`): `auth` (TunnelAuth{AccountTag, TunnelSecret} from `Credentials.Auth()`, `connection/connection.go:47-52`), `tunnelId` (16-byte UUID), `connIndex` (UInt8, per connection index, 0..N), `options` (ConnectionOptions).
   - ConnectionOptions come from `ConnectionOptionsSnapshot.ConnectionOptions()` (`client/config.go:62-70`): `Client{ClientID: connector UUID, Version, Arch, Features: feature list}` (from `client/config.go:47-60`), `OriginLocalIP` (parsed from the local address of the edge socket — QUIC: `addr.UDP.String()`; HTTP/2: `edgeConn.LocalAddr().String()`, `supervisor/tunnel.go:467,494`), `ReplaceExisting: false`, `CompressionQuality: 0`, `NumPreviousAttempts` = retry backoff counter (`uint8(backoff.Retries())`, `supervisor/tunnel.go:467,494`).
4. **Server reply**: `Message::return { answerId: 1, results: Payload{ content: RegistrationServer_registerConnection_Results { result: ConnectionResponse }, capTable: [] } }`. Either union arm. The pogs client reads `response.Result().Which()` (`pogs/registration_server.go:232-266`).
5. **Client sends `Message::finish { questionId: 1, releaseResultCaps: false }`** — capnp-go always finishes a resolved question (`rpc.go:520-522` in `handleReturnMessage`; `newFinishMessage`). On *exception* returns it sends finish with `releaseResultCaps: true`.
6. **Post-registration** (`connection/control.go:84-121`): on success, if `connIndex == 0 && !details.TunnelIsRemotelyManaged`, the client calls **`updateLocalConfiguration`** (method 2, params `{config: Data}` = JSON ingress config) and awaits the (empty) return.
7. **Steady state** — the control stream stays open; `waitForUnregister` (`control.go:124-147`) blocks on ctx / graceful-shutdown signal.
8. **Unregister** (`control.go:132-140`, `registration_client.go:70-81`): sends `Message::call { questionId: 2, target: importedCap(0), interfaceId: 0xf71695ec7fe85497, methodId: 1, params: empty payload }`, waits for empty return, then finishes. `Close()` (`registration_client.go:83-88`) closes the capnp client (sends `release` for the bootstrap import) and the transport (closes the stream).

Per-call timeout: `requestTimeout` (= `RPCTimeout`, configurable, `supervisor/tunnel.go:458`); unregister uses `gracePeriod` (`MaxGracePeriod = 3 min`, `connection/connection.go:16`).

---

## 4. What the edge returns and what the client must do

`RegisterConnectionResponse` = the `ConnectionResponse` union inside `RegistrationServer_registerConnection_Results`:

- **`connectionDetails` arm** → `ConnectionDetails { uuid: Data(16), locationName: Text, tunnelIsRemotelyManaged: Bool }` (`tunnelrpc.capnp:60-64`). Client (`control.go:92-99`, `observer.go:55-70`):
  - `uuid` is the per-connection id — used for logging/observability (`registrationDetails.UUID`), not for reconnection in this version (unlike legacy `eventDigest`/`connDigest`).
  - `locationName` — colo code; logged and used in `sendConnectedEvent`/metrics (`connection/metrics.go:149`).
  - `tunnelIsRemotelyManaged` — gates the `updateLocalConfiguration` push (only when false AND connIndex 0, `control.go:101-119`).
  - Triggers `connectedFuse.Connected()` (marks connection healthy for the reconnect/supervisor logic, `control.go:98`).
- **`error` arm** → `ConnectionError { cause: Text, retryAfter: Int64(ns), shouldRetry: Bool }` (`tunnelrpc.capnp:56-58`). Pogs client (`pogs/registration_server.go:240-253`) wraps: if `shouldRetry`, error becomes `RetryErrorAfter(err, retryAfter)`; else plain error. `control.go:86-92` special-cases:
  - `cause == "EDUPCONN"` (`connection/errors.go:12-14`) → `DupConnRegisterTunnelError`, metrics label `dup_edge_conn`; supervisor treats it as non-retryable on this edge address (picks a new address, `supervisor/tunnel.go:404-412`).
  - other errors → `ServerRegisterTunnelError{Cause, Permanent: !retryable}` (`connection/errors.go:32-52`); retryable errors are retried, permanent errors stop the connection.
- **Transport-level failure** (connection drop before return) surfaces as `RPCError`/context timeout and is treated as a recoverable connection error.

---

## 5. capnp-rpc behaviors a minimal Rust RPC layer must replicate

Schema + wire facts (numeric IDs are stable, from `.capnp` annotations and generated code):

- **rpc.capnp `Message` union** (standard, `vendor/zombiezen.com/go/capnproto2/std/capnp/rpc/rpc.capnp.go:12-29`): `unimplemented=0, abort=1, call=2, return=3, finish=4, resolve=5, release=6, bootstrap=8, disembargo=13, ...`. TypeIDs: `Message=0x91b79f1f808db032`, `Call=0x836a53ce789d4cd4`, `Return=0x9e19b28d3db3573a`, `Finish=0xd37d2eb2c2f80e63`, `Bootstrap=0xe94ccf8031176ec4`, `Release=0xad1a6c0d7dd07497`, `Payload=0x9a0e61223d96743b`, `CapDescriptor=...`, `MessageTarget=...`.
- **Call message layout** (`rpc.capnp.go:714-935`): data = `questionId` UInt32@0, `methodId` UInt16@2, `sendResultsTo` union UInt16@3 (0=caller, 1=yourself), `interfaceId` UInt64@8; pointers: 0=`target` (MessageTarget), 1=`params` (Payload), 2=`thirdParty` (union). `sendResultsTo` is set to **caller** (0) for plain client calls.
- **MessageTarget**: union UInt16@4 — `importedCap=0` (UInt32@0) or `promisedAnswer=1`. Registration uses `importedCap`.
- **Return message layout**: data = `answerId` UInt32@0, `releaseParamCaps` bit32 (inverted in capnp-go), union UInt16@6 (`results=0, exception=1, canceled=2, resultsSentElsewhere=3, takeFromOtherQuestion=4, acceptFromThirdParty=5`); ptr 0 = `results`/`exception` payload. `results` arm is a `Payload { content (root pointer), capTable: List(CapDescriptor) }`.
- **Bootstrap**: `Message::bootstrap { questionId }`. Server must answer `return { answerId, results: { content: interface pointer with capTable index 0, capTable: [senderHosted(exportId 0)] } }`. The registration payloads contain no capabilities, so cap tables are empty for both params and results in practice — but the bootstrap return must still carry the interface capability so the client can target `importedCap(0)` for subsequent calls.
- **Call dispatch** is by `(interfaceId, methodId)`:
  - registerConnection = `(0xf71695ec7fe85497, 0)`
  - unregisterConnection = `(0xf71695ec7fe85497, 1)`
  - updateLocalConfiguration = `(0xf71695ec7fe85497, 2)`
  - (legacy, deprecated but part of the same interface graph: TunnelServer methods 0-5 on `0xea58385c65416035`; SessionManager 0-1 on `0x839445a59fb01686`; ConfigurationManager 0 on `0xb48edfbdaa25db04`.)
- **Question/answer ids**: client increments monotonically from 0 (bootstrap=0, register=1, unregister=2, updateConfig would be 2 or 3 depending on order). Server echoes the question id as answerId. A server must answer every call with a return; the client sends **finish { questionId, releaseResultCaps }** after each return (releaseResultCaps=false for results, true for exception). The server should not send anything further for that question.
- **Error return**: `Message::return { answerId, exception: Exception { reason: Text, type: UInt16 } }` (`Exception_TypeID = 0xd625b7063acf691a`). This is the transport-level error path; cloudflared maps it to `RPCError` (never interpreted as a business error).
- **Release/unimplemented/abort**: capnp-go also emits `release` (decrement export refcounts when a client is closed — sent when `registrationClient.Close()` runs) and responds with `unimplemented` to unknown messages to avoid feedback loops. A minimal implementation can treat these as optional but should at least tolerate them; ignoring `finish` from the peer is acceptable for a client-only registration path, though the edge will send finish after your returns.
- **Framing**: unpacked capnp stream framing (4-byte LE segment count + per-segment LE word sizes + padded header + segment data, §2a). Message bodies are plain Cap'n Proto messages with root = rpc.capnp `Message`.
- **QUIC specifics**: registration stream = first bidi stream opened by client, raw RPC from byte 0. Edge-initiated RPC streams = 6-byte signature `52 BB 82 5C DB 65` then raw RPC. HTTP/2 control stream = HTTP/2 stream with `Cf-Cloudflared-Proxy-Connection-Upgrade: control-stream` request header; raw RPC in the request/response body (both directions over one HTTP/2 stream).
- **Stream framing is bidirectional & full-duplex** on a single stream; messages multiplex via question/answer ids. Since registration is strictly request/response on one stream, a sequential minimal implementation works, but the edge may send unrelated RPC messages on the registration stream only in the edge-initiated direction (it doesn't — the registration stream is dedicated).

### Practical notes for the Rust implementation

- You need a Cap'n Proto encoder/decoder plus the small rpc.capnp schema (or hand-rolled Message union) and the tunnelrpc schema. `capnp` (rust) can compile both `rpc.capnp` and `tunnelrpc.capnp`; the only RPC machinery you must implement yourself is bootstrap + call + return + finish over the stream transport.
- The minimal wire sequence to register: write `bootstrap{qid}` → read `return{aid=0, capTable=[senderHosted 0]}` → write `call{qid=1, interfaceId=0xf71695ec7fe85497, methodId=0, target=importedCap(0), params}` → read `return{aid=1, results}` → write `finish{qid=1, releaseResultCaps=false}`. Unregister: same with methodId=1 and empty params.
- Question/answer ids are local to each connection (both sides may allocate; ids in one direction are independent of the other). Start at 0 and increment per new question.
- `server.Ack(p.Options)` in the pogs server handler is just an optimization hint to dispatch async handlers (capnp-go `server` package); it does not affect the wire protocol.

---

## Key file map

- `tunnelrpc/proto/tunnelrpc.capnp` — registration schema (authoritative field names/ordinals).
- `tunnelrpc/proto/tunnelrpc.capnp.go` — generated IDs/offsets (authoritative numeric values).
- `tunnelrpc/registration_client.go` — client transport bootstrap + 3 RPC methods.
- `tunnelrpc/registration_server.go` + `tunnelrpc/pogs/registration_server.go` — server-side handlers (edge side; useful as reference for params/results marshaling).
- `tunnelrpc/utils.go` — SafeTransport, Conn options, temp-error retry wrapper.
- `tunnelrpc/quic/protocol.go` — stream signatures (data vs rpc) and version byte.
- `tunnelrpc/quic/cloudflared_client.go` / `cloudflared_server.go` / `session_server.go` — edge-initiated RPC streams.
- `connection/control.go` — control stream orchestration (register → config push → wait → unregister).
- `connection/http2.go` — HTTP/2 control-stream detection and raw-body transport.
- `connection/quic_connection.go` — first-stream-is-control-plane for QUIC.
- `client/config.go` — ConnectionOptions population.
- `connection/errors.go` — `EDUPCONN` duplicate-connection handling.
- `vendor/zombiezen.com/go/capnproto2/rpc/{rpc.go,transport.go,tables.go,question.go,answer.go}` — capnp-go RPC wire behavior (framing, bootstrap, call, return, finish).
- `vendor/zombiezen.com/go/capnproto2/std/capnp/rpc/rpc.capnp.go` — standard RPC message structs/layouts.
- `vendor/zombiezen.com/go/capnproto2/mem.go` — stream framing (encoder/decoder).

