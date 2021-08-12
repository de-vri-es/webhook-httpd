use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use openssl::ssl::{Ssl, SslAcceptor};
use tokio::net::TcpStream;
use tokio_openssl::SslStream;

use crate::config;

const RELOAD_AFTER_SUCCESS: Duration = Duration::from_secs(3600 * 24);
const RELOAD_AFTER_ERROR: Duration = Duration::from_secs(60);
const RELOAD_AFTER_ERROR_MAX: Duration = Duration::from_secs(3600);

pub struct TlsAcceptor {
	certificate_chain: PathBuf,
	private_key: PathBuf,
	acceptor: SslAcceptor,
	next_reload: Instant,
	fail_timeout: Duration,
}

impl TlsAcceptor {
	/// Create an acceptor from a configuration.
	pub fn from_config(config: &config::Tls) -> Result<Self, ()> {
		let certificate_chain = config.certificate_chain.clone();
		let private_key = config.private_key.clone();
		let acceptor = load_tls_files(&certificate_chain, &private_key)?;
		Ok(Self {
			certificate_chain,
			private_key,
			acceptor,
			next_reload: Instant::now() + RELOAD_AFTER_SUCCESS,
			fail_timeout: RELOAD_AFTER_ERROR,
		})
	}

	/// Reload the certificate chain and private key from disk.
	pub fn reload(&mut self) -> Result<(), ()> {
		match load_tls_files(&self.certificate_chain, &self.private_key) {
			Ok(acceptor) => {
				self.acceptor = acceptor;
				self.next_reload = Instant::now() + RELOAD_AFTER_SUCCESS;
				self.fail_timeout = RELOAD_AFTER_ERROR;
				Ok(())
			},
			Err(e) => {
				self.next_reload = Instant::now() + self.fail_timeout;
				self.fail_timeout = (self.fail_timeout * 2).min(RELOAD_AFTER_ERROR_MAX);
				Err(e)
			}
		}
	}

	pub async fn accept(&mut self, connection: TcpStream) -> Result<SslStream<TcpStream>, ()> {
		if Instant::now() >= self.next_reload {
			log::info!("reloading TLS certificate and key.");
			self.reload().ok();
		}
		let session = Ssl::new(self.acceptor.context())
			.map_err(|e| log::error!("failed to create new TLS session: {}", e))?;
		let mut stream = SslStream::new(session, connection)
			.map_err(|e| log::error!("failed to create TLS stream: {}", e))?;
		Pin::new(&mut stream)
			.accept()
			.await
			.map_err(|e| log::error!("TLS handshake failed: {}", e))?;
		Ok(stream)
	}
}

fn load_tls_files(certificate_chain: &Path, private_key: &Path) -> Result<openssl::ssl::SslAcceptor, ()> {
	let mut builder = openssl::ssl::SslAcceptor::mozilla_modern_v5(openssl::ssl::SslMethod::tls_server())
		.map_err(|e| log::error!("failed to configure TLS acceptor: {}", e))?;
	builder.set_private_key_file(private_key, openssl::ssl::SslFiletype::PEM)
		.map_err(|e| log::error!("failed to load private key from {}: {}", private_key.display(), e))?;
	builder.set_certificate_chain_file(certificate_chain)
		.map_err(|e| log::error!("failed to load certificate chain from {}: {}", certificate_chain.display(), e))?;
	Ok(builder.build())
}
