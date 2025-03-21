use ldk_node::logger::{LogWriter, LogRecord};
use chrono::Utc;
use std::path::Path;
use std::fs;
use std::io::{BufWriter, Write};
use std::sync::Mutex;

pub(crate) struct Logger(Mutex<fs::File>);

impl Logger {
	pub(crate) fn new(path: &Path) -> Result<Logger, ()> {
		Ok(Self(Mutex::new(
			fs::OpenOptions::new().create(true).append(true).open(path).map_err(|_| ())?
		)))
	}
}

impl LogWriter for Logger {
	fn log(&self, record: LogRecord) {
		let mut file = self.0.lock().unwrap();
		let mut buffer = BufWriter::new(&mut *file);
		let _ = write!(&mut buffer,
			"{} {:<5} [{}:{}] {}\n",
			Utc::now().format("%Y-%m-%d %H:%M:%S"),
			record.level.to_string(),
			record.module_path,
			record.line,
			record.args
		);
	}
}
