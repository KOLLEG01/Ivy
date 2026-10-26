//! Bounded transport for checksum-pinned npm payloads.
use crate::{
    IvyError, IvyResult,
    portable_dependencies::{MAXIMUM_PORTABLE_PACKAGE_BYTES, check_portable_response},
};
use async_compression::tokio::bufread::{
    BrotliDecoder, DeflateDecoder, GzipDecoder, ZlibDecoder, ZstdDecoder,
};
use futures_util::TryStreamExt;
use ivy_contracts::JsString;
use reqwest::{Client, ClientBuilder, Url, header};
use std::{io, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio_util::io::StreamReader;

#[derive(Clone)]
pub struct PortableHttp {
    client: Client,
}

impl PortableHttp {
    /// Preserve the owner's TLS/proxy settings and use standard HTTP decoding.
    pub fn new(builder: ClientBuilder) -> IvyResult<Self> {
        let client = builder
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Ivy-Host")
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|_| IvyError::internal())?;
        Ok(Self { client })
    }

    /// One deadline covers redirects and decoded bytes. Installation verifies
    /// the nonempty body and its exact lockfile checksum before extraction.
    pub async fn fetch(&self, resolved: &JsString) -> IvyResult<Vec<u8>> {
        tokio::time::timeout(
            Duration::from_secs(120),
            self.fetch_body(&resolved.to_utf8()?),
        )
        .await
        .map_err(|_| IvyError::internal())?
    }

    async fn fetch_body(&self, resolved: &str) -> IvyResult<Vec<u8>> {
        let mut url = Url::parse(resolved).map_err(|_| IvyError::internal())?;
        for redirects in 0..=20 {
            request_url(&url)?;
            let response = self
                .client
                .get(url.clone())
                .header(header::ACCEPT_ENCODING, "gzip, deflate, br, zstd")
                .send()
                .await
                .map_err(|_| IvyError::internal())?;
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308)
                && let Some(location) = response.headers().get(header::LOCATION)
            {
                if redirects == 20 {
                    return Err(IvyError::internal());
                }
                url = url
                    .join(location.to_str().map_err(|_| IvyError::internal())?)
                    .map_err(|_| IvyError::internal())?;
                continue;
            }
            check_portable_response(
                response.status().is_success(),
                response
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .map(|value| value.to_str())
                    .transpose()
                    .map_err(|_| IvyError::internal())?,
            )?;
            let coding = response
                .headers()
                .get(header::CONTENT_ENCODING)
                .map(|value| value.to_str())
                .transpose()
                .map_err(|_| IvyError::internal())?
                .unwrap_or("")
                .to_owned();
            let mut body: Box<dyn AsyncRead + Unpin + Send> = Box::new(StreamReader::new(
                response.bytes_stream().map_err(io::Error::other),
            ));
            for coding in coding
                .split(',')
                .rev()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let mut reader = BufReader::new(body);
                body = match coding.to_ascii_lowercase().as_str() {
                    "identity" => Box::new(reader),
                    "gzip" | "x-gzip" => {
                        let mut decoder = GzipDecoder::new(reader);
                        decoder.multiple_members(true);
                        Box::new(decoder)
                    }
                    "deflate"
                        if reader
                            .fill_buf()
                            .await
                            .map_err(|_| IvyError::internal())?
                            .first()
                            .is_some_and(|byte| byte & 0x0f == 8) =>
                    {
                        Box::new(ZlibDecoder::new(reader))
                    }
                    "deflate" => Box::new(DeflateDecoder::new(reader)),
                    "br" => Box::new(BrotliDecoder::new(reader)),
                    "zstd" => Box::new(ZstdDecoder::new(reader)),
                    _ => {
                        return Err(IvyError::new(
                            "artifact_changed",
                            "Unsupported portable dependency content encoding.",
                        ));
                    }
                };
            }
            let mut bytes = Vec::new();
            body.take(MAXIMUM_PORTABLE_PACKAGE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| IvyError::internal())?;
            if bytes.len() > MAXIMUM_PORTABLE_PACKAGE_BYTES {
                return Err(IvyError::new(
                    "content_too_large",
                    "Portable runtime dependency exceeds its package limit.",
                ));
            }
            return Ok(bytes);
        }
        Err(IvyError::internal())
    }
}

fn request_url(url: &Url) -> IvyResult<()> {
    const BLOCKED: &[u16] = &[
        1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79, 87, 95, 101,
        102, 103, 104, 109, 110, 111, 113, 115, 117, 119, 123, 135, 137, 139, 143, 161, 179, 389,
        427, 465, 512, 513, 514, 515, 526, 530, 531, 532, 540, 548, 554, 556, 563, 587, 601, 636,
        989, 990, 993, 995, 1719, 1720, 1723, 2049, 3659, 4045, 4190, 5060, 5061, 6000, 6566, 6666,
        6667, 6668, 6669, 6697, 10080,
    ];
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url
            .port_or_known_default()
            .is_some_and(|port| BLOCKED.contains(&port))
    {
        return Err(IvyError::internal());
    }
    Ok(())
}
