use serde::Deserialize;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Deserialize)]
pub enum QueueType {
	Fifo,
	Lifo,
}
