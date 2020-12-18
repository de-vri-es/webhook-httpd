use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::{mpsc, oneshot, watch};

use crate::types::QueueType;

pub type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

#[derive(Debug, Clone)]
pub struct Scheduler {
	command_tx: mpsc::UnboundedSender<Command>,
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
	pub fn new(max_concurrent: Option<usize>, queue_size: Option<usize>, queue_type: QueueType, stop_rx: watch::Receiver<bool>) -> Self {
		let inner = SchedulerInner::new(max_concurrent, queue_size, queue_type, stop_rx);
		let command_tx = inner.command_tx.clone();
		tokio::spawn(inner.run());
		Self { command_tx }
	}

	pub async fn post(&self, job: Job) -> Result<(), Error> {
		let (result_tx, result_rx) = oneshot::channel();
		self.command_tx
			.send(Command::NewJob(job, result_tx))
			.map_err(|_| Error::new("scheduler stopped"))?;
		result_rx.await.map_err(|_| Error::new("scheduler stopped"))?
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

	stop_rx: watch::Receiver<bool>,
	command_tx: mpsc::UnboundedSender<Command>,
	command_rx: mpsc::UnboundedReceiver<Command>,
}

impl SchedulerInner {
	fn new(max_concurrent: Option<usize>, queue_size: Option<usize>, queue_type: QueueType, stop_rx: watch::Receiver<bool>) -> Self {
		let (command_tx, command_rx) = mpsc::unbounded_channel();
		Self {
			max_concurrent,
			queue_size,
			queue_type,
			running: 0,
			queue: VecDeque::new(),
			accept_jobs: true,
			process_queue: true,
			stop_rx,
			command_tx,
			command_rx,
		}
	}

	async fn run(mut self) {
		while self.accept_jobs || self.running > 0 {
			tokio::select!(
				stop = self.stop_rx.recv() => {
					if stop.unwrap_or(true) {
						self.accept_jobs = false;
						self.process_queue = false;
					}
				},
				command = self.command_rx.recv() => {
					match command.unwrap() {
						Command::NewJob(job, result_tx) => self.handle_new_job(job, result_tx),
						Command::JobFinished => self.handle_job_finished(),
					}
				},
			)
		}

		// Dropping `self.stop_rx` allows the sender to detect that we stopped.
		// This happens anyway, but let's be explicit for documentation purposes.
		drop(self.stop_rx);
	}

	fn handle_new_job(&mut self, job: Job, result_tx: oneshot::Sender<Result<(), Error>>) {
		if !self.accept_jobs {
			result_tx.send(Err(Error::new("scheduler is not accepting new jobs"))).unwrap_or(());
		} else if !limit_reached(self.running, self.max_concurrent) {
			// There should never be something in the queue as long as there are free job slots.
			debug_assert!(self.queue.is_empty());
			self.running += 1;
			self.spawn_job(job);
			result_tx.send(Ok(())).unwrap_or(())
		} else {
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
}

#[cfg(test)]
mod test {
	use super::*;
	use assert2::{assert, let_assert};
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::sync::Arc;
	use tokio::sync::Semaphore;

	fn runtime() -> tokio::runtime::Runtime {
		let_assert!(Ok(runtime) = tokio::runtime::Builder::new().basic_scheduler().enable_all().build());
		runtime
	}

	#[test]
	fn unlimited_concurrency() {
		runtime().block_on(async {
			let (mut stop_tx, stop_rx) = watch::channel(false);
			let scheduler = Scheduler::new(None, None, QueueType::Fifo, stop_rx);
			let started = Arc::new(AtomicUsize::new(0));
			let completed = Arc::new(AtomicUsize::new(0));
			let notify = Arc::new(Semaphore::new(0));

			for _ in 0..100 {
				let job = Box::pin({
					let started = started.clone();
					let completed = completed.clone();
					let notify = notify.clone();
					async move {
						started.fetch_add(1, Ordering::Relaxed);
						notify.acquire().await.forget();
						completed.fetch_add(1, Ordering::Relaxed);
					}
				});

				assert!(let Ok(()) = scheduler.post(job).await);
			}

			tokio::task::yield_now().await;
			assert!(started.load(Ordering::Relaxed) == 100);
			assert!(completed.load(Ordering::Relaxed) == 0);

			notify.add_permits(100);
			stop_tx.broadcast(true).unwrap_or(());
			stop_tx.closed().await;
			assert!(completed.load(Ordering::Relaxed) == 100);
		});
	}

	#[test]
	fn limited_concurrency_abort_queue() {
		runtime().block_on(async {
			let (mut stop_tx, stop_rx) = watch::channel(false);
			let scheduler = Scheduler::new(Some(4), Some(8), QueueType::Fifo, stop_rx);

			let started = Arc::new(AtomicUsize::new(0));
			let notify = Arc::new(Semaphore::new(0));
			for _ in 0..100 {
				let job = Box::pin({
					let started = started.clone();
					let notify = notify.clone();
					async move {
						started.fetch_add(1, Ordering::Relaxed);
						notify.acquire().await.forget();
					}
				});
				assert!(let Ok(()) = scheduler.post(job).await);
			}

			tokio::task::yield_now().await;
			assert!(started.load(Ordering::Relaxed) == 4);

			notify.add_permits(100);
			stop_tx.broadcast(true).unwrap_or(());
			stop_tx.closed().await;
		});
	}
}
