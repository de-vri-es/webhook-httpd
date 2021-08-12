use std::marker::Unpin;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

pub enum MaybeTls<T> {
	Plain(T),
	Tls(tokio_openssl::SslStream<T>),
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncRead for MaybeTls<T> {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf) -> Poll<std::io::Result<()>> {
		match self.get_mut() {
			Self::Plain(x) => Pin::new(x).poll_read(cx, buf),
			Self::Tls(x) => Pin::new(x).poll_read(cx, buf),
		}
	}
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncWrite for MaybeTls<T> {
	fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
		match self.get_mut() {
			Self::Plain(x) => Pin::new(x).poll_write(cx, buf),
			Self::Tls(x) => Pin::new(x).poll_write(cx, buf),
		}
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
		match self.get_mut() {
			Self::Plain(x) => Pin::new(x).poll_flush(cx),
			Self::Tls(x) => Pin::new(x).poll_flush(cx),
		}
	}

	fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
		match self.get_mut() {
			Self::Plain(x) => Pin::new(x).poll_shutdown(cx),
			Self::Tls(x) => {
				// Swallow not connected error when trying to shut-down, connection is closed anyway.
				// If we don't swallow them, hyper always gives errors for TLS connections.
				match Pin::new(x).poll_shutdown(cx) {
					Poll::Ready(Err(ref e)) if e.kind() == std::io::ErrorKind::NotConnected => Poll::Ready(Ok(())),
					x => x,
				}
			}
		}
	}
}
