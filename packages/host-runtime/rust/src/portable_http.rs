//! Native transport for pinned npm payloads. The caller supplies its TLS/proxy
//! profile; automatic decoding must stay off so raw response policy runs first.
use crate::{IvyError, IvyResult, portable_dependencies::{MAXIMUM_PORTABLE_PACKAGE_BYTES, check_portable_response}};
use async_compression::{codecs::{self, DecodeV2}, core::util::{PartialBuffer, WriteBuffer}};
use futures_util::TryStreamExt;
use ivy_contracts::JsString;
use reqwest::{Client, ClientBuilder, Response, Url, header::{self, HeaderMap}};
use std::{io, pin::Pin, task::{Context, Poll}, time::Duration};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader, ReadBuf};
use tokio_util::io::StreamReader;

#[derive(Clone)]
pub struct PortableHttp { client: Client }

impl PortableHttp {
    /// Retain the caller's explicit certificate/proxy inputs. Redirects and the
    /// request/body deadline belong to this adapter, rather than each hop.
    pub fn new(builder: ClientBuilder) -> IvyResult<Self> {
        let client = builder.no_gzip().no_brotli().no_deflate().no_zstd()
            .redirect(reqwest::redirect::Policy::none()).referer(false)
            .retry(reqwest::retry::never()).http1_only().user_agent("node")
            .build().map_err(|_| IvyError::internal())?;
        Ok(Self { client })
    }

    /// The original fetch deadline covers all redirects and the decoded body.
    /// Package checksum/nonempty policy runs in the subsequent install stage.
    pub async fn fetch(&self, resolved: &JsString) -> IvyResult<Vec<u8>> {
        tokio::time::timeout(Duration::from_secs(120), self.fetch_body(resolved)).await
            .map_err(|_| IvyError::internal())?
    }

    async fn fetch_body(&self, resolved: &JsString) -> IvyResult<Vec<u8>> {
        let mut url = Url::parse(&String::from_utf16_lossy(&resolved.0)).map_err(|_| IvyError::internal())?;
        for redirects in 0..=20 {
            request_url(&url)?;
            let response = self.client.get(url.clone()).header(header::ACCEPT, "*/*")
                .header(header::ACCEPT_LANGUAGE, "*").header("sec-fetch-mode", "cors")
                .header(header::ACCEPT_ENCODING, if url.scheme() == "https" { "br, gzip, deflate" } else { "gzip, deflate" })
                .send().await.map_err(|_| IvyError::internal())?;
            let status = response.status().as_u16();
            let location = combined_header(response.headers(), header::LOCATION);
            let redirect = matches!(status, 301 | 302 | 303 | 307 | 308);
            // Undici selects/rejects codings before publishing the response,
            // even for a non-success status. Followed redirects skip decoders.
            let codings = response_codings(&response, redirect && location.as_ref().is_some_and(|v| !v.is_empty()))?;
            if redirect && let Some(location) = location {
                if redirects == 20 { return Err(IvyError::internal()) }
                let previous_fragment = url.fragment().map(str::to_owned);
                // Raw Location bytes are interpreted as UTF-8, including the
                // replacement of malformed bytes, by the original fetch path.
                let bytes: Vec<_> = location.chars().map(|c| c as u8).collect();
                url = url.join(&String::from_utf8_lossy(&bytes)).map_err(|_| IvyError::internal())?;
                if url.fragment().is_none_or(str::is_empty) { url.set_fragment(previous_fragment.as_deref()); }
                continue;
            }
            let declared = combined_header(response.headers(), header::CONTENT_LENGTH);
            check_portable_response(response.status().is_success(), declared.as_deref())?;
            if matches!(status, 204 | 205 | 304) { return Ok(Vec::new()) }
            let body = response.bytes_stream().map_err(io::Error::other);
            return decoded_body(Box::new(StreamReader::new(body)), &codings).await.map_err(|_| IvyError::internal());
        }
        Err(IvyError::internal())
    }
}

fn request_url(url: &Url) -> IvyResult<()> {
    // Fetch rejects credentials and the standard bad-port list before opening
    // a connection. The planner separately requires an initial HTTPS locator.
    const BLOCKED: &[u16] = &[1,7,9,11,13,15,17,19,20,21,22,23,25,37,42,43,53,69,77,79,87,95,101,102,103,104,109,110,111,113,115,117,119,123,135,137,139,143,161,179,389,427,465,512,513,514,515,526,530,531,532,540,548,554,556,563,587,601,636,989,990,993,995,1719,1720,1723,2049,3659,4045,4190,5060,5061,6000,6566,6665,6666,6667,6668,6669,6679,6697,10080];
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some()
        || url.port().is_some_and(|port| BLOCKED.contains(&port)) { return Err(IvyError::internal()) }
    Ok(())
}
fn combined_header(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    let values: Vec<_> = headers.get_all(name).iter().map(|value| value.as_bytes().iter().map(|b| char::from(*b)).collect::<String>()).collect();
    (!values.is_empty()).then(|| values.join(", "))
}
#[derive(Clone, Copy)]
enum Coding { Gzip, Deflate, Brotli, Zstd }
fn response_codings(response: &Response, followed: bool) -> IvyResult<Vec<Coding>> {
    if followed || matches!(response.status().as_u16(), 101 | 103 | 204 | 205 | 304) { return Ok(Vec::new()) }
    let Some(header) = combined_header(response.headers(), header::CONTENT_ENCODING).filter(|value| !value.is_empty()) else { return Ok(Vec::new()) };
    let values: Vec<_> = header.split(',').collect();
    if values.len() > 5 { return Err(IvyError::internal()) }
    let mut codings = Vec::new();
    for value in values.into_iter().rev() {
        let value = value.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0b' | '\x0c' | '\u{a0}')).to_ascii_lowercase();
        codings.push(match value.as_str() {
            "gzip" | "x-gzip" => Coding::Gzip, "deflate" => Coding::Deflate, "br" => Coding::Brotli, "zstd" => Coding::Zstd,
            _ => return Ok(Vec::new()),
        });
    }
    Ok(codings)
}

type Reader = Box<dyn AsyncRead + Unpin + Send>;
// The codecs' flush operation matches fetch's lenient final flush. Their finish
// operation enforces a complete frame, which changes truncated-response policy.
// Gzip alone restarts for concatenated members; a zero byte ends its padding.
struct FlushDecoder {
    reader: BufReader<Reader>, codec: Box<dyn DecodeV2 + Send>,
    gzip: bool, member_end: bool, flushing: bool, draining: bool, ended: bool,
}
impl FlushDecoder {
    fn poll_decode(&mut self, cx: &mut Context<'_>, output: &mut WriteBuffer<'_>) -> Poll<io::Result<()>> {
        let mut consumed_bytes = 0;
        loop {
            if self.ended { return Poll::Ready(Ok(())) }
            if self.flushing {
                if self.codec.flush(output)? { self.ended = true; }
            } else {
                let bytes = match Pin::new(&mut self.reader).poll_fill_buf(cx) {
                    Poll::Ready(value) => value?,
                    Poll::Pending if !output.written().is_empty() => return Poll::Ready(Ok(())),
                    Poll::Pending => return Poll::Pending,
                };
                if bytes.is_empty() { self.flushing = true; continue }
                // Finish consuming every upstream codec and the transport even
                // when this codec ignores trailing bytes after its first frame.
                if self.draining {
                    let consumed = bytes.len(); Pin::new(&mut self.reader).consume(consumed); consumed_bytes += consumed;
                } else {
                if self.member_end {
                    if !self.gzip || bytes[0] == 0 { self.draining = true; continue }
                    self.codec.reinit()?; self.member_end = false;
                }
                let mut input = PartialBuffer::new(bytes);
                let done = self.codec.decode(&mut input, output)?;
                let consumed = input.written().len();
                Pin::new(&mut self.reader).consume(consumed);
                consumed_bytes += consumed;
                if done { self.member_end = true; if !self.gzip { self.draining = true; } }
                }
            }
            if output.has_no_spare_space() || self.ended { return Poll::Ready(Ok(())) }
            if consumed_bytes >= 64 * 1024 {
                if !output.written().is_empty() { return Poll::Ready(Ok(())) }
                cx.waker().wake_by_ref(); return Poll::Pending;
            }
        }
    }
}
impl AsyncRead for FlushDecoder {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, output: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if output.remaining() == 0 { return Poll::Ready(Ok(())) }
        let mut bytes = WriteBuffer::new_initialized(output.initialize_unfilled());
        let result = self.poll_decode(cx, &mut bytes); let written = bytes.written().len();
        output.advance(written); result
    }
}
async fn decoded_body(mut reader: Reader, codings: &[Coding]) -> io::Result<Vec<u8>> {
    for coding in codings {
        let mut buffered = BufReader::new(reader);
        let Some(first) = buffered.fill_buf().await?.first().copied() else { return Ok(Vec::new()) };
        let codec: Box<dyn DecodeV2 + Send> = match coding {
            Coding::Gzip => Box::new(codecs::GzipDecoder::new()),
            Coding::Deflate if first & 0x0f == 8 => Box::new(codecs::ZlibDecoder::new()),
            Coding::Deflate => Box::new(codecs::DeflateDecoder::new()),
            Coding::Brotli => Box::new(codecs::BrotliDecoder::new()),
            Coding::Zstd => Box::new(codecs::ZstdDecoder::new()),
        };
        reader = Box::new(FlushDecoder { reader: buffered, codec, gzip: matches!(coding, Coding::Gzip), member_end: false, flushing: false, draining: false, ended: false });
    }
    let mut bytes = Vec::new(); let mut overflow = false; let mut buffer = [0; 32 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 { break }
        if bytes.len() + read <= MAXIMUM_PORTABLE_PACKAGE_BYTES && !overflow { bytes.extend_from_slice(&buffer[..read]); }
        else { overflow = true; }
        tokio::task::yield_now().await;
    }
    // Preserve the original read-to-completion/error order without retaining
    // unbounded decoded data. Install applies the same checksum-limit error.
    if overflow { bytes.clear(); }
    Ok(bytes)
}
