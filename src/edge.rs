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
pub(crate) struct EdgeAddr {
    pub addr: SocketAddr,
}

/// Resolves the edge addresses for a connection, preferring the regional SRV
/// records used by cloudflared and falling back to well-known hostnames.
pub(crate) async fn discover_edges(region: Option<&str>) -> Result<Vec<EdgeAddr>> {
    let service = match region {
        Some(r) if !r.is_empty() => format!("{r}-{SRV_SERVICE}"),
        _ => SRV_SERVICE.to_string(),
    };
    let mut edges = Vec::new();
    match srv_lookup(&service, SRV_PROTO, SRV_NAME).await {
        Ok(records) if !records.is_empty() => {
            for (host, port) in records {
                for ip in resolve_host(&host).await {
                    let addr = EdgeAddr {
                        addr: SocketAddr::new(ip, port),
                    };
                    if !edges.contains(&addr) {
                        edges.push(addr);
                    }
                }
            }
        }
        _ => {
            tracing::debug!("SRV lookup unavailable, using fallback edge hostnames");
            for host in FALLBACK_EDGES {
                for ip in resolve_host(host).await {
                    let addr = EdgeAddr {
                        addr: SocketAddr::new(ip, FALLBACK_EDGE_PORT),
                    };
                    if !edges.contains(&addr) {
                        edges.push(addr);
                    }
                }
            }
        }
    }
    if edges.is_empty() {
        return Err(Error::EdgeDiscovery(
            "no edge addresses could be resolved".into(),
        ));
    }
    Ok(edges)
}

async fn resolve_host(host: &str) -> Vec<IpAddr> {
    let mut out = Vec::new();
    if let Ok(addrs) = tokio::net::lookup_host((host, FALLBACK_EDGE_PORT)).await {
        for addr in addrs {
            let ip = addr.ip();
            if !out.contains(&ip) {
                out.push(ip);
            }
        }
    }
    out
}

/// Returns `(hostname, port)` pairs from the SRV records.
async fn srv_lookup(service: &str, proto: &str, name: &str) -> io::Result<Vec<(String, u16)>> {
    let resolver = resolver_address().await;
    for addr in resolver {
        match query_srv(addr, service, proto, name).await {
            Ok(records) if !records.is_empty() => return Ok(records),
            Ok(_) => continue,
            Err(e) => tracing::debug!(%addr, "srv query failed: {e}"),
        }
    }
    Ok(Vec::new())
}

async fn resolver_address() -> Vec<SocketAddr> {
    let mut candidates = Vec::new();
    if let Ok(contents) = tokio::fs::read_to_string("/etc/resolv.conf").await {
        for line in contents.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("nameserver") {
                let host = rest.trim();
                if let Ok(ip) = host.parse::<IpAddr>() {
                    candidates.push(SocketAddr::new(ip, 53));
                }
            }
        }
    }
    candidates.push(SocketAddr::from(([1, 1, 1, 1], 53)));
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

    let id: u16 = rand16();
    let mut query = Vec::with_capacity(12 + qname.len() + 4);
    query.extend_from_slice(&id.to_be_bytes());
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

    let mut buf = vec![0u8; 4096];
    let timeout = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut buf))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "dns timeout"))??;
    if timeout == 0 {
        return Ok(Vec::new());
    }
    parse_srv_response(&buf[..timeout], id)
}

fn parse_srv_response(bytes: &[u8], id: u16) -> io::Result<Vec<(String, u16)>> {
    if bytes.len() < 12 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short dns header",
        ));
    }
    let resp_id = u16::from_be_bytes([bytes[0], bytes[1]]);
    if resp_id != id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "dns id mismatch",
        ));
    }
    let ancount = u16::from_be_bytes([bytes[6], bytes[7]]) as usize;
    let mut pos = 12usize;
    // Skip the question section.
    pos = skip_name(bytes, pos)?;
    pos += 4; // qtype + qclass

    let mut records = Vec::new();
    for _ in 0..ancount {
        if pos >= bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated dns answer",
            ));
        }
        pos = skip_name(bytes, pos)?;
        if pos + 10 > bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated rr"));
        }
        let rtype = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]);
        let rclass = u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]);
        let rdlen = u16::from_be_bytes([bytes[pos + 8], bytes[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated rdata",
            ));
        }
        let rdata = &bytes[pos..pos + rdlen];
        pos += rdlen;
        if rclass != 1 {
            continue;
        }
        if rtype == 33 && rdlen >= 6 {
            let port = u16::from_be_bytes([rdata[4], rdata[5]]);
            let mut target = String::new();
            // The target name is at rdata offset 6; positions are
            // message-absolute, so resolve relative to the message start.
            let mut name_pos = pos - rdlen + 6;
            decode_name(bytes, &mut name_pos, &mut target)?;
            records.push((target, port));
        }
    }
    Ok(records)
}

fn skip_name(bytes: &[u8], mut pos: usize) -> io::Result<usize> {
    loop {
        if pos >= bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "name overflow"));
        }
        let len = bytes[pos];
        if len == 0 {
            return Ok(pos + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Ok(pos + 2);
        }
        if len & 0xc0 != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad label"));
        }
        pos += 1 + len as usize;
    }
}

/// Decodes a (possibly compressed) name into a hostname string. `pos` is a
/// message-absolute position and is advanced past the name when it is not
/// compressed.
fn decode_name(bytes: &[u8], pos: &mut usize, out: &mut String) -> io::Result<()> {
    let mut p = *pos;
    let mut jumped = false;
    loop {
        if p >= bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "name overflow"));
        }
        let len = bytes[p];
        if len == 0 {
            if !jumped {
                *pos = p + 1;
            }
            break;
        }
        if len & 0xc0 == 0xc0 {
            if p + 1 >= bytes.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "bad pointer"));
            }
            let target = (((len & 0x3f) as usize) << 8) | bytes[p + 1] as usize;
            if !jumped {
                *pos = p + 2;
            }
            p = target;
            jumped = true;
            continue;
        }
        if len & 0xc0 != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad label"));
        }
        if p + 1 + len as usize > bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "label overflow"));
        }
        let label = &bytes[p + 1..p + 1 + len as usize];
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&String::from_utf8_lossy(label));
        p += 1 + len as usize;
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

fn rand16() -> u16 {
    let mut buf = [0u8; 2];
    let _ = boring::rand::rand_bytes(&mut buf);
    u16::from_be_bytes(buf)
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
        // A hand-built response: one SRV record for
        // region1.v2.argotunnel.com port 7844.
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
}
