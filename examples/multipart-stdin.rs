use tokio::io;
use multer::Multipart;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::io::Write;
use std::fs;
use chrono::Local;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Read the CONTENT_TYPE environment variable to get the boundary
    let content_type = match std::env::var("CONTENT_TYPE") {
        Ok(content_type) => content_type,
        Err(_) => {
            eprintln!("Error: CONTENT_TYPE environment variable not found.");
            return Ok(());
        }
    };

    // Parse the boundary from the content_type, exit early if we don't have it.
    let boundary = match content_type.split(';').find(|s| s.trim().starts_with("boundary=")) {
        Some(boundary) => boundary.trim().trim_start_matches("boundary=").to_string(),
        None => {
            eprintln!("Error: Boundary not found in CONTENT_TYPE.");
            return Ok(());
        }
    };

    // Read the OUTPUT_FOLDER environment variable to store files somewhere
    let output_folder = std::env::var("OUTPUT_FOLDER").unwrap_or_default();
	if !output_folder.is_empty() {
	    println!("writing to: {}",output_folder);
		if !Path::new(&output_folder).is_dir() {
        	if let Err(err) = fs::create_dir_all(&output_folder) {
            	eprintln!("Error creating directory: {}", err);
        	} else {
            	println!("Directory '{}' created successfully.", output_folder);
        	}
	    }
    } else {
        println!("No output folder specified.");
    }

    let mut multipart = Multipart::with_reader(io::stdin(), &boundary);
    while let Some(mut field) = multipart.next_field().await? {
        let field_name = field.name().unwrap_or_default().to_string();
        let has_filename= field.file_name().map(|s| s.to_string());

        // Check if the field has a file name
        if let Some(file_name) = has_filename {
            println!("Writing a file: {}", &file_name);

			let apply_timestamp = std::env::var("APPLY_TIMESTAMP").is_ok();
			let timestamp = if apply_timestamp {
                format!("_{}", Local::now().format("%Y%m%d%H%M"))
            } else {
                "".to_string()
            };
            let file_extension = Path::new(&file_name)
                .extension()
                .map(|ext| ext.to_str().unwrap_or(""))
                .unwrap_or("");
			let file_name_noext = extract_filename_without_ext(&file_name);
            let mut unique_name = format!("{}{}.{}", file_name_noext, timestamp, file_extension);
            let mut unique_file_name = Path::new(&output_folder).join(unique_name);

			// Loop until a unique filename is found
			let count_suffix = std::env::var("COUNT_SUFFIX").is_ok();
			if count_suffix {
				let mut counter = 0;
				while unique_file_name.exists() {
                	unique_name = format!("{}{}.{}.{}", file_name_noext, timestamp, counter, file_extension);
                	unique_file_name = PathBuf::from(&output_folder).join(&unique_name);
                	counter += 1;
            	}
			}

            if let Ok(mut file) = File::create(&unique_file_name) {
                while let Some(chunk) = field.chunk().await? {
                    if let Err(e) = file.write_all(&chunk) {
                        eprintln!("Error writing to file: {}", e);
                        break;
                    }
                }
                println!("File '{}' uploaded and saved as '{:?}'", &file_name, &unique_file_name);
            } else {
                eprintln!("Error creating file: {}", &file_name);
            }
        } else {
            while let Some(chunk) = field.chunk().await? {
                println!("Field '{}' = {}", field_name, String::from_utf8_lossy(&chunk));
            }
        }
    }

    Ok(())
}

fn extract_filename_without_ext(file_name: &str) -> String {
    let file_path = Path::new(file_name);

    if let Some(file_stem) = file_path.file_stem() {
        if let Some(file_stem_str) = file_stem.to_str() {
            return file_stem_str.to_string();
        }
    }

    String::new()
}

