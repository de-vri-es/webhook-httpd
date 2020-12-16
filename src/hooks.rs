use serde::Deserialize;
use std::path::PathBuf;

use crate::types::QueueType;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "kebab-case")]
pub struct Hook {
	/// The URL for the hook (only the path part).
	pub url: String,

	/// The commands to execute when the hook is triggered.
	pub commands: Vec<Command>,

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

impl MaybeBound {
	/// Get the bound as `Option<usize>`.
	pub fn bound(self) -> Option<usize> {
		match self {
			Self::Unlimited => None,
			Self::N(x) => Some(x),
		}
	}
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

impl<'de> Deserialize<'de> for MaybeBound {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::de::Deserializer<'de>
	{
		struct Visitor;
		impl<'de> serde::de::Visitor<'de> for Visitor {
			type Value = MaybeBound;
			fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
				write!(f, "number or \"unlimited\"")
			}

			fn visit_enum<A: serde::de::EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
				use serde::de::VariantAccess;

				#[derive(Deserialize)]
				#[serde(rename_all = "kebab-case")]
				enum Variant { Unlimited };

				let (_, data): (Variant, _) = data.variant()?;
				data.unit_variant()?;
				Ok(MaybeBound::Unlimited)
			}

			fn visit_u64<E: serde::de::Error>(self, data: u64) -> Result<Self::Value, E> {
				use serde::de::Unexpected;
				use std::convert::TryFrom;
				usize::try_from(data)
					.map_err(|_| E::invalid_value(Unexpected::Unsigned(data), &format!("a number in the range 0-{}", usize::MAX).as_str()))
					.map(MaybeBound::N)
			}
		}

		deserializer.deserialize_enum("MaybeUnbound", &["Unlimited"], Visitor)
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
		D: serde::de::Deserializer<'de>
	{
		use serde::de::Error;

		let args = Vec::<String>::deserialize(deserializer)?;
		Self::from_vec(args)
			.map_err(D::Error::custom)
	}
}

pub fn deserialize(data: impl AsRef<str>) -> Result<Vec<Hook>, serde_yaml::Error> {
	serde_yaml::from_str(data.as_ref())
}

pub fn example_hooks() -> &'static str {
	include_str!("../example-hooks.yaml")
}

#[cfg(test)]
mod test {
	use assert2::assert;
	use super::*;

	#[test]
	fn deserialize_example_hooks() {
		assert!(let Ok(_) = deserialize(example_hooks()))
	}
}
