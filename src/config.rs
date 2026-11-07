//! Process configuration: command-line flags, each with an `MM_` environment
//! variable. Everything else lives in the database. `MM_TRUST_PROXY` and
//! `MM_SETUP_TOKEN` are read by vole-kit itself.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use clap::Parser;

/// Mindful Magpie: a self-hosted start page for your links and services.
#[derive(Debug, Clone, Parser)]
#[command(name = "mindful-magpie", version, about, long_about = None)]
pub struct Config {
    /// Directory for the database (magpie.sqlite), the session key and
    /// pre-migration backups.
    #[arg(long, env = "MM_DATA_DIR", default_value = "/data")]
    pub data_dir: PathBuf,

    /// Address to listen on.
    #[arg(long, env = "MM_BIND", default_value_t = IpAddr::V4(Ipv4Addr::UNSPECIFIED))]
    pub bind: IpAddr,

    /// Port to listen on.
    #[arg(long, env = "MM_PORT", default_value_t = 3010)]
    pub port: u16,

    /// The address people use to reach this instance, like
    /// https://start.example.com. Used in links and the same-origin checks;
    /// without it, the address of each request is used.
    #[arg(long, env = "MM_PUBLIC_URL", value_parser = parse_public_url)]
    pub public_url: Option<String>,
}

impl Config {
    pub fn bind_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.port)
    }
}

/// An http(s) URL for the root of a host, without query or fragment, returned
/// without a trailing slash. Hosting under a subpath is not supported.
fn parse_public_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let url = url::Url::parse(raw).map_err(|e| format!("not a URL ({e})"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("use an http:// or https:// address, like https://start.example.com".into());
    }
    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
        return Err("leave out user names, queries and fragments".into());
    }
    if url.path() != "/" {
        return Err(format!(
            "serve Magpie at the root of its own address; hosting under the path {} is not supported",
            url.path().trim_end_matches('/')
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_defaults() {
        let c = Config::try_parse_from(["mindful-magpie"]).unwrap();
        assert_eq!(c.data_dir, PathBuf::from("/data"));
        assert_eq!(c.port, 3010);
        assert_eq!(c.bind_addr().to_string(), "0.0.0.0:3010");
        assert_eq!(c.public_url, None);
        let c = Config::try_parse_from([
            "mindful-magpie",
            "--port",
            "8080",
            "--bind",
            "127.0.0.1",
            "--public-url",
            "https://start.example.com/",
        ])
        .unwrap();
        assert_eq!(c.bind_addr().to_string(), "127.0.0.1:8080");
        assert_eq!(c.public_url.as_deref(), Some("https://start.example.com"));
    }

    #[test]
    fn public_urls_are_checked() {
        assert_eq!(
            parse_public_url("https://example.com/").as_deref(),
            Ok("https://example.com")
        );
        assert_eq!(
            parse_public_url(" http://localhost:3010 ").as_deref(),
            Ok("http://localhost:3010")
        );
        for bad in ["example.com", "ftp://x", "https://x?y=1", "https://u@x"] {
            assert!(parse_public_url(bad).is_err(), "{bad}");
        }
        // Subpath hosting does not work, so a path is refused with a clear message.
        for bad in ["https://example.com/start", "https://example.com/start/"] {
            let err = parse_public_url(bad).unwrap_err();
            assert!(
                err.contains("not supported") && err.contains("/start"),
                "{bad}: {err}"
            );
        }
    }
}
