use indicatif::{ProgressBar, ProgressStyle};
use std::io::{self, BufRead};
use std::time::Instant;

fn main() {
    let start_time = Instant::now();
    let stdin = io::stdin();
    let mut total: u64 = 0;
    let mut current: u64 = 0;
    let mut message = String::from("Initializing...");
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg} ETA: {eta_precise}"
        )
        .unwrap()
        .progress_chars("##-")
    );
    pb.set_message(message.clone());

    for line in stdin.lines() {
        let line = match line {
            Ok(l) => l.trim().to_string(),
            Err(_) => break,
        };
        if line.is_empty() {
            continue;
        }
        if line.starts_with("set_total ") {
            if let Ok(t) = line[10..].parse::<u64>() {
                total = t;
                pb.set_length(total);
            }
        } else if line.starts_with("msg ") {
            message = line[4..].to_string();
            pb.set_message(message.clone());
        } else if line == "update" {
            current += 1;
            pb.set_position(current);
        } else if line == "done" {
            pb.finish_with_message(format!("Completed in {:.2}s", start_time.elapsed().as_secs_f64()));
            break;
        }
    }
}
