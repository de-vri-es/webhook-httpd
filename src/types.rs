use serde::Deserialize;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueType {
	Fifo,
	Lifo,
}
