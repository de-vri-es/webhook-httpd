use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::net::IpAddr;
use indexmap::IndexMap;

use crate::logging::LogLevel;
use crate::types::QueueType;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
	/// Bind the HTTP server to a specific address.
	pub bind_address: Option<IpAddr>,

	/// Bind the server to this port.
	#[serde(default = "default_port")]
	pub port: u16,

	/// TLS settings.
	pub tls: Option<Tls>,

	/// Log messages with this log leven and up.
	#[serde(default = "default_log_level")]
	pub log_level: LogLevel,

	/// The hooks to execute when a matching request is received.
	pub hooks: Vec<Hook>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "kebab-case")]
pub struct Tls {
	/// The path to the private key encoded as PEM file.
	pub private_key: PathBuf,

	/// Path to the chain of certificates.
	///
	/// The first certificate in the chain must be the server certificate.
	pub certificate_chain: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "kebab-case")]
pub struct Hook {
	/// The URL for the hook (only the path part).
	pub url: String,

	/// The commands to execute when the hook is triggered.
	pub commands: Vec<Command>,

	/// The environment variables to set when the hook is triggered.
	#[serde(default)]
	pub environment: IndexMap<String, String>,

	/// The maxmimum number of concurrent jobs for the hook.
	///
	/// Can be a number or `unlimited`.
	#[serde(default = "default_max_concurrent")]
	pub max_concurrent: MaybeBound,

	/// Queue jobs that were suppresed because of the concurrency limit.
	///
	/// Queued jobs will be executed when a previous job for the hook finishes.
	#[serde(default = "default_queue_size")]
	pub queue_size: MaybeBound,

	/// The type of job queue: LIFO or FIFO.
	///
	/// With a FIFO queue, the older job in the queue is chosen for execution.
	/// With a LIFO queue, the newest job in the queue is executed first.
	///
	/// This also affects which jobs are dropped when the queue size reaches the maximum.
	#[serde(default = "default_queue_type")]
	pub queue_type: QueueType,

	/// The working directory for the commands.
	pub working_dir: Option<PathBuf>,

	/// The shared secret for the X-Hub-Signature-256 header.
	pub secret: Option<String>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum MaybeBound {
	N(usize),
	Unlimited,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Command {
	cmd: CommandArgs,

	#[serde(default = "default_stdin")]
	stdin: Stdin,
	#[serde(default = "default_has_response")]
	has_response: bool,
}

#[derive(Debug, Clone)]
pub struct CommandArgs {
	args: Vec<String>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "kebab-case")]
pub enum Stdin {
	Nothing,
	RequestBody,
}

impl Config {
	pub fn socket_address(&self) -> std::net::SocketAddr {
		let address = self.bind_address.unwrap_or(std::net::Ipv6Addr::UNSPECIFIED.into());
		std::net::SocketAddr::new(address, self.port)
	}

	pub fn parse(data: impl AsRef<[u8]>) -> Result<Self, serde_yaml::Error> {
		serde_yaml::from_slice(data.as_ref())
	}

	pub fn read_from_file(path: impl AsRef<Path>) -> Result<Self, String> {
		let path = path.as_ref();
		let data = std::fs::read(path)
			.map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
		Self::parse(data)
			.map_err(|e| format!("failed to parse {}: {}", path.display(), e))
	}

	pub fn example() -> &'static str {
		include_str!("../example-config.yaml")
	}
}

impl MaybeBound {
	/// Get the bound as `Option<usize>`.
	pub fn bound(self) -> Option<usize> {
		match self {
			Self::Unlimited => None,
			Self::N(x) => Some(x),
		}
	}
}

fn default_port() -> u16 {
	8091
}

fn default_log_level() -> LogLevel {
	LogLevel::Info
}

fn default_max_concurrent() -> MaybeBound {
	MaybeBound::N(1)
}

fn default_queue_size() -> MaybeBound {
	MaybeBound::N(1)
}

fn default_queue_type() -> QueueType {
	QueueType::Lifo
}

fn default_stdin() -> Stdin {
	Stdin::Nothing
}

fn default_has_response() -> bool {
	false
}

impl<'de> Deserialize<'de> for MaybeBound {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::de::Deserializer<'de>,
	{
		struct Visitor;
		impl<'de> serde::de::Visitor<'de> for Visitor {
			type Value = MaybeBound;

			fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
				write!(f, "number or \"unlimited\"")
			}

			fn visit_str<E: serde::de::Error>(self, data: &str) -> Result<Self::Value, E> {
				use serde::de::Unexpected;
				if data == "unlimited" {
					Ok(MaybeBound::Unlimited)
				} else {
					Err(E::invalid_value(Unexpected::Str(data), &"number or \"unlimited\""))
				}
			}

			fn visit_u64<E: serde::de::Error>(self, data: u64) -> Result<Self::Value, E> {
				use serde::de::Unexpected;
				use std::convert::TryFrom;
				usize::try_from(data)
					.map_err(|_| E::invalid_value(Unexpected::Unsigned(data), &format!("a number in the range 0-{} or \"unlimited\"", usize::MAX).as_str()))
					.map(MaybeBound::N)
			}
		}

		deserializer.deserialize_any(Visitor)
	}
}

impl Command {
	pub fn cmd(&self) -> &str {
		&self.cmd.args[0]
	}

	pub fn args(&self) -> &[String] {
		&self.cmd.args[1..]
	}

	pub fn wants_request_body(&self) -> bool {
		self.stdin == Stdin::RequestBody
	}

	pub fn has_response(&self) -> bool {
		self.has_response == true
	}
}

impl CommandArgs {
	fn from_vec(args: Vec<String>) -> Result<Self, &'static str> {
		if args.is_empty() {
			Err("command arguments can not be empty")
		} else {
			Ok(Self { args })
		}
	}
}

impl<'de> Deserialize<'de> for CommandArgs {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::de::Deserializer<'de>,
	{
		use serde::de::Error;

		let args = Vec::<String>::deserialize(deserializer)?;
		Self::from_vec(args).map_err(D::Error::custom)
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use assert2::assert;

	#[test]
	fn deserialize_example_hooks() {
		assert!(let Ok(_) = Config::parse(Config::example()))
	}
}
