//! HTTPS GET through WinHTTP (Windows' own HTTP stack: proxies, TLS and redirects as the
//! system has them). Used for update checks and downloads, and for League's local API.

use std::io::Write;

use anyhow::{Context, Result, bail};
use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, w};

/// `https://host[:port]/path` → (host, port, path).
pub fn split_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, 443),
    };
    (!host.is_empty()).then(|| (host.to_owned(), port, path.to_owned()))
}

/// Options for one request.
pub struct Get<'a> {
    pub host: &'a str,
    pub port: u16,
    pub path: &'a str,
    /// Accept a certificate Windows doesn't trust (only for League's localhost API).
    pub any_certificate: bool,
    /// Give up after this many bytes.
    pub max_bytes: u64,
    pub timeout_ms: i32,
}

/// Runs the request and writes the body to `out`. Fails on a non-2xx status.
pub fn get(g: &Get<'_>, out: &mut dyn Write) -> Result<u64> {
    let agent = HSTRING::from(format!("Retake/{}", env!("CARGO_PKG_VERSION")));
    // SAFETY: handles are closed below; buffers outlive each call.
    unsafe {
        let session = WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None, None, 0);
        if session.is_null() {
            bail!("WinHttpOpen failed");
        }
        let close = |h: *mut core::ffi::c_void| {
            let _ = WinHttpCloseHandle(h);
        };
        let t = g.timeout_ms;
        let _ = WinHttpSetTimeouts(session, t, t, t, t);
        let conn = WinHttpConnect(session, &HSTRING::from(g.host), g.port, 0);
        if conn.is_null() {
            close(session);
            bail!("connecting to {}", g.host);
        }
        let req = WinHttpOpenRequest(
            conn,
            w!("GET"),
            &HSTRING::from(g.path),
            None,
            None,
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if req.is_null() {
            close(conn);
            close(session);
            bail!("WinHttpOpenRequest failed");
        }
        if g.any_certificate {
            let flags: u32 = SECURITY_FLAG_IGNORE_UNKNOWN_CA
                | SECURITY_FLAG_IGNORE_CERT_CN_INVALID
                | SECURITY_FLAG_IGNORE_CERT_DATE_INVALID;
            let _ = WinHttpSetOption(
                Some(req),
                WINHTTP_OPTION_SECURITY_FLAGS,
                Some(&flags.to_ne_bytes()),
            );
        }
        let result = (|| -> Result<u64> {
            WinHttpSendRequest(req, None, None, 0, 0, 0).context("sending the request")?;
            WinHttpReceiveResponse(req, std::ptr::null_mut()).context("receiving the response")?;
            let mut status = 0u32;
            let mut len = size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                None,
                Some((&raw mut status).cast()),
                &mut len,
                std::ptr::null_mut(),
            )
            .context("reading the status")?;
            if !(200..300).contains(&status) {
                bail!("HTTP {status} from {}{}", g.host, g.path);
            }
            let mut total = 0u64;
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let mut avail = 0u32;
                WinHttpQueryDataAvailable(req, &mut avail)?;
                if avail == 0 {
                    break;
                }
                let want = avail.min(buf.len() as u32);
                let mut read = 0u32;
                WinHttpReadData(req, buf.as_mut_ptr().cast(), want, &mut read)?;
                out.write_all(&buf[..read as usize])?;
                total += u64::from(read);
                if total > g.max_bytes {
                    bail!("response larger than {} bytes", g.max_bytes);
                }
            }
            Ok(total)
        })();
        close(req);
        close(conn);
        close(session);
        result
    }
}

/// GET into memory as text.
pub fn get_text(url: &str, max_bytes: u64) -> Result<String> {
    let (host, port, path) = split_url(url).context("not an https URL")?;
    let mut body = Vec::new();
    get(
        &Get {
            host: &host,
            port,
            path: &path,
            any_certificate: false,
            max_bytes,
            timeout_ms: 15_000,
        },
        &mut body,
    )?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_into_host_port_path() {
        assert_eq!(
            split_url("https://api.github.com/repos/a/b/releases/latest"),
            Some((
                "api.github.com".into(),
                443,
                "/repos/a/b/releases/latest".into()
            ))
        );
        assert_eq!(
            split_url("https://127.0.0.1:2999/liveclientdata/allgamedata"),
            Some((
                "127.0.0.1".into(),
                2999,
                "/liveclientdata/allgamedata".into()
            ))
        );
        assert_eq!(split_url("http://example.com/"), None);
        assert_eq!(split_url("https://h"), Some(("h".into(), 443, "/".into())));
    }
}
