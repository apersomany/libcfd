# Technical brief: Quick Tunnel creation and edge discovery in cloudflared

Reference checkout: `/home/aperso/libcfd/research/cloudflared` @ `61a0b0b3` (tag `2026.7.3-3-g61a0b0b3`, i.e. a 2026-era release).

Scope note: this checkout has NO `cmd/cloudflared/tunnel/run.go` and NO use of `/cdn-cgi/trace`. The quick tunnel path is `TunnelCommand` -> `RunQuickTunnel` (in `cmd/cloudflared/tunnel/quick_tunnel.go`), and edge discovery is DNS SRV based only. Anything the task asked about that referenced older behavior is documented as "not present in this checkout".

## 1. Quick Tunnel creation HTTP request

Entry point: `cloudflared tunnel --url <url>` (or `--hello-world`) with no `--name`.

- Dispatch: `cmd/cloudflared/tunnel/cmd.go:250-261` — `TunnelCommand` runs `RunQuickTunnel(sc)` when `quick-service != "" && (url or hello-world set)`.
- Request built at `cmd/cloudflared/tunnel/quick_tunnel.go:38`:

```go
req, err := http.NewRequest(http.MethodPost, fmt.Sprintf("%s/tunnel", sc.c.String("quick-service")), nil)
```

- Method: `POST`
- Host: value of the `quick-service` flag, default `"https://api.trycloudflare.com"` (`cmd/cloudflared/tunnel/cmd.go:873-875`, hidden flag).
- Full URL: `https://api.trycloudflare.com/tunnel` (no query string, no path beyond `/tunnel`).
- Headers (`quick_tunnel.go:42-43`):
  - `Content-Type: application/json`
  - `User-Agent: cloudflared/<version>` — `cliutil.BuildInfo.UserAgent()` at `cmd/cloudflared/cliutil/build_info.go:56-58` returns `fmt.Sprintf("cloudflared/%s", bi.CloudflaredVersion)`.
- Body: `nil` (empty request body).
- No `TryCloudflare` header exists anywhere in this checkout. The header of the old argo-tunnel era is gone; account binding is done entirely by the service response.
- Client config (`quick_tunnel.go:31-36`): `http.Client` with `TLSHandshakeTimeout`, `ResponseHeaderTimeout`, `Timeout` all = `httpTimeout = 15 * time.Second` (`quick_tunnel.go:19`).

### Account id acquisition

The account id is NOT fetched by the client. `api.trycloudflare.com` returns it: the `account_tag` field of the JSON `result` object is read into `data.Result.AccountTag` (`quick_tunnel.go:70`) and becomes `connection.Credentials.AccountTag`.

## 2. Response JSON shape and extracted fields

Parsed at `quick_tunnel.go:46-54` into (`quick_tunnel.go:100-127`):

```go
type QuickTunnelResponse struct {
    Success bool
    Result  QuickTunnel
    Errors  []QuickTunnelError
}
type QuickTunnelError struct {
    Code    int
    Message string
}
type QuickTunnel struct {
    ID         string `json:"id"`
    Name       string `json:"name"`
    Hostname   string `json:"hostname"`
    AccountTag string `json:"account_tag"`
    Secret     []byte `json:"secret"`
}
```

Extracted fields (`quick_tunnel.go:58-80`):
- `tunnelID` = `uuid.Parse(data.Result.ID)` (Go `uuid.UUID`; the `id` field is a UUID string).
- `credentials` = `connection.Credentials{AccountTag: data.Result.AccountTag, TunnelSecret: data.Result.Secret, TunnelID: tunnelID}` (type at `connection/connection.go:64-72`).
- `url` = `data.Result.Hostname`, prefixed with `"https://"` if missing (`quick_tunnel.go:76-79`) — the quick tunnel hostname, e.g. `<random>.trycloudflare.com`.
- `data.Result.Name` is parsed but unused downstream.
- The hostname is also stored as `connection.TunnelProperties{Credentials: credentials, QuickTunnelUrl: data.Result.Hostname}` (`quick_tunnel.go:103-104`).

Defaults forced for quick tunnels (`quick_tunnel.go:84-91`):
- `--protocol` forced to `quic` if unset.
- `--ha-connections` forced to `1`.

## 3. Edge discovery

There is no HTTP config endpoint for edge discovery. Discovery is DNS-based, in `edgediscovery/`.

### SRV record

`edgediscovery/allregions/discovery.go:18-20`:

```go
srvService = "v2-origintunneld"
srvProto   = "tcp"
srvName    = "argotunnel.com"
```

Lookup: `net.LookupSRV("v2-origintunneld", "tcp", "argotunnel.com")` i.e. `_v2-origintunneld._tcp.argotunnel.com` (`discovery.go:112-131`, `EdgeDiscovery`). With a region override the service name is prefixed: `RegionalServiceName(region)` at `regions.go:138-144` yields e.g. `us-v2-origintunneld` -> `_us-v2-origintunneld._tcp.argotunnel.com`. Port 7844 comes from the SRV record's `port` field (applied to both TCP and UDP addrs at `discovery.go:187-188`); tests and prechecks hardcode 7844 (`prechecks/probes.go:43-44`, `prechecks/probes_test.go:29`).

Fallback when the system resolver fails: DoT lookup against `1.1.1.1:853` (`cloudflare-dns.com`) — `fallbackLookupSRV = lookupSRVWithDOT` (`discovery.go:99`), `dotServerAddr = "1.1.1.1:853"`, `dotTimeout = 15s` (`discovery.go:25-27`, impl `discovery.go:153-171`).

### Region structure and static fallback

- `Regions` holds two `Region`s (`regions.go:8-13`). `ResolveEdge` requires the SRV query to return >= 2 targets and assigns `edgeAddrs[0]` to region1 and `edgeAddrs[1]` to region2 (`regions.go:18-30`).
- Each SRV target is a CNAME hostname (e.g. `region1.v2.argotunnel.com`); `resolveSRV` resolves it to one `EdgeAddr{TCP, UDP, IPVersion}` per IP (`discovery.go:157-180`).
- Region hostnames seen elsewhere in the repo: `region1.v2.argotunnel.com`, `region2.v2.argotunnel.com`, `us-region1.v2.argotunnel.com`, `us-region2.v2.argotunnel.com`, `fed-region1.v2.argotunnel.com`, `fed-region2.v2.argotunnel.com` (`prechecks/probes.go:61-66`, used by probe resolution at `prechecks/probes.go:333`). These are the well-known edge CNAMEs; the canonical source is the SRV record.
- Static override: `--edge` flag (`TUNNEL_EDGE`), hidden, "Only works in Cloudflare's internal testing environment" (`cmd.go:668-672`); when set, `StaticEdge` bypasses DNS (`supervisor/supervisor.go:58-70`, `edgediscovery/edgediscovery.go:49-56`). No default list of static edge hostnames exists in the code — DNS is the default.

### Address selection / fallback ordering

- `supervisor.NewSupervisor` resolves the edge once at startup: `StaticEdge(config.EdgeAddrs)` if `--edge` set, else `ResolveEdge(config.Log, config.Region, config.EdgeIPVersion)` (`supervisor/supervisor.go:58-70`). Region comes from `--region` (`TUNNEL_REGION`) or the credentials' endpoint; both at once is an error (`cmd/cloudflared/tunnel/configuration.go:192-203`).
- Initial RPC/management address: `GetAddrForRPC` -> `regions.GetAnyAddress()` which prefers region1 (`edgediscovery.go:58-67`, `regions.go:53-59`).
- Per connection: `GetAddr(connIndex)` returns the address already used by that conn if any; otherwise `GetUnusedAddr` picks the region with more available addrs (randomized if equal), preferring a region different from the last one used (`edgediscovery.go:71-90`, `regions.go:67-86`, `getAddrs` at `regions.go:90-99`).
- IP version: `--edge-ip-version` (`TUNNEL_EDGE_IP_VERSION`, default `auto`) -> `ConfigIPVersion` `4|6|auto` (`cmd/cloudflared/tunnel/configuration.go:298-308`); `NewRegion` splits v4/v6 sets, uses system preference (first family from DNS) unless overridden, and falls back to the secondary set after `timeoutDuration = 10 * time.Minute` (`region.go:14-62`).
- Connectivity failure handling: `GetDifferentAddr(connIndex, hasConnectivityError)` returns the old address to the pool and assigns a new one (`edgediscovery.go:92-110`), bounded by `--max-edge-addr-retries`; on exhaustion the supervisor falls back to the next protocol (QUIC -> HTTP2) — `Protocol.fallback()` at `connection/protocol.go:47-55` and `supervisor/supervisor.go:282`.
- Protocol selection for `auto`: TXT record `protocol-v2.argotunnel.com`, JSON array `[{"protocol":"quic","percentage":N},...]`, `ProtocolPercentage()` at `edgediscovery/protocol.go:11-44`.
- Feature flags: TXT record `cfd-features.argotunnel.com` JSON (`features/selector.go:16-30`).

### Other edge-related hostnames

- `quic.cftunnel.com` — TLS SNI for QUIC (`connection/protocol.go:21`)
- `h2.cftunnel.com` — TLS SNI for HTTP/2 (`connection/protocol.go:19`)
- `probe.cftunnel.com` — SNI for preflight probes (`connection/protocol.go:23`)
- `management.argotunnel.com` / `management.fed.argotunnel.com` — management service (`cmd.go:1080`, `credentials/credentials.go:14`)
- `cfd-features.argotunnel.com` — feature-flag TXT (`features/selector.go:17`)
- `update.argotunnel.com` — updater (`cmd/cloudflared/updater/update.go:30`)
- `h2.cftunnel.com`/`quic.cftunnel.com` are TLS names only; actual IPs come from SRV.

## 4. Token format and what is sent during registration

### Credentials vs token

- `connection.Credentials` (`connection/connection.go:64-72`): `AccountTag string`, `TunnelSecret []byte`, `TunnelID uuid.UUID`, `Endpoint string`.
- Named-tunnel token JSON/base64 form — `TunnelToken` (`connection/connection.go:89-110`): `{"a": accountTag, "s": secret, "t": tunnelID, "e": endpoint?}`, base64.StdEncoding of the JSON via `Encode()`. For quick tunnels no token file exists; the Credentials struct is built directly from the HTTP response.
- `Credentials.Auth()` (`connection/connection.go:74-79`) produces `pogs.TunnelAuth{AccountTag, TunnelSecret}`.

### Registration RPC (both transports)

The control stream calls `registrationClient.RegisterConnection(ctx, creds.Auth(), creds.TunnelID, connOptions, connIndex, edgeAddress)` — `connection/control.go:86-90`; RPC is `registerConnection(auth: TunnelAuth, tunnelId: Data, connIndex: UInt8, options: ConnectionOptions)` over capnp (`tunnelrpc/proto/tunnelrpc.capnp:160-167`). `TunnelAuth` is `{accountTag: Text, tunnelSecret: Data}` (`tunnelrpc.capnp:160-163`, `tunnelrpc/pogs/registration_server.go:245-247`). `ConnectionOptions` carries `ClientInfo{clientId, features, version, arch}`, `originLocalIp`, `replaceExisting`, `compressionQuality`, `numPreviousAttempts` (`tunnelrpc.capnp:117-125`, `client/config.go:40-69`). Response: `ConnectionDetails{uuid, locationName, tunnelIsRemotelyManaged}` (`tunnelrpc.capnp:150-158`).

- QUIC: first stream opened by cloudflared is the control stream carrying the capnp RPC (no client hello / metadata preamble) — `connection/quic_connection.go:89-108`; stream transport is `rpc.StreamTransport` via `SafeTransport` (`tunnelrpc/utils.go:37-41`). Incoming data streams are capnp `ConnectRequest{dest, type, metadata}` (`tunnelrpc/pogs/quic_metadata_protocol.go:47-58`).
- HTTP/2: the edge dials INTO cloudflared's H2 server (`http2.Server.ServeConn`); the registration stream is an H2 request with internal header `Cf-Cloudflared-Proxy-Connection-Upgrade: control-stream` (`connection/http2.go:27-31,377-388,120-124`), then the same capnp registration RPC runs over the hijacked stream.

### TLS to the edge (no client certificate for quick tunnels)

- TLS config built by `tlsconfig.CreateTunnelConfig(cacert, serverName)` (`cmd/cloudflared/tunnel/configuration.go:158-171`, `tlsconfig/certreloader.go:130-160`): RootCAs = system pool + Cloudflare root CA (`tlsconfig/cloudflare_ca.go`), ServerName set, NO client certificate, NO token-derived TLS auth.
- QUIC: SNI `quic.cftunnel.com`, ALPN `["argotunnel"]` (`connection/protocol.go:62-78`); QUIC dial at `connection/quic.go:24-45` via quic-go.
- HTTP/2: SNI `h2.cftunnel.com`, no custom ALPN, plain TLS over TCP via `edgediscovery.DialEdge` (`edgediscovery/dial.go:14-47`).
- The tunnel secret is used only as `TunnelAuth.tunnelSecret` inside the post-TLS registration RPC — never as a TLS client cert, and never logged.

## 5. Edge config/lookup endpoints

- None for edge addresses in this checkout. No `/cdn-cgi/trace`, no region-over-HTTP lookup, no per-colo config endpoint. `git log -S "cdn-cgi/trace"` over the whole repo history returns nothing.
- Edge addresses come exclusively from DNS SRV (`_v2-origintunneld._tcp.argotunnel.com`, region-prefixed variants), with DoT fallback to `1.1.1.1:853`.
- Other DNS "endpoints": `protocol-v2.argotunnel.com` (TXT, protocol percentages), `cfd-features.argotunnel.com` (TXT, feature flags).
- HTTP API endpoints used elsewhere (not edge discovery): `api.trycloudflare.com/tunnel` (quick tunnel creation), `api.cloudflare.com/client/v4` (named tunnel CRUD via `cfapi/`), `management.argotunnel.com` (management service), `update.argotunnel.com` (updates).

## Data flow summary

`cloudflared tunnel --url U`
-> POST `https://api.trycloudflare.com/tunnel` (empty body)
-> JSON `{success, result:{id, name, hostname, account_tag, secret}, errors}`
-> `Credentials{AccountTag, TunnelSecret, TunnelID}` + hostname
-> resolve edge via SRV `_v2-origintunneld._tcp.argotunnel.com` (or `--edge`/`--region`/`--edge-ip-version`)
-> pick addr (region1 first, per-conn reuse, fallback protocol QUIC->HTTP2)
-> TLS to `quic.cftunnel.com`/`h2.cftunnel.com` (system + CF root CA, no client cert)
-> open control stream, capnp `registerConnection(TunnelAuth{accountTag, tunnelSecret}, tunnelId, connIndex, ConnectionOptions)`
-> edge accepts; requests arrive as QUIC streams / H2 requests with headers `Cf-Cloudflared-Proxy-Connection-Upgrade` and `cf-cloudflared-*` internal headers (`connection/header.go:18-26`) and are proxied to the origin handler.

## Files an implementer should open first

1. `cloudflared/cmd/cloudflared/tunnel/quick_tunnel.go` (whole file, 127 lines) — quick tunnel HTTP request + response parsing.
2. `cloudflared/edgediscovery/allregions/discovery.go` — SRV lookup and DoT fallback.
3. `cloudflared/edgediscovery/allregions/regions.go` + `edgediscovery/edgediscovery.go` — address selection.
4. `cloudflared/connection/control.go` — registration RPC invocation.
5. `cloudflared/tunnelrpc/proto/tunnelrpc.capnp` — the wire schema for registration.

