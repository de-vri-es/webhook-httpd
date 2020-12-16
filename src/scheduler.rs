use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::{mpsc, oneshot, watch};

use crate::types::QueueType;

pub type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

#[derive(Debug, Clone)]
pub struct Scheduler {
	command_tx: mpsc::UnboundedSender<Command>,
	stopped_rx: watch::Receiver<bool>,
}

#[derive(Debug, Clone)]
pub struct Error {
	msg: String,
}

impl Error {
	fn new(msg: impl std::string::ToString) -> Self {
		Error { msg: msg.to_string() }
	}
}

impl std::fmt::Display for Error {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.msg.fmt(f)
	}
}

impl Scheduler {
	pub fn new(max_concurrent: Option<usize>, queue_size: Option<usize>, queue_type: QueueType) -> Self {
		let (stopped_tx, stopped_rx) = watch::channel(false);
		let inner = SchedulerInner::new(max_concurrent, queue_size, queue_type, stopped_tx);
		let command_tx = inner.command_tx.clone();
		tokio::spawn(inner.run());
		Self { command_tx, stopped_rx }
	}

	pub async fn post(&self, job: Job) -> Result<(), Error> {
		let (result_tx, result_rx) = oneshot::channel();
		self.command_tx
			.send(Command::NewJob(job, result_tx))
			.map_err(|_| Error::new("scheduler stopped"))?;
		result_rx.await.map_err(|_| Error::new("scheduler stopped"))?
	}

	pub async fn jobs_running(&self) -> usize {
		let (result_tx, result_rx) = oneshot::channel();
		self.command_tx.send(Command::GetRunningJobs(result_tx)).unwrap_or_else(|_| ());
		result_rx.await.unwrap_or(0)
	}

	pub async fn stop(&mut self, finish_queue: bool) {
		self.command_tx.send(Command::Stop(finish_queue)).unwrap_or_else(|_| ());
		while self.stopped_rx.recv().await != Some(true) {
			// Keep going...
		}
	}
}

struct SchedulerInner {
	max_concurrent: Option<usize>,
	queue_size: Option<usize>,
	queue_type: QueueType,

	running: usize,
	queue: VecDeque<Job>,
	accept_jobs: bool,
	process_queue: bool,

	stopped_tx: watch::Sender<bool>,
	command_tx: mpsc::UnboundedSender<Command>,
	command_rx: mpsc::UnboundedReceiver<Command>,
}

impl SchedulerInner {
	fn new(max_concurrent: Option<usize>, queue_size: Option<usize>, queue_type: QueueType, stopped_tx: watch::Sender<bool>) -> Self {
		let (command_tx, command_rx) = mpsc::unbounded_channel();
		Self {
			max_concurrent,
			queue_size,
			queue_type,
			running: 0,
			queue: VecDeque::new(),
			accept_jobs: true,
			process_queue: true,
			stopped_tx,
			command_tx,
			command_rx,
		}
	}

	async fn run(mut self) {
		while let Some(command) = self.command_rx.recv().await {
			match command {
				Command::NewJob(job, result_tx) => self.handle_new_job(job, result_tx),
				Command::JobFinished => self.handle_job_finished(),
				Command::GetRunningJobs(result_tx) => {
					let _: Result<_, _> = result_tx.send(self.running);
				},
				Command::Stop(finish_queue) => {
					self.accept_jobs = false;
					self.process_queue = finish_queue;
				},
			}
			if !self.accept_jobs && self.running == 0 {
				break;
			}
		}
		self.stopped_tx.broadcast(true).unwrap_or(()) // Nobody cares that we stopped? That's alright, we don't care either.
	}

	fn handle_new_job(&mut self, job: Job, result_tx: oneshot::Sender<Result<(), Error>>) {
		if !self.accept_jobs {
			result_tx.send(Err(Error::new("scheduler is not accepting new jobs"))).unwrap_or(());
		} else if !limit_reached(self.running, self.max_concurrent) {
			eprintln!("spawning job directly");
			// There should never be something in the queue as long as there are free job slots.
			debug_assert!(self.queue.is_empty());
			self.running += 1;
			self.spawn_job(job);
			result_tx.send(Ok(())).unwrap_or(())
		} else {
			eprintln!("queueing job");
			self.enqueue_job(job);
			result_tx.send(Ok(())).unwrap_or(())
		}
	}

	fn handle_job_finished(&mut self) {
		if !self.process_queue {
			self.running -= 1;
		} else if let Some(job) = self.unqueue_job() {
			self.spawn_job(job);
		} else {
			self.running -= 1;
		}
	}

	fn spawn_job(&mut self, job: Job) {
		let command_tx = self.command_tx.clone();
		tokio::spawn(async move {
			job.await;
			command_tx.send(Command::JobFinished).unwrap_or(());
		});
	}

	fn enqueue_job(&mut self, job: Job) {
		if limit_reached(self.queue.len(), self.queue_size) {
			if self.queue_type == QueueType::Fifo {
				// Drop job. We prefer keeping older jobs.
			} else {
				// Pop and drop job from the back of the queue.
				self.queue.pop_back();
				self.queue.push_front(job);
			}
		} else {
			self.queue.push_back(job)
		}
	}

	fn unqueue_job(&mut self) -> Option<Job> {
		match self.queue_type {
			QueueType::Fifo => self.queue.pop_front(),
			QueueType::Lifo => self.queue.pop_back(),
		}
	}
}

fn limit_reached(value: usize, bound: Option<usize>) -> bool {
	match bound {
		None => false,
		Some(n) => value >= n,
	}
}

enum Command {
	NewJob(Job, oneshot::Sender<Result<(), Error>>),
	JobFinished,
	GetRunningJobs(oneshot::Sender<usize>),
	Stop(bool),
}

#[cfg(test)]
mod test {
	use super::*;
	use assert2::{assert, let_assert};
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::sync::Arc;

	fn runtime() -> tokio::runtime::Runtime {
		let_assert!(Ok(runtime) = tokio::runtime::Builder::new().basic_scheduler().enable_all().build());
		runtime
	}

	#[test]
	fn unlimited_concurrency() {
		runtime().block_on(async {
			let mut scheduler = Scheduler::new(None, None, QueueType::Fifo);
			let mut senders = Vec::new();
			let completed = Arc::new(AtomicUsize::new(0));

			for i in 0..100 {
				let (tx, rx) = oneshot::channel();
				senders.push(tx);
				let completed = completed.clone();
				let job = Box::pin(async move {
					// Wait for sender.
					assert!(let Ok(()) = rx.await);
					completed.fetch_add(1, Ordering::Relaxed);
				});
				assert!(let Ok(()) = scheduler.post(job).await);
				assert!(scheduler.jobs_running().await == i + 1);
			}

			assert!(completed.load(Ordering::Relaxed) == 0);
			for tx in senders {
				assert!(let Ok(()) = tx.send(()));
			}

			scheduler.stop(false).await;
			assert!(completed.load(Ordering::Relaxed) == 100);
		});
	}

	#[test]
	fn limited_concurrency_abort_queue() {
		runtime().block_on(async {
			let mut scheduler = Scheduler::new(Some(4), Some(8), QueueType::Fifo);
			let mut senders = Vec::new();
			let completed = Arc::new(AtomicUsize::new(0));

			for i in 0..100 {
				let (tx, rx) = oneshot::channel();
				senders.push(tx);
				let completed = completed.clone();
				let job = Box::pin(async move {
					// Wait for sender.
					assert!(let Ok(()) = rx.await);
					completed.fetch_add(1, Ordering::Relaxed);
				});
				assert!(let Ok(()) = scheduler.post(job).await);
				assert!(scheduler.jobs_running().await == (i + 1).min(4));
			}

			assert!(completed.load(Ordering::Relaxed) == 0);
			for (i, tx) in senders.into_iter().enumerate() {
				if i < 12 {
					assert!(let Ok(()) = tx.send(()));
				} else {
					assert!(let Err(()) = tx.send(()));
				}
			}

			scheduler.stop(false).await;
			assert!(completed.load(Ordering::Relaxed) == 4);
		});
	}

	#[test]
	fn limited_concurrency_finish_queue() {
		runtime().block_on(async {
			let mut scheduler = Scheduler::new(Some(4), Some(8), QueueType::Fifo);
			let mut senders = Vec::new();
			let completed = Arc::new(AtomicUsize::new(0));

			for i in 0..100 {
				let (tx, rx) = oneshot::channel();
				senders.push(tx);
				let completed = completed.clone();
				let job = Box::pin(async move {
					// Wait for sender.
					assert!(let Ok(()) = rx.await);
					completed.fetch_add(1, Ordering::Relaxed);
				});
				assert!(let Ok(()) = scheduler.post(job).await);
				assert!(scheduler.jobs_running().await == (i + 1).min(4));
			}

			assert!(completed.load(Ordering::Relaxed) == 0);
			for (i, tx) in senders.into_iter().enumerate() {
				if i < 12 {
					assert!(let Ok(()) = tx.send(()));
				} else {
					assert!(let Err(()) = tx.send(()));
				}
			}

			scheduler.stop(true).await;
			assert!(completed.load(Ordering::Relaxed) == 12);
		});
	}
}
