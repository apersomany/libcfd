//! Edge discovery for tunnel connections.
//!
//! Mirrors cloudflared: look up the `_v2-origintunneld._tcp.argotunnel.com`
//! SRV record (region-prefixed when a region is set), resolve each target to
//! an IP, and fall back to the well-known edge hostnames when DNS fails.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::error::{Error, Result};

const SRV_SERVICE: &str = "v2-origintunneld";
const SRV_PROTO: &str = "tcp";
const SRV_NAME: &str = "argotunnel.com";
const FALLBACK_EDGE_PORT: u16 = 7844;
const FALLBACK_EDGES: &[&str] = &["region1.v2.argotunnel.com", "region2.v2.argotunnel.com"];

/// One resolved edge address.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgeAddress {
    pub address: SocketAddr,
}

/// Resolves the edge addresses for a connection, preferring the regional SRV
/// records used by cloudflared and falling back to well-known hostnames.
pub(crate) async fn discover_edges(
    region: Option<&str>,
    ip_version: Option<&str>,
) -> Result<Vec<EdgeAddress>> {
    let service = match region {
        Some(r) if !r.is_empty() => format!("{r}-{SRV_SERVICE}"),
        _ => SRV_SERVICE.to_string(),
    };
    let mut edges = Vec::new();
    match srv_lookup(&service, SRV_PROTO, SRV_NAME).await {
        Ok(records) if !records.is_empty() => {
            for (host, port) in records {
                for ip in resolve_host(&host, ip_version).await {
                    let address = EdgeAddress {
                        address: SocketAddr::new(ip, port),
                    };
                    if !edges.contains(&address) {
                        edges.push(address);
                    }
                }
            }
        }
        _ => {
            tracing::debug!("SRV lookup unavailable, using fallback edge hostnames");
            for host in FALLBACK_EDGES {
                for ip in resolve_host(host, ip_version).await {
                    let address = EdgeAddress {
                        address: SocketAddr::new(ip, FALLBACK_EDGE_PORT),
                    };
                    if !edges.contains(&address) {
                        edges.push(address);
                    }
                }
            }
        }
    }
    if edges.is_empty() {
        return Err(Error::edge_discovery("no edge addresses could be resolved"));
    }
    Ok(edges)
}

async fn resolve_host(host: &str, ip_version: Option<&str>) -> Vec<IpAddr> {
    let mut out = Vec::new();
    if let Ok(addrs) = tokio::net::lookup_host((host, FALLBACK_EDGE_PORT)).await {
        for address in addrs {
            let ip = address.ip();
            let matches_version = match ip_version {
                Some("4") | Some("v4") => ip.is_ipv4(),
                Some("6") | Some("v6") => ip.is_ipv6(),
                _ => true,
            };
            if matches_version && !out.contains(&ip) {
                out.push(ip);
            }
        }
    }
    out
}

/// Returns `(hostname, port)` pairs from the SRV records.
async fn srv_lookup(service: &str, proto: &str, name: &str) -> io::Result<Vec<(String, u16)>> {
    let resolver = resolver_address().await;
    for address in resolver {
        match query_srv(address, service, proto, name).await {
            Ok(records) if !records.is_empty() => return Ok(records),
            Ok(_) => continue,
            Err(e) => tracing::debug!(%address, "srv query failed: {e}"),
        }
    }
    Ok(Vec::new())
}

fn parse_resolv_conf(contents: &str) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("nameserver") {
            let host = rest.trim();
            if let Ok(ip) = host.parse::<IpAddr>() {
                out.push(SocketAddr::new(ip, 53));
            }
        }
    }
    out
}

#[cfg(any(target_os = "macos", test))]
fn parse_scutil_dns(contents: &str) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with("nameserver[")
            && let Some((_, rhs)) = line.split_once(':')
            && let Ok(ip) = rhs.trim().parse::<IpAddr>()
            && !out.contains(&SocketAddr::new(ip, 53))
        {
            out.push(SocketAddr::new(ip, 53));
        }
    }
    out
}

#[cfg(any(target_os = "windows", test))]
fn parse_netsh_dns(contents: &str) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for token in contents.split_whitespace() {
        if let Ok(ip) = token.parse::<IpAddr>() {
            if ip.is_unspecified() {
                continue;
            }
            // Skip Windows site-local IPv6 DNS placeholder addresses (fec0::/10).
            if let IpAddr::V6(v6) = ip
                && (v6.segments()[0] & 0xffc0) == 0xfec0
            {
                continue;
            }
            let addr = SocketAddr::new(ip, 53);
            if !out.contains(&addr) {
                out.push(addr);
            }
        }
    }
    out
}

async fn resolver_address() -> Vec<SocketAddr> {
    let mut candidates = Vec::new();
    if let Ok(contents) = tokio::fs::read_to_string("/etc/resolv.conf").await {
        candidates.extend(parse_resolv_conf(&contents));
    }
    #[cfg(target_os = "macos")]
    if candidates.is_empty()
        && let Ok(Ok(output)) = tokio::task::spawn_blocking(|| {
            std::process::Command::new("scutil").arg("--dns").output()
        })
        .await
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        candidates.extend(parse_scutil_dns(&stdout));
    }
    #[cfg(target_os = "windows")]
    if candidates.is_empty()
        && let Ok(addrs) = tokio::task::spawn_blocking(|| {
            let mut found = Vec::new();
            for family in ["ip", "ipv6"] {
                if let Ok(output) = std::process::Command::new("netsh")
                    .args(["interface", family, "show", "dnsservers"])
                    .output()
                    && output.status.success()
                {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for addr in parse_netsh_dns(&stdout) {
                        if !found.contains(&addr) {
                            found.push(addr);
                        }
                    }
                }
            }
            found
        })
        .await
    {
        candidates.extend(addrs);
    }
    let fallback = SocketAddr::from(([1, 1, 1, 1], 53));
    if !candidates.contains(&fallback) {
        candidates.push(fallback);
    }
    candidates
}

async fn query_srv(
    resolver: SocketAddr,
    service: &str,
    proto: &str,
    name: &str,
) -> io::Result<Vec<(String, u16)>> {
    let mut qname = Vec::new();
    encode_name(&mut qname, service)?;
    encode_name(&mut qname, proto)?;
    encode_name(&mut qname, name)?;
    qname.push(0);

    let identifier: u16 = random16();
    let mut query = Vec::with_capacity(12 + qname.len() + 4);
    query.extend_from_slice(&identifier.to_be_bytes());
    query.extend_from_slice(&[0x01, 0x00]); // RD
    query.extend_from_slice(&[0, 1]); // qdcount
    query.extend_from_slice(&[0, 0]); // ancount
    query.extend_from_slice(&[0, 0]); // nscount
    query.extend_from_slice(&[0, 0]); // arcount
    query.extend_from_slice(&qname);
    query.extend_from_slice(&[0, 33]); // SRV
    query.extend_from_slice(&[0, 1]); // IN

    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(resolver).await?;
    socket.send(&query).await?;

    let mut buffer = vec![0u8; 4096];
    let timeout = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut buffer))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "dns timeout"))??;
    if timeout == 0 {
        return Ok(Vec::new());
    }
    parse_srv_response(&buffer[..timeout], identifier)
}

fn parse_srv_response(bytes: &[u8], identifier: u16) -> io::Result<Vec<(String, u16)>> {
    if bytes.len() < 12 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short dns header",
        ));
    }
    let response_identifier = u16::from_be_bytes([bytes[0], bytes[1]]);
    if response_identifier != identifier {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "dns id mismatch",
        ));
    }
    let flags = u16::from_be_bytes([bytes[2], bytes[3]]);
    let rcode = flags & 0x000f;
    if rcode != 0 {
        return Err(io::Error::other(format!(
            "dns server returned rcode {rcode}"
        )));
    }
    if (flags & 0x0200) != 0 {
        tracing::debug!("dns srv response was truncated");
    }

    let answer_count = u16::from_be_bytes([bytes[6], bytes[7]]) as usize;
    let mut position = 12usize;
    // Skip the question section.
    position = skip_name(bytes, position)?;
    position += 4; // qtype + qclass

    let mut records = Vec::new();
    for _ in 0..answer_count {
        if position >= bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated dns answer",
            ));
        }
        position = skip_name(bytes, position)?;
        if position + 10 > bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated rr"));
        }
        let rtype = u16::from_be_bytes([bytes[position], bytes[position + 1]]);
        let rclass = u16::from_be_bytes([bytes[position + 2], bytes[position + 3]]);
        let rdlen = u16::from_be_bytes([bytes[position + 8], bytes[position + 9]]) as usize;
        position += 10;
        if position + rdlen > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated rdata",
            ));
        }
        let rdata = &bytes[position..position + rdlen];
        position += rdlen;
        if rclass != 1 {
            continue;
        }
        if rtype == 33 && rdlen >= 6 {
            let port = u16::from_be_bytes([rdata[4], rdata[5]]);
            let mut target = String::new();
            // The target name starts at rdata offset 6; positions are message-absolute, so resolve relative to the message start.
            let mut name_position = position - rdlen + 6;
            decode_name(bytes, &mut name_position, &mut target)?;
            records.push((target, port));
        }
    }
    Ok(records)
}

fn skip_name(bytes: &[u8], mut position: usize) -> io::Result<usize> {
    loop {
        if position >= bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "name overflow"));
        }
        let length = bytes[position];
        if length == 0 {
            return Ok(position + 1);
        }
        if length & 0xc0 == 0xc0 {
            return Ok(position + 2);
        }
        if length & 0xc0 != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad label"));
        }
        position += 1 + length as usize;
    }
}

/// Decodes a (possibly compressed) name into a hostname string. `pos` is a
/// message-absolute position and is advanced past the name when it is not
/// compressed.
fn decode_name(bytes: &[u8], position: &mut usize, out: &mut String) -> io::Result<()> {
    let mut p = *position;
    let mut jumped = false;
    let mut jumps = 0;
    loop {
        if jumps > 16 || out.len() > 255 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "compression loop or name too long",
            ));
        }
        if p >= bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "name overflow"));
        }
        let length = bytes[p];
        if length == 0 {
            if !jumped {
                *position = p + 1;
            }
            break;
        }
        if length & 0xc0 == 0xc0 {
            if p + 1 >= bytes.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "bad pointer"));
            }
            let target = (((length & 0x3f) as usize) << 8) | bytes[p + 1] as usize;
            if !jumped {
                *position = p + 2;
            }
            p = target;
            jumped = true;
            jumps += 1;
            continue;
        }
        if length & 0xc0 != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad label"));
        }
        if p + 1 + length as usize > bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "label overflow"));
        }
        let label = &bytes[p + 1..p + 1 + length as usize];
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&String::from_utf8_lossy(label));
        p += 1 + length as usize;
    }
    Ok(())
}

fn encode_name(out: &mut Vec<u8>, name: &str) -> io::Result<()> {
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad label"));
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    Ok(())
}

fn random16() -> u16 {
    let mut buffer = [0u8; 2];
    let _ = getrandom::fill(&mut buffer);
    u16::from_be_bytes(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_srv_query_name() {
        let mut out = Vec::new();
        encode_name(&mut out, "v2-origintunneld").unwrap();
        encode_name(&mut out, "tcp").unwrap();
        encode_name(&mut out, "argotunnel.com").unwrap();
        out.push(0);
        assert_eq!(
            out,
            [
                16, b'v', b'2', b'-', b'o', b'r', b'i', b'g', b'i', b'n', b't', b'u', b'n', b'n',
                b'e', b'l', b'd', 3, b't', b'c', b'p', 10, b'a', b'r', b'g', b'o', b't', b'u',
                b'n', b'n', b'e', b'l', 3, b'c', b'o', b'm', 0
            ]
        );
    }

    #[test]
    fn parses_srv_response() {
        // A hand-built response: one SRV record for region1.v2.argotunnel.com on port 7844.
        let mut bytes = vec![0x12, 0x34]; // id
        bytes.extend_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
        // question: name + SRV + IN
        bytes.extend_from_slice(&[16]);
        bytes.extend_from_slice(b"v2-origintunneld");
        bytes.extend_from_slice(&[3]);
        bytes.extend_from_slice(b"tcp");
        bytes.extend_from_slice(&[10]);
        bytes.extend_from_slice(b"argotunnel");
        bytes.extend_from_slice(&[3]);
        bytes.extend_from_slice(b"com");
        bytes.extend_from_slice(&[0, 0, 33, 0, 1]);
        // answer: name pointer to question name (offset 12)
        bytes.extend_from_slice(&[0xc0, 0x0c]);
        bytes.extend_from_slice(&[0, 33, 0, 1]); // type SRV, class IN
        bytes.extend_from_slice(&[0, 0, 0, 60]); // ttl
        let mut rdata = vec![0, 0, 0, 0]; // priority, weight
        rdata.extend_from_slice(&[0x1e, 0xa4]); // port 7844
        // target: region1.v2.argotunnel.com
        rdata.extend_from_slice(&[7]);
        rdata.extend_from_slice(b"region1");
        rdata.extend_from_slice(&[2, b'v', b'2']);
        rdata.extend_from_slice(&[10]);
        rdata.extend_from_slice(b"argotunnel");
        rdata.extend_from_slice(&[3]);
        rdata.extend_from_slice(b"com");
        rdata.push(0);
        bytes.extend_from_slice(&((rdata.len() as u16).to_be_bytes()));
        bytes.extend_from_slice(&rdata);

        let records = parse_srv_response(&bytes, 0x1234).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "region1.v2.argotunnel.com");
        assert_eq!(records[0].1, 7844);
    }

    #[test]
    fn rejects_truncated_dns_messages() {
        // Shorter than the 12-byte header.
        assert!(parse_srv_response(&[0x12], 0x1234).is_err());
        // Header but no question section.
        assert!(
            parse_srv_response(&[0x12, 0x34, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0], 0x1234).is_err()
        );
    }

    #[test]
    fn rejects_wrong_dns_identifier() {
        let bytes = [0x00, 0x01, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        assert!(parse_srv_response(&bytes, 0x1234).is_err());
    }

    #[test]
    fn rejects_bad_name_labels() {
        // A label with the 0x40 extension bits set is malformed.
        let bytes = [0x12, 0x34, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0, 0x40, b'x'];
        assert!(parse_srv_response(&bytes, 0x1234).is_err());
    }

    #[test]
    fn rejects_overflowing_name_length() {
        // A label length that runs past the end of the message.
        let bytes = [0x12, 0x34, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0, 0x20, b'x'];
        assert!(parse_srv_response(&bytes, 0x1234).is_err());
    }

    #[test]
    fn rejects_cyclic_dns_compression_pointers() {
        // Construct a circular compression pointer pointing back to itself (offset 12 -> offset 12)
        let bytes = [
            0x12, 0x34, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0,
            0x0c, // Pointer at 12 pointing to 12
        ];
        let mut pos = 12;
        let mut out = String::new();
        let result = decode_name(&bytes, &mut pos, &mut out);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("compression loop"));
    }

    #[test]
    fn rejects_dns_error_rcode() {
        // rcode 3 (NXDOMAIN) in flags [0x81, 0x83]
        let bytes = [0x12, 0x34, 0x81, 0x83, 0, 1, 0, 0, 0, 0, 0, 0];
        let err = parse_srv_response(&bytes, 0x1234).unwrap_err();
        assert!(err.to_string().contains("rcode 3"));
    }

    #[test]
    fn parses_platform_dns_outputs() {
        let resolv = "nameserver 10.0.0.1\nnameserver 2001:4860:4860::8888\n";
        assert_eq!(
            parse_resolv_conf(resolv),
            vec![
                "10.0.0.1:53".parse().unwrap(),
                "[2001:4860:4860::8888]:53".parse().unwrap(),
            ]
        );

        let scutil = "resolver #1\n  nameserver[0] : 192.168.1.1\n  nameserver[1] : 192.168.1.1\n";
        assert_eq!(
            parse_scutil_dns(scutil),
            vec!["192.168.1.1:53".parse().unwrap()]
        );

        let netsh = "Configuration for interface \"Ethernet\"\n    DNS servers configured through DHCP:  192.168.1.254\n                                          8.8.8.8\n    fec0:0:0:ffff::1\n";
        assert_eq!(
            parse_netsh_dns(netsh),
            vec![
                "192.168.1.254:53".parse().unwrap(),
                "8.8.8.8:53".parse().unwrap(),
            ]
        );
    }
}
