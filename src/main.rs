use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use structopt::clap::AppSettings;
use structopt::StructOpt;
use tokio::net::TcpListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{oneshot, watch};
use tokio::stream::StreamExt;

mod config;
mod logging;
mod maybe_tls;
mod scheduler;
mod types;

use config::{Config, Hook};
use maybe_tls::MaybeTls;
use scheduler::Scheduler;

use hyper::{server::conn::Http, Body, StatusCode};
type Request = hyper::Request<Body>;
type Response = hyper::Response<Body>;

#[derive(StructOpt)]
#[structopt(setting = AppSettings::ColoredHelp)]
#[structopt(setting = AppSettings::DeriveDisplayOrder)]
#[structopt(setting = AppSettings::UnifiedHelpMessage)]
struct Options {
	/// The configuration file to load.
	#[structopt(long, short)]
	#[structopt(required_unless = "print-example-hooks")]
	config: PathBuf,

	/// Print example configuration and exit.
	#[structopt(long)]
	print_example_config: bool,

	/// Override the log level.
	#[structopt(long, short)]
	#[structopt(value_name = "LOG-LEVEL")]
	#[structopt(possible_values = &["error", "warn", "info", "debug", "trace"])]
	log_level: Option<log::LevelFilter>,
}

fn main() {
	let options = Options::from_args();

	if options.print_example_config {
		println!("{}", Config::example());
	} else {
		match Config::read_from_file(&options.config) {
			Err(e) => {
				logging::init(module_path!(), options.log_level.unwrap_or(log::LevelFilter::Info));
				log::error!("{}", e);
				std::process::exit(1);
			}
			Ok(config) => {
				logging::init(module_path!(), options.log_level.unwrap_or(config.log_level));
				if let Err(()) = do_main(config) {
					std::process::exit(1);
				}
			},
		}
	}
}

fn do_main(config: Config) -> Result<(), ()> {

	let mut runtime = tokio::runtime::Builder::new()
		.basic_scheduler()
		.enable_all()
		.build()
		.map_err(|e| log::error!("failed to create tokio runtime: {}", e))?;

	runtime.block_on(async move {
		let (mut stop_tx, stop_rx) = watch::channel(false);

		let tls_acceptor = match &config.tls {
			None => None,
			Some(tls_config) => Some(load_tls_files(tls_config)?),
		};

		let socket_address = config.socket_address();
		let hooks = Arc::new(build_hook_schedulers(config.hooks, stop_rx)?);

		let listener = TcpListener::bind(socket_address)
			.await
			.map_err(|e| log::error!("failed to bind listening TCP socket to {}: {}", socket_address, e))?;

		if tls_acceptor.is_some() {
			log::info!("listening for HTTPS connections on {}", socket_address);
		} else {
			log::info!("listening for HTTP connections on {}", socket_address);
		}


		let mut signals = hook_signals()?;
		tokio::spawn(run_server(listener, tls_acceptor, socket_address, hooks));

		let signal = signals.next().await
			.ok_or_else(|| log::error!("signal stream closed unexpectedly"))?;
		log::info!("received {} signal", signal);
		stop_tx.broadcast(true).unwrap_or(());
		stop_tx.closed().await;
		Ok(())
	})
}

fn load_tls_files(config: &config::Tls) -> Result<openssl::ssl::SslAcceptor, ()> {
	let mut builder = openssl::ssl::SslAcceptor::mozilla_modern_v5(openssl::ssl::SslMethod::tls_server())
		.map_err(|e| log::error!("failed to configure TLS acceptor: {}", e))?;
	builder.set_private_key_file(&config.private_key, openssl::ssl::SslFiletype::PEM)
		.map_err(|e| log::error!("failed to load private key from {}: {}", config.certificate_chain.display(), e))?;
	builder.set_certificate_chain_file(&config.certificate_chain)
		.map_err(|e| log::error!("failed to load certificate chain from {}: {}", config.certificate_chain.display(), e))?;
	Ok(builder.build())
}

fn build_hook_schedulers(hooks: Vec<Hook>, stop_rx: watch::Receiver<bool>) -> Result<BTreeMap<String, HookScheduler>, ()> {
	let mut hook_schedulers = BTreeMap::new();
	for hook in hooks {
		let url = hook.url.clone();
		let scheduler = HookScheduler::new(hook, stop_rx.clone());
		if hook_schedulers.insert(url.clone(), scheduler).is_some() {
			log::error!("multiple hooks defined for URL: {}", url);
			return Err(());
		}
		log::info!("loaded hook for URL: {}", url)
	}
	log::info!("loaded {} hooks", hook_schedulers.len());
	Ok(hook_schedulers)
}

fn hook_signals() -> Result<impl tokio::stream::Stream<Item = &'static str>, ()> {
	let sigint = signal(SignalKind::interrupt())
		.map_err(|e| log::error!("failed to register SIGINT handler: {}", e))?
		.map(|()| "SIGINT");
	let sigterm = signal(SignalKind::terminate())
		.map_err(|e| log::error!("failed to register SIGTERM handler: {}", e))?
		.map(|()| "SIGTERM");
	Ok(sigint.merge(sigterm))
}

async fn run_server(
	mut listener: TcpListener,
	tls_acceptor: Option<openssl::ssl::SslAcceptor>,
	socket_address: std::net::SocketAddr,
	hooks: Arc<BTreeMap<String, HookScheduler>>,
) -> Result<(), ()> {
	loop {
		let (connection, addr) = listener
			.accept()
			.await
			.map_err(|e| log::error!("failed to accept connection on {}: {}", socket_address, e))?;
		log::debug!("accepted new connection from {}", addr);

		let connection = if let Some(tls_acceptor) = tls_acceptor.as_ref() {
			match tokio_openssl::accept(tls_acceptor, connection).await {
				Ok(x) => MaybeTls::Tls(x),
				Err(e) => {
					log::error!("TLS handshake failed: {}", e);
					continue;
				}
			}
		} else {
			MaybeTls::Plain(connection)
		};

		let hooks = hooks.clone();
		tokio::spawn(async move {
			let handler = hyper::service::service_fn(move |request| {
				let hooks = hooks.clone();
				async move { handle_request(&hooks, request, addr).await }
			});
			Http::new()
				.http1_keep_alive(true)
				.http2_keep_alive_interval(Some(Duration::from_secs(20)))
				.serve_connection(connection, handler)
				.await
				.unwrap_or_else(|e| log::error!("error while serving HTTP connection: {}", e))
		});
	}
}

struct HookScheduler {
	hook: Hook,
	scheduler: Scheduler,
}

impl HookScheduler {
	fn new(hook: Hook, stop_rx: watch::Receiver<bool>) -> Self {
		let scheduler = Scheduler::new(hook.max_concurrent.bound(), hook.queue_size.bound(), hook.queue_type, stop_rx);
		Self { hook, scheduler }
	}

	async fn post(&self, request: Request, body: Vec<u8>, remote_addr: SocketAddr) -> Response {
		let (done_tx, done_rx) = oneshot::channel();
		let job = self.make_job(request, body, remote_addr, done_tx);
		if let Err(e) = self.scheduler.post(job).await {
			log::error!("{}: scheduler did not accept new job: {}", self.hook.url, e);
			generic_error()
		} else {
			log::debug!("{}: job posted", self.hook.url);
			match done_rx.await {
				Ok(response) => response,
				Err(_) => {
					log::error!("{}: job was dropped before it finished", self.hook.url);
					simple_response(StatusCode::INTERNAL_SERVER_ERROR, "job dropped due to resource limits")
				},
			}
		}
	}

	fn make_job(&self, request: Request, body: Vec<u8>, remote_addr: SocketAddr, done_tx: oneshot::Sender<Response>) -> scheduler::Job {
		let hook = self.hook.clone();
		Box::pin(async move {
			for cmd in &hook.commands {
				if let Err(()) = run_command(&cmd, &hook, &request, &body, remote_addr).await {
					// Ignore errors: other end of channel was dropped, nobody cares about our response anymore.
					done_tx.send(generic_error()).unwrap_or(());
					return;
				}
			}
			// Ignore errors: other end of channel was dropped, nobody cares about our response anymore.
			done_tx.send(simple_response(StatusCode::OK, "thank you, come again")).unwrap_or(());
		})
	}
}

async fn handle_request(hooks: &BTreeMap<String, HookScheduler>, mut request: Request, remote_addr: SocketAddr) -> Result<Response, std::convert::Infallible> {
	log::trace!("received {} request for {}", request.method(), request.uri().path());
	if request.method() != hyper::Method::POST {
		return Ok(simple_response(hyper::StatusCode::METHOD_NOT_ALLOWED, "hooks must use the POST method"));
	}

	// Look up the hook in the map.
	let hook = match hooks.get(request.uri().path()) {
		Some(x) => x,
		None => {
			log::warn!("{}: no hook found", request.uri().path());
			return Ok(simple_response(StatusCode::NOT_FOUND, "no hook found"));
		},
	};

	// Collect the body so we can feed it to a command and to check the signature.
	let body = match collect_body(request.body_mut()).await {
		Ok(x) => x,
		Err(e) => {
			log::error!("{}: failed to read request body: {}", hook.hook.url,  e);
			return Ok(simple_response(StatusCode::BAD_REQUEST, "failed to read request body"));
		},
	};

	if let Some(secret) = &hook.hook.secret {
		let signature = match get_signature_header(request.headers()) {
			Ok(x) => x,
			Err(e) => {
				log::error!("{}: {}", hook.hook.url, e);
				return Ok(simple_response(StatusCode::BAD_REQUEST, "missing or invalid X-Hub-Signature-256 header"));
			},
		};
		let digest = compute_digest(secret, &body);
		if !digest.eq_ignore_ascii_case(signature) {
			log::error!("{}: request signature ({}) does not match the payload digest ({})", hook.hook.url, signature, digest);
			return Ok(simple_response(StatusCode::BAD_REQUEST, "invalid X-Hub-Signature-256 header"));
		}
		log::trace!("{}: X-Hub-Signature-256 signature matches: {}", hook.hook.url, digest);
	}

	log::info!("{}: triggering hook", hook.hook.url);
	Ok(hook.post(request, body, remote_addr).await)
}

async fn run_command(cmd: &config::Command, hook: &Hook, request: &Request, body: &[u8], remote_addr: SocketAddr) -> Result<(), ()> {
	use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

	let mut command = tokio::process::Command::new(cmd.cmd());
	command.args(cmd.args());
	if cmd.wants_request_body() {
		command.stdin(std::process::Stdio::piped());
	}
	command.stdout(std::process::Stdio::piped());
	command.stderr(std::process::Stdio::piped());
	if let Some(working_dir) = &hook.working_dir {
		command.current_dir(working_dir);
	}

	// Fill in environment variables based on request information.
	if cmd.wants_request_body() {
		set_request_environment(&mut command, request, Some(body), remote_addr);
	} else {
		set_request_environment(&mut command, request, None, remote_addr);
	}

	let mut subprocess = command.spawn().map_err(|e| log::error!("{}: failed to run command {:?}: {}", hook.url, cmd.cmd(), e))?;

	if let Some(mut stdin) = subprocess.stdin.take() {
		stdin
			.write_all(body)
			.await
			.map_err(|e| log::error!("{}: failed to write request body to stdin of command {:?}: {}", hook.url, cmd.cmd(), e))?;
	}

	let stdout = tokio::io::BufReader::new(subprocess.stdout.take().unwrap());
	let stderr = tokio::io::BufReader::new(subprocess.stderr.take().unwrap());
	let mut output = stdout.lines().merge(stderr.lines());

	while let Some(line) = output.next().await {
		let line = match line {
			Ok(x) => x,
			Err(e) => {
				log::error!("{}: failed to read command {:?} output: {}", hook.url, cmd.cmd(), e);
				break;
			},
		};
		log::info!("{}: {}: {}", hook.url, cmd.cmd(), line);
	}

	let status = subprocess
		.await
		.map_err(|e| log::error!("{}: failed to wait for command {:?}: {}", hook.url, cmd.cmd(), e))?;
	if status.success() {
		Ok(())
	} else {
		log::error!("{}: command {:?} exitted with {}", hook.url, cmd.cmd(), status);
		Err(())
	}
}

fn get_signature_header(headers: &hyper::HeaderMap) -> Result<&str, String> {
	let signature = headers.get("X-Hub-Signature-256")
		.ok_or("request is not signed with a X-Hub-Signature-256 header")?
		.to_str()
		.map_err(|e| format!("malformed X-Hub-Signature-256 header: {}", e))?
		.strip_prefix("sha256=")
		.ok_or("malformed X-Hub-Signature-256 header: signature does not start with 'sha256='")?;
	Ok(signature)
}

fn compute_digest(secret: &str, data: &[u8]) -> String {
	use hmac::{Mac, NewMac};
	// HMAC keys can be any size, so unwrap can't fail.
	let mut digest = hmac::Hmac::<sha2::Sha256>::new_varkey(secret.as_bytes()).unwrap();
	digest.update(data);
	let digest = digest.finalize();
	to_hex(&digest.into_bytes())
}

fn to_hex(data: &[u8]) -> String {
	let mut output = String::with_capacity(data.len() * 2);
	for &byte in data {
		output.push(std::char::from_digit(byte as u32 >> 4, 16).unwrap());
		output.push(std::char::from_digit(byte as u32 & 0x0F, 16).unwrap());
	}
	output
}

async fn collect_body(body: &mut Body) -> Result<Vec<u8>, hyper::Error> {
	use hyper::body::HttpBody;

	let mut data = Vec::new();
	while let Some(chunk) = body.data().await {
		data.extend_from_slice(chunk?.as_ref());
	}
	Ok(data)
}

fn simple_response(status: StatusCode, data: impl Into<Vec<u8>>) -> Response {
	let mut data: Vec<u8> = data.into();
	if data.last() != Some(&b'\n') {
		data.push(b'\n');
	}
	hyper::Response::builder().status(status).body(data.into()).unwrap()
}

fn generic_error() -> Response {
	simple_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to run hook, see server logs for more details")
}

fn set_request_environment(command: &mut tokio::process::Command, request: &Request, body: Option<&[u8]>, remote_addr: SocketAddr) {
	use std::ffi::OsStr;
	use std::os::unix::ffi::OsStrExt;

	if let Some(body) = body {
		command.env("CONTENT_LENGTH", body.len().to_string());
		if let Some(content_type) = request.headers().get("Content-Type") {
			command.env("CONTENT_TYPE", OsStr::from_bytes(content_type.as_bytes()));
		}
	}
	command.env("URL_PATH", request.uri().path().to_string());
	command.env("URL_QUERY", request.uri().query().unwrap_or(""));
	command.env("REMOTE_ADDRESS", remote_addr.ip().to_string());
	command.env("REMOTE_PORT", remote_addr.port().to_string());
}
