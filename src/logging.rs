use std::io::Write;

/// Initialize the logging system with a pretty format.
///
/// Logging for the specified root module will be set to Info, Debug or Trace,
/// depending on the verbosity parameter.
///
/// Logging for all other modules is set to [`log::LevelFilter::Warn`].
pub fn init(root_module: &str, level: log::LevelFilter) {
	env_logger::Builder::from_default_env()
		.format(|buffer, record: &log::Record| {
			let now = chrono::Local::now();
			use env_logger::fmt::Color;

			let mut prefix_style = buffer.style();
			let prefix;

			match record.level() {
				log::Level::Trace => {
					prefix = "TRACE";
					prefix_style.set_bold(true);
				},
				log::Level::Debug => {
					prefix = "DEBUG";
					prefix_style.set_bold(true);
				},
				log::Level::Info => {
					prefix = " INFO";
					prefix_style.set_bold(true);
				},
				log::Level::Warn => {
					prefix = " WARN";
					prefix_style.set_color(Color::Yellow).set_bold(true);
				},
				log::Level::Error => {
					prefix = "ERROR";
					prefix_style.set_color(Color::Red).set_bold(true);
				},
			};

			writeln!(buffer, "{time} {prefix}: {msg}",
				time = now.format("%F %H:%M:%S"),
				prefix = prefix_style.value(prefix),
				msg = record.args()
			)
		})
		.filter_module(root_module, level)
		.init();
}
