use multer::Multipart;
use std::path::Path;
use std::io::Write;
use chrono::Local;

#[tokio::main(flavor = "current_thread")]
async fn main() {
	if let Err(()) = do_main().await {
		std::process::exit(1);
	}
}

async fn do_main() -> Result<(), ()> {
	// Read the CONTENT_TYPE environment variable to get the boundary
	let content_type = std::env::var("CONTENT_TYPE")
		.map_err(|e| eprintln!("Error: Failed to get CONTENT_TYPE environment variable: {e}"))?;

	// Parse the boundary from the content_type, exit early if we don't have it.
	let boundary = multer::parse_boundary(content_type)
		.map_err(|e| eprintln!("Error: Failed to parse multipart boundary from CONTENT_TYPE: {e}"))?;

	// Read the OUTPUT_FOLDER environment variable to store files somewhere
	let output_folder = std::env::var("OUTPUT_FOLDER")
		.map_err(|e| eprintln!("Error: Failed to get OUTPUT_FOLDER environment variable: {e}"))?;
	let output_folder = Path::new(&output_folder);

	// Create the output folder.
	eprintln!("Output folder: {}", output_folder.display());
	std::fs::create_dir_all(output_folder)
		.map_err(|e| eprintln!("Error: Failed to create output folder: {e}"))?;

	// Check the PREFIX_TIMESTAMP environment variable to check if we should prefix uploaded files with a timestamp.
	let prefix_timestamp = std::env::var_os("PREFIX_TIMESTAMP").unwrap_or_default();
	let prefix_timestamp = prefix_timestamp != "0" && prefix_timestamp != "" && prefix_timestamp != "false";
	eprintln!("Add timestamp to file names: {prefix_timestamp}");

	// Remember one timestamp so all files from one upload get the same timestamp.
	let now = Local::now().format("%Y%m%d%H%M%S");

	// Loop over all multipart fields.
	let mut multipart = Multipart::with_reader(tokio::io::stdin(), &boundary);
	while let Some(field) = multipart.next_field().await.transpose() {
		let mut field = field.map_err(|e| eprintln!("Error: Failed to get next multipart field: {e}"))?;

		let Some(field_name) = field.name().map(|x| x.to_owned()) else {
			eprintln!("Found multipart field without name, skipping");
			continue;
		};

		let Some(file_name) = field.file_name().map(|x| x.to_owned()) else {
			let text = field.text().await
				.map_err(|e| eprintln!("Error: Failed to get data for field {field_name}: {e}"))?;
			let text = text.strip_suffix('\n').unwrap_or(&text);
			if text.contains('\n') {
				eprintln!("Field {field_name}:\n{text}\n");
			} else {
				eprintln!("Field {field_name}: {text}");
			}
			continue;
		};

		eprintln!("Field {field_name}: file upload with name {file_name:?}");

		let output_file = match prefix_timestamp {
			true => output_folder.join(format!("{}-{file_name}", now)),
			false => output_folder.join(&file_name),
		};

		let mut file = std::fs::OpenOptions::new()
			.create_new(true)
			.write(true)
			.open(&output_file)
			.map_err(|e| eprintln!("Error: Failed to create {}: {e}", output_file.display()))?;

		while let Some(chunk) = field.chunk().await.transpose() {
			let chunk = chunk.map_err(|e| eprintln!("Error: Failed to read data chunk from stdin: {e}"))?;
			if let Err(e) = file.write_all(&chunk) {
				eprintln!("Error: Failed to write to {}: {e}", output_file.display());
				break;
			}
		}

		eprintln!("Saved file {file_name:?} to {}", output_file.display());
	}

	Ok(())
}
