use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "PascalCase")]
pub struct Hook {
	/// The URL for the hook (only the path part).
	#[serde(rename = "URL")]
	pub url: String,

	/// The commands to execute when the hook is triggered.
	pub commands: Vec<Command>,

	/// The working directory for the commands.
	pub working_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Command {
	args: Vec<String>,
}

impl Command {
	fn from_vec(args: Vec<String>) -> Result<Self, &'static str> {
		if args.is_empty() {
			Err("command arguments can not be empty")
		} else {
			Ok(Command { args })
		}
	}

	pub fn command(&self) -> &str {
		&self.args[0]
	}

	pub fn arguments(&self) -> &[String] {
		&self.args[1..]
	}
}

impl<'de> Deserialize<'de> for Command {
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

impl Serialize for Command {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::ser::Serializer
	{
		self.args.serialize(serializer)
	}
}

pub fn from_toml(bytes: impl AsRef<[u8]>) -> Result<Vec<Hook>, toml::de::Error> {
	#[derive(Debug, Clone, Deserialize)]
	#[serde(deny_unknown_fields)]
	struct Config {
		#[serde(rename = "Hook")]
		hooks: Vec<Hook>,
	}

	let parsed: Config = toml::from_slice(bytes.as_ref())?;
	Ok(parsed.hooks)
}

pub fn to_toml(hooks: &[Hook]) -> String {
	#[derive(Debug, Clone, Serialize)]
	struct Config<'a> {
		#[serde(rename = "Hook")]
		hooks: &'a [Hook],
	}

	// Data is always valid to serialize as TOML, so unwrap can't fail.
	toml::to_string(&Config { hooks }).unwrap()
}

pub fn example_hooks() -> Vec<Hook> {
	vec![
		Hook {
			url: "/echo-foo".to_string(),
			commands: vec![Command::from_vec(vec!["echo".to_string(), "foo".to_string()]).unwrap()],
			working_dir: None,
		},
		Hook {
			url: String::from("/update-repository"),
			commands: vec![
				Command::from_vec(vec!["git".to_string(), "fetch".to_string()]).unwrap(),
				Command::from_vec(vec!["git".to_string(), "reset".to_string(), "--hard".to_string(), "origin/main".to_string()]).unwrap(),
			],
			working_dir: Some(PathBuf::from("/my/git/repo")),
		},
	]
}
