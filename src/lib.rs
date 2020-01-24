/*!
# WHOIS Rust

This is a WHOIS client library for Rust, inspired by https://github.com/hjr265/node-whois

## Usage

You can make a **servers.json** file or copy one from https://github.com/hjr265/node-whois

This is a simple example of **servers.json**.

```json
{
    "org": "whois.pir.org",
    "": "whois.ripe.net",
    "_": {
        "ip": {
            "host": "whois.arin.net",
            "query": "n + $addr\r\n"
        }
    }
}
```

Then, use the `from_path` (or `from_string` if your JSON data is in-memory) associated function to create a `WhoIs` instance.

```rust,ignore
extern crate whois_rust;

use whois_rust::WhoIs;

let whois = WhoIs::from_path("/path/to/servers.json").unwrap();
```

Use the `lookup` method and input a `WhoIsLookupOptions` instance to lookup a domain or an IP.

```rust,ignore
extern crate whois_rust;

use whois_rust::{WhoIs, WhoIsLookupOptions};

let whois = WhoIs::from_path("/path/to/servers.json").unwrap();

let result: String = whois.lookup(WhoIsLookupOptions::from_string("magiclen.org").unwrap()).unwrap();
```
*/

pub use idna;
use lazy_static::lazy_static;
pub use serde_json;
pub use validators;

use async_std::fs::File;
use async_std::future::{timeout as async_timeout, TimeoutError};
use async_std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use async_std::io::prelude::{ReadExt, WriteExt};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

use serde_json::{Map, Value};
use validators::domain::{DomainError, DomainUnlocalhostableWithoutPort};
use validators::host::{Host, HostLocalable};
use validators::ipv4::{IPv4Error, IPv4LocalableWithoutPort};
use validators::ipv6::{IPv6Error, IPv6LocalableWithoutPort};

use regex::Regex;

use idna::domain_to_ascii;

lazy_static! {
    static ref REF_SERVER_RE: Regex = {
        Regex::new(r"(ReferralServer|Registrar Whois|Whois Server|WHOIS Server|Registrar WHOIS Server):[^\S\n]*(r?whois://)?(.*)").unwrap()
    };
}

#[derive(Debug, Error)]
pub enum WhoIsError {
    #[error("SerdeJsonError {0}")]
    SerdeJsonError(#[from] serde_json::Error),
    #[error("IOError {0}")]
    IOError(#[from] std::io::Error),
    #[error("DomainError {0}")]
    DomainError(#[from] DomainError),
    #[error("IPv4Error {0}")]
    IPv4Error(#[from] IPv4Error),
    #[error("IPv6Error {0}")]
    IPv6Error(#[from] IPv6Error),
    #[error("TimeoutError {0}")]
    TimeoutError(#[from] TimeoutError),
    #[error("MapError {0}")]
    MapError(String),
    #[error("Should Retry")]
    RetryError(WhoIsServerValue, u16),
}

#[derive(Debug)]
pub enum Target {
    Domain(DomainUnlocalhostableWithoutPort),
    IPv4(IPv4LocalableWithoutPort),
    IPv6(IPv6LocalableWithoutPort),
}

/// The options about how to lookup.
#[derive(Debug)]
pub struct WhoIsLookupOptions {
    /// The target that you want to lookup.
    pub target: Target,
    /// The WHOIS server that you want to use. If it is **None**, an appropriate WHOIS server will be chosen from the list of WHOIS servers that the `WhoIs` instance have. The default value is **None**.
    pub server: Option<WhoIsServerValue>,
    /// Number of times to follow redirects. The default value is 2.
    pub follow: u16,
    /// Socket timeout in milliseconds. The default value is 60000.
    pub timeout: Option<Duration>,
}

/// The model of a WHOIS server.
#[derive(Debug, Clone)]
pub struct WhoIsServerValue {
    pub host: HostLocalable,
    pub query: Option<String>,
    pub punycode: bool,
}

impl WhoIsServerValue {
    fn from_value(value: &Value) -> Result<WhoIsServerValue, WhoIsError> {
        if let Some(obj) = value.as_object() {
            match obj.get("host") {
                Some(host) => {
                    if let Some(host) = host.as_str() {
                        let host = match HostLocalable::from_str(host) {
                            Ok(host) => host,
                            Err(_) => return Err(WhoIsError::MapError("The server value is an object, but it has not a correct host string.".to_string()))
                        };
                        let query = match obj.get("query") {
                            Some(query) => {
                                if let Some(query) = query.as_str() {
                                    Some(query.to_string())
                                } else {
                                    return Err(WhoIsError::MapError("The server value is an object, but it has an incorrect query string.".to_string()));
                                }
                            }
                            None => None,
                        };
                        let punycode = match obj.get("punycode") {
                            Some(punycode) => {
                                if let Some(punycode) = punycode.as_bool() {
                                    punycode
                                } else {
                                    return Err(WhoIsError::MapError("The server value is an object, but it has an incorrect punycode boolean value.".to_string()));
                                }
                            }
                            None => DEFAULT_PUNYCODE,
                        };
                        Ok(WhoIsServerValue {
                            host,
                            query,
                            punycode,
                        })
                    } else {
                        Err(WhoIsError::MapError(
                            "The server value is an object, but it has not a host string."
                                .to_string(),
                        ))
                    }
                }
                None => Err(WhoIsError::MapError(
                    "The server value is an object, but it has not a host string.".to_string(),
                )),
            }
        } else if let Some(host) = value.as_str() {
            Self::from_string(host)
        } else {
            Err(WhoIsError::MapError(
                "The server value is not an object or a host string.".to_string(),
            ))
        }
    }

    fn from_string<S: AsRef<str>>(string: S) -> Result<WhoIsServerValue, WhoIsError> {
        let host = string.as_ref();
        let host = match HostLocalable::from_str(host) {
            Ok(host) => host,
            Err(_) => {
                return Err(WhoIsError::MapError(
                    "The server value is not a correct host string.".to_string(),
                ))
            }
        };
        Ok(WhoIsServerValue {
            host,
            query: None,
            punycode: DEFAULT_PUNYCODE,
        })
    }
}

const DEFAULT_FOLLOW: u16 = 2;
const DEFAULT_TIMEOUT: u64 = 60000;
const DEFAULT_WHOIS_HOST_PORT: u64 = 43;
const DEFAULT_WHOIS_HOST_QUERY: &str = "$addr\r\n";
const DEFAULT_PUNYCODE: bool = true;

impl WhoIsLookupOptions {
    pub fn from_target(target: Target) -> WhoIsLookupOptions {
        WhoIsLookupOptions {
            target,
            server: None,
            follow: DEFAULT_FOLLOW,
            timeout: Some(Duration::from_millis(DEFAULT_TIMEOUT)),
        }
    }

    pub fn from_domain<S: AsRef<str>>(domain: S) -> Result<WhoIsLookupOptions, WhoIsError> {
        let domain = domain.as_ref();

        let domain = DomainUnlocalhostableWithoutPort::from_str(domain)?;
        let server = Target::Domain(domain);

        Ok(Self::from_target(server))
    }

    pub fn from_ipv4<S: AsRef<str>>(ipv4: S) -> Result<WhoIsLookupOptions, WhoIsError> {
        let ipv4 = ipv4.as_ref();

        let ipv4 = IPv4LocalableWithoutPort::from_str(ipv4)?;
        let server = Target::IPv4(ipv4);

        Ok(Self::from_target(server))
    }

    pub fn from_ipv6<S: AsRef<str>>(ipv6: S) -> Result<WhoIsLookupOptions, WhoIsError> {
        let ipv6 = ipv6.as_ref();

        let ipv6 = IPv6LocalableWithoutPort::from_str(ipv6)?;
        let server = Target::IPv6(ipv6);

        Ok(Self::from_target(server))
    }

    pub fn from_string<S: AsRef<str>>(string: S) -> Result<WhoIsLookupOptions, WhoIsError> {
        match Self::from_ipv4(&string) {
            Ok(opt) => Ok(opt),
            Err(_) => match Self::from_ipv6(&string) {
                Ok(opt) => Ok(opt),
                Err(_) => Self::from_domain(&string),
            },
        }
    }
}

/// The `WhoIs` structure stores the list of WHOIS servers in-memory.
#[derive(Debug)]
pub struct WhoIs {
    map: HashMap<String, WhoIsServerValue>,
    ip: WhoIsServerValue,
}

impl WhoIs {
    /// Read the list of WHOIS servers (JSON data) from a file to create a `WhoIs` instance.
    pub async fn from_path<P: AsRef<Path>>(path: P) -> Result<WhoIs, WhoIsError> {
        let path = path.as_ref();

        let mut file = File::open(path).await?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await?;

        let map: Map<String, Value> = serde_json::from_slice(&buf)?;

        Self::from_inner(map)
    }

    /// Read the list of WHOIS servers (JSON data) from a string to create a `WhoIs` instance.
    pub fn from_string<S: AsRef<str>>(string: S) -> Result<WhoIs, WhoIsError> {
        let string = string.as_ref();

        let map: Map<String, Value> = serde_json::from_str(string)?;

        Self::from_inner(map)
    }

    fn from_inner(mut map: Map<String, Value>) -> Result<WhoIs, WhoIsError> {
        let ip = match map.remove("_") {
            Some(server) => {
                if !server.is_object() {
                    return Err(WhoIsError::MapError(
                        "`_` in the server list is not an object.".to_string(),
                    ));
                }
                match server.get("ip") {
                    Some(server) => {
                        if server.is_null() {
                            return Err(WhoIsError::MapError(
                                "`ip` in the `_` object in the server list is null.".to_string(),
                            ));
                        }
                        WhoIsServerValue::from_value(server)?
                    }
                    None => {
                        return Err(WhoIsError::MapError(
                            "Cannot find `ip` in the `_` object in the server list.".to_string(),
                        ))
                    }
                }
            }
            None => return Err(WhoIsError::MapError("Cannot find `_` in the server list.".to_string())),
        };

        let mut new_map: HashMap<String, WhoIsServerValue> = HashMap::with_capacity(map.len());

        for (k, v) in map {
            if !v.is_null() {
                let server_value = WhoIsServerValue::from_value(&v)?;
                new_map.insert(k, server_value);
            }
        }

        Ok(WhoIs {
            map: new_map,
            ip,
        })
    }

    async fn connect_timeout(
        addr: &SocketAddr,
        timeout: Duration,
    ) -> Result<TcpStream, WhoIsError> {
        async_timeout(timeout, TcpStream::connect(addr)).await?.map_err(Into::into)
    }

    async fn _lookup_inner(
        server: &WhoIsServerValue,
        text: &str,
        timeout: Option<Duration>,
        follow: u16,
    ) -> Result<String, WhoIsError> {
        let addr = match &server.host.as_host() {
            Host::Domain(domain) => {
                if domain.get_port().is_some() {
                    domain.get_full_domain().to_string()
                } else {
                    format!("{}:{}", domain.get_full_domain(), DEFAULT_WHOIS_HOST_PORT)
                }
            }
            Host::IPv4(ipv4) => {
                if ipv4.get_port().is_some() {
                    ipv4.get_full_ipv4().to_string()
                } else {
                    format!("{}:{}", ipv4.get_full_ipv4(), DEFAULT_WHOIS_HOST_PORT)
                }
            }
            Host::IPv6(ipv6) => {
                if ipv6.get_port().is_some() {
                    ipv6.get_full_ipv6().to_string()
                } else {
                    format!("[{}]:{}", ipv6.get_full_ipv6(), DEFAULT_WHOIS_HOST_PORT)
                }
            }
        };

        let mut client = if let Some(timeout) = timeout {
            let socket_addrs: Vec<SocketAddr> = addr.to_socket_addrs().await?.collect();

            let mut client = None;

            for socket_addr in socket_addrs.iter().take(socket_addrs.len() - 1) {
                if let Ok(c) = Self::connect_timeout(&socket_addr, timeout).await {
                    client = Some(c);
                    break;
                }
            }

            let client = if let Some(client) = client {
                client
            } else {
                let socket_addr = &socket_addrs[socket_addrs.len() - 1];
                Self::connect_timeout(&socket_addr, timeout).await?
            };

            // client.set_read_timeout(Some(timeout))?;
            // client.set_write_timeout(Some(timeout))?;
            client
        } else {
            TcpStream::connect(&addr).await?
        };

        if let Some(query) = &server.query {
            client.write_all(query.replace("$addr", text).as_bytes()).await?;
        } else {
            client.write_all(DEFAULT_WHOIS_HOST_QUERY.replace("$addr", text).as_bytes()).await?;
        }

        client.flush().await?;

        let mut query_result = String::new();

        client.read_to_string(&mut query_result).await?;

        if follow > 0 {
            if let Some(c) = REF_SERVER_RE.captures(&query_result) {
                if let Some(h) = c.get(3) {
                    let h = h.as_str();
                    if h.ne(&addr) {
                        if let Ok(server) = WhoIsServerValue::from_string(h) {
                            return Err(WhoIsError::RetryError(server, follow - 1));
                        }
                    }
                }
            }
        }

        Ok(query_result)
    }

    async fn lookup_inner(
        server: &WhoIsServerValue,
        text: &str,
        timeout: Option<Duration>,
        follow: u16,
    ) -> Result<String, WhoIsError> {
        let mut follow = follow;
        let mut server = server.clone();
        while follow > 0 {
            match Self::_lookup_inner(&server, text, timeout, follow).await {
                Ok(x) => return Ok(x),
                Err(WhoIsError::RetryError(s, f)) => {
                    server = s;
                    follow = f;
                }
                Err(e) => return Err(e),
            }
        }
        Err(WhoIsError::MapError("Max follow".to_string()))
    }

    /// Lookup a domain or an IP.
    pub async fn lookup(&self, options: WhoIsLookupOptions) -> Result<String, WhoIsError> {
        match &options.target {
            Target::IPv4(ipv4) => {
                let server = match &options.server {
                    Some(server) => server,
                    None => &self.ip,
                };
                Self::lookup_inner(server, ipv4.get_full_ipv4(), options.timeout, options.follow).await
            }
            Target::IPv6(ipv6) => {
                let server = match &options.server {
                    Some(server) => server,
                    None => &self.ip,
                };
                Self::lookup_inner(server, ipv6.get_full_ipv6(), options.timeout, options.follow).await
            }
            Target::Domain(domain) => {
                let mut tld = domain.get_full_domain();
                let server = match &options.server {
                    Some(server) => server,
                    None => {
                        let mut server;
                        loop {
                            server = self.map.get(tld);

                            if server.is_some() {
                                break;
                            }

                            if tld.is_empty() {
                                break;
                            }

                            match tld.find('.') {
                                Some(index) => {
                                    tld = &tld[index + 1..];
                                }
                                None => {
                                    tld = "";
                                }
                            }
                        }
                        match server {
                            Some(server) => server,
                            None => {
                                return Err(WhoIsError::MapError(
                                    "No whois server is known for this kind of object.".to_string(),
                                ))
                            }
                        }
                    }
                };

                if server.punycode {
                    let punycode_domain = domain_to_ascii(domain.get_full_domain()).unwrap();
                    Self::lookup_inner(server, &punycode_domain, options.timeout, options.follow).await
                } else {
                    Self::lookup_inner(
                        server,
                        domain.get_full_domain(),
                        options.timeout,
                        options.follow,
                    ).await
                }
            }
        }
    }
}
