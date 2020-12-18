use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use structopt::clap::AppSettings;
use structopt::StructOpt;
use tokio::net::TcpListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{oneshot, watch};
use tokio::stream::StreamExt;

mod hooks;
mod logging;
mod scheduler;
mod types;

use scheduler::Scheduler;

use hyper::{server::conn::Http, Body, StatusCode};
type Request = hyper::Request<Body>;
type Response = hyper::Response<Body>;

#[derive(StructOpt)]
#[structopt(setting = AppSettings::ColoredHelp)]
#[structopt(setting = AppSettings::DeriveDisplayOrder)]
#[structopt(setting = AppSettings::UnifiedHelpMessage)]
struct Options {
	/// Show more verbose output (works up to two times).
	#[structopt(long, short)]
	#[structopt(parse(from_occurrences))]
	verbose: i8,

	/// Show less verbose output (works up to two times).
	#[structopt(long, short)]
	#[structopt(parse(from_occurrences))]
	quiet: i8,

	/// Bind the server to a specific address.
	#[structopt(long, short)]
	address: Option<IpAddr>,

	/// Bind to the specified port.
	#[structopt(long, short)]
	#[structopt(default_value = "8091")]
	port: u16,

	/// The TOML file with hooks.
	#[structopt(long, short)]
	#[structopt(value_name = "HOOKS.toml")]
	#[structopt(required_unless = "print-example-hooks")]
	hooks: Option<PathBuf>,

	/// Print example hooks and exit.
	#[structopt(long)]
	print_example_hooks: bool,
}

impl Options {
	fn socket_address(&self) -> std::net::SocketAddr {
		let address = self.address.unwrap_or(std::net::Ipv6Addr::UNSPECIFIED.into());
		std::net::SocketAddr::new(address, self.port)
	}
}

fn main() {
	let options = Options::from_args();
	logging::init(module_path!(), options.verbose - options.quiet);
	if let Err(()) = do_main(options) {
		std::process::exit(1);
	}
}

fn do_main(options: Options) -> Result<(), ()> {
	if options.print_example_hooks {
		println!("{}", hooks::example_hooks());
		return Ok(());
	}

	let mut runtime = tokio::runtime::Builder::new()
		.basic_scheduler()
		.enable_all()
		.build()
		.map_err(|e| log::error!("failed to create tokio runtime: {}", e))?;

	runtime.block_on(async move {
		let (mut stop_tx, stop_rx) = watch::channel(false);
		let hooks = Arc::new(load_hooks(options.hooks.as_ref().unwrap(), stop_rx)?);

		let socket_address = options.socket_address();
		let listener = TcpListener::bind(socket_address)
			.await
			.map_err(|e| log::error!("failed to bind listening TCP socket to {}: {}", socket_address, e))?;
		log::info!("listening for HTTP connections on {}", socket_address);

		let mut signals = hook_signals()?;
		tokio::spawn(run_server(listener, socket_address, hooks));

		let signal = signals.next().await
			.ok_or_else(|| log::error!("signal stream closed unexpectedly"))?;
		log::info!("received {} signal", signal);
		stop_tx.broadcast(true).unwrap_or(());
		stop_tx.closed().await;
		Ok(())
	})
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

async fn run_server(mut listener: TcpListener, socket_address: std::net::SocketAddr, hooks: Arc<BTreeMap<String, HookScheduler>>) -> Result<(), ()> {
	loop {
		let (connection, addr) = listener
			.accept()
			.await
			.map_err(|e| log::error!("failed to accept connection on {}: {}", socket_address, e))?;
		log::debug!("accepted  new connection from {}", addr);

		let hooks = hooks.clone();
		tokio::spawn(async move {
			let handler = hyper::service::service_fn(move |request| {
				let hooks = hooks.clone();
				async move { handle_request(&hooks, request).await }
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

fn load_hooks(path: impl AsRef<Path>, stop_rx: watch::Receiver<bool>) -> Result<BTreeMap<String, HookScheduler>, ()> {
	let path = path.as_ref();
	let data = std::fs::read_to_string(&path).map_err(|e| log::error!("failed to read {}: {}", path.display(), e))?;
	let hook_list = hooks::deserialize(&data).map_err(|e| log::error!("failed to parse hooks from {}: {}", path.display(), e))?;

	let mut hook_map = BTreeMap::new();
	for hook in hook_list {
		let url = hook.url.clone();
		let scheduler = HookScheduler::new(hook, stop_rx.clone());
		if hook_map.insert(url.clone(), scheduler).is_some() {
			log::error!("multiple hooks defined for URL: {}", url);
			return Err(());
		}
		log::info!("loaded hook for URL: {}", url)
	}
	log::info!("loaded {} hooks", hook_map.len());
	Ok(hook_map)
}

struct HookScheduler {
	hook: hooks::Hook,
	scheduler: Scheduler,
}

impl HookScheduler {
	fn new(hook: hooks::Hook, stop_rx: watch::Receiver<bool>) -> Self {
		let scheduler = Scheduler::new(hook.max_concurrent.bound(), hook.queue_size.bound(), hook.queue_type, stop_rx);
		Self { hook, scheduler }
	}

	async fn post(&self, body: Vec<u8>) -> Response {
		let (done_tx, done_rx) = oneshot::channel();
		let job = self.make_job(body, done_tx);
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

	fn make_job(&self, body: Vec<u8>, done_tx: oneshot::Sender<Response>) -> scheduler::Job {
		let hook = self.hook.clone();
		Box::pin(async move {
			for cmd in &hook.commands {
				if let Err(()) = run_command(&cmd, &hook, &body).await {
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

async fn handle_request(hooks: &BTreeMap<String, HookScheduler>, mut request: Request) -> Result<Response, std::convert::Infallible> {
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
		let signature = match request.headers().get("X-Hub-Signature-256") {
			Some(x) => x,
			None => {
				log::error!("{}: request is not signed with a X-Hub-Signature-256 header", hook.hook.url);
				return Ok(simple_response(
					StatusCode::BAD_REQUEST,
					"request must be signed with X-Hub-Signature-256 header",
				));
			},
		};
		let signature = match signature.to_str() {
			Ok(x) => x,
			Err(e) => {
				log::error!("{}: malformed X-Hub-Signature-256 header: {}", hook.hook.url, e);
				return Ok(simple_response(StatusCode::BAD_REQUEST, "invalid X-Hub-Signature-256 header"));
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
	Ok(hook.post(body).await)
}

async fn run_command(cmd: &hooks::Command, hook: &hooks::Hook, body: &[u8]) -> Result<(), ()> {
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
