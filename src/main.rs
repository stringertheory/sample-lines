use clap::{Arg, Command};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::fs::File;
use std::io::ErrorKind;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::process;
use std::str::FromStr;

/// Configuration for the sampling program
struct Config {
    sample_size: Option<usize>,
    rate: Option<f64>,
    seed: Option<u64>,
    filename: Option<String>,
    preserve_headers: Option<usize>,
}

/// Iterate over lines as raw bytes, so input doesn't have to be valid UTF-8.
/// Only the trailing `\n` is removed, which keeps any `\r` from CRLF input.
fn byte_lines<R: BufRead>(mut reader: R) -> impl Iterator<Item = io::Result<Vec<u8>>> {
    std::iter::from_fn(move || {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => None,
            Ok(_) => {
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                Some(Ok(line))
            }
            Err(e) => Some(Err(e)),
        }
    })
}

/// Perform reservoir sampling on lines from an iterator
fn reservoir_sample<T, I>(lines: I, k: usize, mut rng: StdRng) -> io::Result<Vec<T>>
where
    I: Iterator<Item = io::Result<T>>,
{
    let mut reservoir: Vec<T> = Vec::with_capacity(k);

    for (total, line_result) in lines.enumerate() {
        let line = line_result?;
        if total < k {
            reservoir.push(line);
        } else {
            let j = rng.gen_range(0..=total);
            if j < k {
                reservoir[j] = line;
            }
        }
    }

    Ok(reservoir)
}

/// Perform probability-based sampling on lines from an iterator
fn probability_sample<T, I>(lines: I, p: f64, mut rng: StdRng) -> io::Result<Vec<T>>
where
    I: Iterator<Item = io::Result<T>>,
{
    let mut sampled = Vec::new();
    for line_result in lines {
        let line = line_result?;
        if rng.gen::<f64>() < p {
            sampled.push(line);
        }
    }
    Ok(sampled)
}

/// Write one line, exiting cleanly if the downstream consumer has gone away
fn write_line(handle: &mut impl Write, line: &[u8]) -> io::Result<()> {
    exit_on_broken_pipe(handle.write_all(line).and_then(|_| handle.write_all(b"\n")))
}

fn exit_on_broken_pipe(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == ErrorKind::BrokenPipe => process::exit(0),
        result => result,
    }
}

/// Write sampled lines to stdout
fn write_results(lines: Vec<Vec<u8>>) -> io::Result<()> {
    let mut handle = BufWriter::new(io::stdout().lock());

    for line in lines {
        write_line(&mut handle, &line)?;
    }

    // Dropping a BufWriter ignores write errors, so flush explicitly.
    exit_on_broken_pipe(handle.flush())
}

/// Parse command line arguments using clap
fn parse_args() -> Config {
    let matches = Command::new("samp")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Randomly sample lines from a file or stdin")
        .arg(
            Arg::new("sample_size")
                .short('n')
                .long("number")
                .value_name("NUM")
                .help("Number of lines to sample (mutually exclusive with -r)")
                .conflicts_with("rate")
                .value_parser(clap::value_parser!(usize)),
        )
        .arg(
            Arg::new("rate")
                .short('r')
                .long("rate")
                .value_name("RATE")
                .help("Probability of keeping a line (e.g., 0.05 means 5%)")
                .conflicts_with("sample_size")
                .value_parser(|s: &str| {
                    let value = f64::from_str(s).map_err(|_| String::from("Must be a float"))?;
                    if (0.0..=1.0).contains(&value) {
                        Ok(value)
                    } else {
                        Err(String::from("Rate must be between 0.0 and 1.0"))
                    }
                }),
        )
        .arg(
            Arg::new("seed")
                .short('s')
                .long("seed")
                .value_name("SEED")
                .help("Optional seed for reproducible sampling")
                .value_parser(clap::value_parser!(u64)),
        )
        .arg(
            Arg::new("preserve_headers")
                .short('p')
                .long("preserve-headers")
                .value_name("NUM")
                .help("Number of header lines to preserve")
                .value_parser(clap::value_parser!(usize)),
        )
        .arg(
            Arg::new("file")
                .value_name("FILE")
                .help("Input file (reads from stdin if not provided)")
                .index(1),
        )
        .after_help(
            "Example usage:
    cat data.txt | samp -n 10
    samp -r 0.05 < big.log
    (head -n 1 && tail -n +2 data.csv | samp -n 10) < data.csv
    samp -n 10 --preserve-headers 1 < data.csv # same as above",
        )
        .get_matches();

    Config {
        sample_size: matches.get_one::<usize>("sample_size").copied(),
        rate: matches.get_one::<f64>("rate").copied(),
        seed: matches.get_one::<u64>("seed").copied(),
        filename: matches.get_one::<String>("file").cloned(),
        preserve_headers: matches.get_one::<usize>("preserve_headers").copied(),
    }
}

fn main() -> io::Result<()> {
    let config = parse_args();

    if config.sample_size.is_none() && config.rate.is_none() {
        eprintln!("Error: Must specify either -n <NUM> or -r <RATE>");
        process::exit(1);
    }

    // Set up the input source
    let reader: Box<dyn BufRead> = match &config.filename {
        Some(file) => {
            let f = match File::open(file) {
                Ok(file) => file,
                Err(e) => {
                    eprintln!("Error: cannot open input file: {}", e);
                    process::exit(1);
                }
            };
            Box::new(BufReader::new(f))
        }
        None => Box::new(BufReader::new(io::stdin())),
    };

    let mut lines = byte_lines(reader);

    // Output preserved headers
    if let Some(num_headers) = config.preserve_headers {
        for _ in 0..num_headers {
            match lines.next() {
                Some(Ok(line)) => write_line(&mut io::stdout(), &line)?,
                Some(Err(e)) => {
                    eprintln!("Error reading input: {}", e);
                    process::exit(1);
                }
                None => return Ok(()), // fewer lines than headers
            }
        }
    }

    let rng = match config.seed {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_entropy(),
    };

    // Dispatch to appropriate sampling method
    let result = if let Some(k) = config.sample_size {
        reservoir_sample(lines, k, rng)?
    } else if let Some(p) = config.rate {
        probability_sample(lines, p, rng)?
    } else {
        unreachable!() // We've already checked that one must be set
    };

    write_results(result)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::io::{Cursor, Read, Write};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use tempfile::NamedTempFile;

    fn find_executable() -> PathBuf {
        let possible_locations = [
            PathBuf::from("target/debug/samp"),
            PathBuf::from("target/release/samp"),
            PathBuf::from("samp.exe"),
            PathBuf::from("samp"),
        ];

        for path in &possible_locations {
            if path.exists() {
                return path.clone();
            }
        }

        eprintln!("Warning: Executable not found in common locations. Tests might fail.");
        possible_locations[0].clone()
    }

    #[test]
    fn test_reservoir_sampling_properties() {
        let input_data = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let k = 5;
        let seed = 12345;

        let run_sample = || {
            let reader = Cursor::new(input_data);
            let rng = StdRng::seed_from_u64(seed);
            reservoir_sample(reader.lines(), k, rng).unwrap()
        };

        let sample1 = run_sample();
        let sample2 = run_sample();

        assert_eq!(sample1.len(), k);
        assert_eq!(sample2.len(), k);

        let input_set: HashSet<_> = input_data.lines().collect();
        for item in &sample1 {
            assert!(input_set.contains(&item.as_str()));
        }

        assert_eq!(sample1, sample2);
    }

    #[test]
    fn test_k_greater_than_input_len() {
        let input_data = "a\nb\nc\nd\n";
        let k = 6;
        let seed = 17;

        let reader = Cursor::new(input_data);
        let rng = StdRng::seed_from_u64(seed);
        let sample = reservoir_sample(reader.lines(), k, rng).unwrap();

        let input_lines: Vec<_> = input_data.lines().collect();
        assert_eq!(sample.len(), input_lines.len());

        let sample_set: HashSet<_> = sample.iter().collect();
        for line in input_lines {
            assert!(sample_set.contains(&line.to_string()));
        }
    }

    #[test]
    fn test_preserve_headers() {
        let input = "h1\nh2\na\nb\nc\nd\n";
        let exe_path = find_executable();

        let output = Command::new(&exe_path)
            .arg("-n")
            .arg("2")
            .arg("-p")
            .arg("2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child.stdin.as_mut().unwrap().write_all(input.as_bytes())?;
                child.wait_with_output()
            })
            .expect("Failed to run samp");

        let result = String::from_utf8_lossy(&output.stdout);
        let mut lines = result.lines();

        assert_eq!(lines.next(), Some("h1"));
        assert_eq!(lines.next(), Some("h2"));

        let sampled: Vec<&str> = lines.collect();
        assert_eq!(sampled.len(), 2);
        for &line in &sampled {
            assert!(input.contains(line));
        }
    }

    #[test]
    fn test_stdin_behavior() {
        let input_data = "a\nb\nc\nd\ne\n";
        let expected_sample_size = 3;
        let exe_path = find_executable();

        let mut child = Command::new(&exe_path)
            .arg("-n")
            .arg(expected_sample_size.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        stdin.write_all(input_data.as_bytes()).unwrap();
        drop(stdin);

        let output = child.wait_with_output().expect("Failed to wait on child");
        let result = String::from_utf8_lossy(&output.stdout);
        let result_lines: Vec<&str> = result.lines().collect();

        assert_eq!(result_lines.len(), expected_sample_size);
        for line in result_lines {
            assert!(input_data.contains(line));
        }
    }

    #[test]
    fn test_pipeline_behavior() {
        let input_data = "a\nb\nc\nd\ne\n";
        let sample_size = 4;
        let exe_path = find_executable();

        let mut child_normal = Command::new(&exe_path)
            .arg("-n")
            .arg(sample_size.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        {
            let mut stdin = child_normal.stdin.take().expect("Failed to open stdin");
            stdin.write_all(input_data.as_bytes()).unwrap();
        }

        let normal_output = child_normal
            .wait_with_output()
            .expect("Failed to wait on child");
        let normal_result = String::from_utf8_lossy(&normal_output.stdout);
        let normal_lines = normal_result.lines().count();
        assert_eq!(normal_lines, sample_size);

        let mut child_with_head = Command::new(&exe_path)
            .arg("-n")
            .arg(sample_size.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        {
            let mut stdin = child_with_head.stdin.take().expect("Failed to open stdin");
            stdin.write_all(input_data.as_bytes()).unwrap();
        }

        let mut stdout = child_with_head
            .stdout
            .take()
            .expect("Failed to open stdout");
        let mut buf = [0u8; 1];
        stdout
            .read_exact(&mut buf)
            .expect("Failed to read from stdout");
        drop(stdout);

        let status = child_with_head.wait().expect("Failed to wait on child");
        assert!(status.success());
    }

    #[test]
    fn test_file_input() {
        let exe_path = find_executable();
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        let input_data = "line1\nline2\nline3\nline4\nline5\n";
        temp_file
            .write_all(input_data.as_bytes())
            .expect("Failed to write to temp file");
        let temp_path = temp_file.path().to_str().unwrap();

        let output = Command::new(&exe_path)
            .arg("-n")
            .arg("3")
            .arg("--seed")
            .arg("17")
            .arg(temp_path)
            .output()
            .expect("Failed to execute process");

        let result = String::from_utf8_lossy(&output.stdout);
        let result_lines: Vec<&str> = result.lines().collect();

        assert_eq!(result_lines.len(), 3);
        for line in result_lines {
            assert!(input_data.contains(line));
        }
    }

    #[test]
    fn test_probability_sample_reproducibility() {
        let input_data = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let expected_output = vec!["b", "f", "g"];
        let exe_path = find_executable();

        let mut child = Command::new(&exe_path)
            .arg("-r")
            .arg("0.5")
            .arg("-s")
            .arg("17")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        {
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin.write_all(input_data.as_bytes()).unwrap();
        }

        let output = child.wait_with_output().expect("Failed to wait on child");
        let result = String::from_utf8_lossy(&output.stdout);
        let result_lines: Vec<&str> = result.lines().collect();

        assert_eq!(result_lines, expected_output);
    }

    #[test]
    fn test_probability_sample_with_headers() {
        let input_data = "HEADER1\nHEADER2\na\nb\nc\nd\ne\n";
        let exe_path = find_executable();

        let mut child = Command::new(&exe_path)
            .arg("-r")
            .arg("0.6")
            .arg("-p")
            .arg("2")
            .arg("-s")
            .arg("17")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        {
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin.write_all(input_data.as_bytes()).unwrap();
        }

        let output = child.wait_with_output().expect("Failed to wait on child");
        let result = String::from_utf8_lossy(&output.stdout);
        let mut lines = result.lines();

        // Check headers are preserved
        assert_eq!(lines.next(), Some("HEADER1"));
        assert_eq!(lines.next(), Some("HEADER2"));

        // Remaining lines are sampled — we don’t assert exact values since probability is involved,
        // but we can check they are a subset of the remaining input
        let sampled: Vec<&str> = lines.collect();
        let valid_lines = ["a", "b", "c", "d", "e"];
        for line in &sampled {
            assert!(
                valid_lines.contains(line),
                "Sampled line '{}' not in valid input set",
                line
            );
        }
    }

    #[test]
    fn test_probability_sample_stdin_only() {
        let input_data = "x\ny\nz\n";
        let exe_path = find_executable();

        let mut child = Command::new(&exe_path)
            .arg("-r")
            .arg("1.0") // Ensure all lines are selected
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        {
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin.write_all(input_data.as_bytes()).unwrap();
        }

        let output = child.wait_with_output().expect("Failed to wait on child");
        let result = String::from_utf8_lossy(&output.stdout);
        let result_lines: Vec<&str> = result.lines().collect();

        let expected: Vec<&str> = input_data.lines().collect();
        assert_eq!(result_lines, expected);
    }

    #[test]
    fn test_probability_sample_rate_zero() {
        let input_data = "a\nb\nc\nd\n";
        let exe_path = find_executable();

        let mut child = Command::new(&exe_path)
            .arg("-r")
            .arg("0.0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        {
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin.write_all(input_data.as_bytes()).unwrap();
        }

        let output = child.wait_with_output().expect("Failed to wait on child");
        let result = String::from_utf8_lossy(&output.stdout);
        let result_lines: Vec<&str> = result.lines().collect();

        assert!(
            result_lines.is_empty(),
            "Expected no output, got {:?}",
            result_lines
        );
    }

    #[test]
    fn test_probability_sample_rate_one() {
        let input_data = "x\ny\nz\n";
        let exe_path = find_executable();

        let mut child = Command::new(&exe_path)
            .arg("-r")
            .arg("1.0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        {
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin.write_all(input_data.as_bytes()).unwrap();
        }

        let output = child.wait_with_output().expect("Failed to wait on child");
        let result = String::from_utf8_lossy(&output.stdout);
        let result_lines: Vec<&str> = result.lines().collect();

        let expected: Vec<&str> = input_data.lines().collect();
        assert_eq!(result_lines, expected, "Expected all lines to be sampled");
    }

    fn run_samp(args: &[&str], input: &[u8]) -> std::process::Output {
        let mut child = Command::new(find_executable())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        child
            .stdin
            .take()
            .expect("Failed to open stdin")
            .write_all(input)
            .unwrap();

        child.wait_with_output().expect("Failed to wait on child")
    }

    #[test]
    fn test_invalid_utf8_passes_through() {
        let input = b"caf\xe9\n\xff\xfe\nplain\n";

        let output = run_samp(&["-r", "1.0"], input);
        assert!(output.status.success());
        assert_eq!(output.stdout, input);

        let output = run_samp(&["-n", "3"], input);
        assert!(output.status.success());
        assert_eq!(output.stdout, input);
    }

    #[test]
    fn test_invalid_utf8_in_header() {
        let input = b"h\xff\na\nb\n";

        let output = run_samp(&["-r", "1.0", "-p", "1"], input);
        assert!(output.status.success());
        assert_eq!(output.stdout, input);
    }

    #[test]
    fn test_crlf_preserved() {
        let input = b"h\r\na\r\nb\r\n";

        let output = run_samp(&["-r", "1.0", "-p", "1"], input);
        assert_eq!(output.stdout, input);
    }

    #[test]
    fn test_missing_final_newline_is_added() {
        let output = run_samp(&["-r", "1.0"], b"a\nb");
        assert_eq!(output.stdout, b"a\nb\n");
    }

    #[test]
    fn test_empty_lines_are_kept() {
        let input = b"a\n\n\nb\n";

        let output = run_samp(&["-r", "1.0"], input);
        assert_eq!(output.stdout, input);
    }

    #[test]
    fn test_broken_pipe_while_writing_large_sample() {
        // Larger than any stdout buffer, so the write fails mid-sample.
        let input = "some line of input\n".repeat(100_000);

        let mut child = Command::new(find_executable())
            .arg("-r")
            .arg("1.0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        drop(child.stdout.take());
        child
            .stdin
            .take()
            .expect("Failed to open stdin")
            .write_all(input.as_bytes())
            .unwrap();

        let output = child.wait_with_output().expect("Failed to wait on child");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "Unexpected failure: {}", stderr);
        assert!(stderr.is_empty(), "Unexpected stderr: {}", stderr);
    }

    #[test]
    fn test_broken_pipe_on_final_flush() {
        // Small enough to sit in a buffer until the flush at the end.
        let mut child = Command::new(find_executable())
            .arg("-r")
            .arg("1.0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        drop(child.stdout.take());
        child
            .stdin
            .take()
            .expect("Failed to open stdin")
            .write_all(b"a\nb\n")
            .unwrap();

        let output = child.wait_with_output().expect("Failed to wait on child");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "Unexpected failure: {}", stderr);
        assert!(stderr.is_empty(), "Unexpected stderr: {}", stderr);
    }

    #[test]
    fn test_preserve_headers_requires_a_number() {
        let output = run_samp(&["-n", "1", "-p"], b"a\n");
        assert!(!output.status.success());

        let output = run_samp(&["-n", "1", "-p", "data.csv"], b"a\n");
        assert!(!output.status.success());
    }

    #[test]
    fn test_preserve_headers_before_filename() {
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        temp_file.write_all(b"h\na\n").unwrap();
        let temp_path = temp_file.path().to_str().unwrap();

        let output = run_samp(&["-n", "1", "-p", "1", temp_path], b"");
        assert_eq!(output.stdout, b"h\na\n");
    }

    #[test]
    fn test_version_flag() {
        let output = Command::new(find_executable())
            .arg("--version")
            .output()
            .expect("Failed to execute process");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            format!("samp {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn test_broken_pipe_during_headers() {
        let mut child = Command::new(find_executable())
            .arg("-n")
            .arg("1")
            .arg("-p")
            .arg("2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to execute process");

        // Close the read end before samp has any input, so its first header
        // write is guaranteed to hit a closed pipe.
        drop(child.stdout.take());
        {
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin.write_all(b"h1\nh2\na\nb\n").unwrap();
        }

        let output = child.wait_with_output().expect("Failed to wait on child");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "Unexpected failure: {}", stderr);
        assert!(stderr.is_empty(), "Unexpected stderr: {}", stderr);
    }

    #[test]
    fn test_probability_sample_invalid_rate_negative() {
        let exe_path = find_executable();

        let output = Command::new(&exe_path)
            .arg("-r=-0.1")
            .output()
            .expect("Failed to execute process");

        assert!(
            !output.status.success(),
            "Expected failure for negative rate, got success"
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Rate must be between 0.0 and 1.0"),
            "Unexpected stderr: {}",
            stderr
        );
    }

    #[test]
    fn test_probability_sample_invalid_rate_too_large() {
        let exe_path = find_executable();

        let output = Command::new(&exe_path)
            .arg("-r")
            .arg("1.5")
            .output()
            .expect("Failed to execute process");

        assert!(
            !output.status.success(),
            "Expected failure for rate > 1.0, got success"
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Rate must be between 0.0 and 1.0"),
            "Unexpected stderr: {}",
            stderr
        );
    }
}
