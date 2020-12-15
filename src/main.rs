use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use structopt::StructOpt;
use structopt::clap::AppSettings;

mod hooks;
mod logging;

#[derive(StructOpt)]
#[structopt(setting = AppSettings::ColoredHelp)]
#[structopt(setting = AppSettings::DeriveDisplayOrder)]
#[structopt(setting = AppSettings::UnifiedHelpMessage)]
struct Options {
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
	logging::init(module_path!(), 0);
	if let Err(()) = do_main(Options::from_args()) {
		std::process::exit(1);
	}
}

fn do_main(options: Options) -> Result<(), ()> {
	if options.print_example_hooks {
		println!("{}", hooks::to_toml(&hooks::example_hooks()));
		return Ok(());
	}

	let mut runtime = tokio::runtime::Builder::new()
		.basic_scheduler()
		.enable_all()
		.build()
		.map_err(|e| log::error!("failed to create tokio runtime: {}", e))?;

	runtime.block_on(async move {
		let hooks_path = options.hooks.as_ref().unwrap(); // Can't fail, hooks is a required option.
		let hooks = std::fs::read_to_string(&hooks_path)
			.map_err(|e| log::error!("failed to read {}: {}", hooks_path.display(), e))?;
		let hooks = hooks::from_toml(&hooks)
			.map_err(|e| log::error!("failed to parse hooks from {}: {}", hooks_path.display(), e))?;
		let hooks = Arc::new(hooks);

		let handle_connection = hyper::service::make_service_fn(move |_| {
			let hooks = hooks.clone();
			async move {
				hyper::Result::Ok(hyper::service::service_fn(move |request| {
					let hooks = hooks.clone();
					async move {
						hyper::Result::Ok(handle_request(hooks.as_slice(), request).await)
					}
				}))
			}
		});

		let socket_address = options.socket_address();
		let server = hyper::Server::try_bind(&socket_address)
			.map_err(|e| log::error!("failed to bind to {}: {}", socket_address, e))?;
		log::info!("Listening on {}", socket_address);
		server.serve(handle_connection)
			.await
			.map_err(|e| log::error!("{}", e))?;
		Ok(())
	})
}

async fn handle_request(hooks: &[hooks::Hook], mut request: hyper::Request<hyper::Body>) -> hyper::Response<hyper::Body> {
	log::info!("received {} request for {}", request.method(), request.uri().path());
	if request.method() != hyper::Method::POST {
		return simple_response(hyper::StatusCode::METHOD_NOT_ALLOWED, "hooks must use the POST method");
	}

	// Find the hook for the request URL.
	let hook = match hooks.iter().find(|hook| hook.url == request.uri().path()) {
		Some(x) => x,
		None => {
			log::info!("no hook found for URL: {}", request.uri().path());
			return simple_response(hyper::StatusCode::NOT_FOUND, "no matching hook found");
		}
	};

	// Collect the body so we can feed it to a command and to check the signature.
	let body = match collect_body(request.body_mut()).await {
		Ok(x) => x,
		Err(e) => {
			log::error!("failed to read request body: {}", e);
			return simple_response(hyper::StatusCode::BAD_REQUEST, "failed to read request body");
		},
	};

	if let Some(secret) = &hook.secret {
		let signature = match request.headers().get("X-Hub-Signature-256") {
			Some(x) => x,
			None => {
				log::error!("request is missing a X-Hub-Signature-256 header");
				return simple_response(hyper::StatusCode::BAD_REQUEST, "request must be signed with X-Hub-Signature-256 header");
			},
		};
		let signature = match signature.to_str() {
			Ok(x) => x,
			Err(e) => {
				log::error!("request has a malformed X-Hub-Signature-256 header: {}", e);
				return simple_response(hyper::StatusCode::BAD_REQUEST, "invalid X-Hub-Signature-256 header");
			},
		};
		let digest = compute_digest(secret, &body);
		if !digest.eq_ignore_ascii_case(signature) {
			log::error!("request signature ({}) does not match the payload digest ({})", signature, digest);
			return simple_response(hyper::StatusCode::BAD_REQUEST, "invalid X-Hub-Signature-256 header");
		}
	}

	log::info!("executing hook for URL: {}", hook.url);
	for cmd in &hook.commands {
		if let Err(()) = run_command(cmd, hook.working_dir.as_deref()).await {
			return simple_response(hyper::StatusCode::INTERNAL_SERVER_ERROR, "failed to run hook, see server logs for more details");
		}
	}

	simple_response(hyper::StatusCode::OK, "")
}

async fn run_command(cmd: &hooks::Command, working_dir: Option<&Path>) -> Result<(), ()> {
	use tokio::io::AsyncBufReadExt;
	use tokio::stream::StreamExt;

	let mut command = tokio::process::Command::new(cmd.command());
	command.args(cmd.arguments());
	command.stdout(std::process::Stdio::piped());
	command.stderr(std::process::Stdio::piped());
	if let Some(working_dir) = working_dir {
		command.current_dir(working_dir);
	}
	let mut subprocess = command.spawn()
		.map_err(|e| log::error!("failed to run command {:?}: {}", cmd.command(), e))?;

	let stdout = tokio::io::BufReader::new(subprocess.stdout.take().unwrap());
	let stderr = tokio::io::BufReader::new(subprocess.stderr.take().unwrap());
	let mut output = stdout.lines().merge(stderr.lines());

	while let Some(line) = output.next().await {
		let line = match line {
			Ok(x) => x,
			Err(e) => {
				log::error!("failed to read command {:?} output: {}", cmd.command(), e);
				break;
			},
		};
		log::info!("{}: {}", cmd.command(), line);
	}

	let status = subprocess.await.map_err(|e| log::error!("failed to wait for command {:?}: {}", cmd.command(), e))?;
	if status.success() {
		Ok(())
	} else {
		log::error!("command {:?} exitted with {}", cmd.command(), status);
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

async fn collect_body(body: &mut hyper::Body) -> Result<Vec<u8>, hyper::Error> {
	use hyper::body::HttpBody;

	let mut data = Vec::new();
	while let Some(chunk) = body.data().await {
		data.extend_from_slice(chunk?.as_ref());
	}
	Ok(data)
}

fn simple_response(status: hyper::StatusCode, data: impl Into<Vec<u8>>) -> hyper::Response<hyper::Body> {
	let mut data: Vec<u8> = data.into();
	if data.last() != Some(&b'\n') {
		data.push(b'\n');
	}
	hyper::Response::builder()
		.status(status)
		.body(data.into())
		.unwrap()
}
