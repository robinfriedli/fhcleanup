use chrono::NaiveDateTime;
use lazy_static::lazy_static;
use regex::Regex;
use rusty_pool::{Builder, ThreadPool};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::SystemTime;
use std::{fs, fs::DirEntry};
use structopt::StructOpt;

/// Clear Windows file history files by finding files with the same name except for a UTC timestamp
/// within the same directory and keeping the latest version of the file, triming the timestamp
/// from the file name (unless disabled) and moving all other copies of the file to a to_delete
/// folder or deleting them instantly (depending on whether --purge is set).
#[derive(StructOpt, Debug)]
#[structopt(name = "fhcleanup")]
struct Opt {
    /// handle all subdirs recursively
    #[structopt(short = "r", long)]
    incl_subdir: bool,

    /// the maximum amount of worker threads to spawn to handle directories,
    /// defaults to the number of CPUs multiplied by four
    #[structopt(short = "t", long)]
    max_threads: Option<u32>,

    /// the target directory to move files that should be deleted,
    /// defaults to `./fhcleanup_to_del/`
    #[structopt(short = "f", long, parse(from_os_str))]
    target_folder: Option<PathBuf>,

    /// whether to delete matching files instantly instead of moving them to a temp folder,
    /// this renders the target_folder arg irrelevant, defaults to false
    #[structopt(short = "p", long)]
    purge: bool,

    /// keep the original file names including timestamp
    #[structopt(short = "n", long)]
    keep_names: bool,

    /// Verbose mode (-v, -vv, -vvv, etc.)
    #[structopt(short, long, parse(from_occurrences))]
    verbose: u8,
}

struct FHFile {
    date: NaiveDateTime,
    dir_path: Option<String>,
    full_name: String,
}

impl FHFile {
    fn rename(self, trimmed_name: &str, verbosity_level: u8) {
        let dir_path = self.dir_path.unwrap_or_else(|| String::from("./"));
        let source_name = dir_path.clone() + self.full_name.as_str();
        let target_name = dir_path + trimmed_name;
        if let Err(e) = fs::rename(&source_name, &target_name) {
            eprintln!("Could not rename '{}': {}", source_name, e);
        } else {
            RENAMED_COUNT.fetch_add(1, Ordering::SeqCst);
            if verbosity_level >= 1 {
                println!("Renamed '{}' to '{}'", source_name, target_name);
            }
        }
    }

    fn mov(self, temp_dir: String, verbosity_level: u8) {
        let target_dir = temp_dir + self.dir_path.as_deref().unwrap_or("");
        if let Err(e) = fs::create_dir_all(&target_dir) {
            panic!("could not create to_delete folder '{}': {}", &target_dir, e);
        }

        let source_name =
            self.dir_path.unwrap_or_else(|| String::from("./")) + self.full_name.as_str();
        let target_name = target_dir + self.full_name.as_str();
        if let Err(e) = fs::rename(&source_name, &target_name) {
            eprintln!("Could not mov '{}': {}", source_name, e);
        } else {
            MOVED_COUNT.fetch_add(1, Ordering::SeqCst);
            if verbosity_level >= 1 {
                println!("Moved '{}' to '{}'", source_name, target_name);
            }
        }
    }

    fn delete(self, verbosity_level: u8) {
        let source_name =
            self.dir_path.unwrap_or_else(|| String::from("./")) + self.full_name.as_str();
        if let Err(e) = fs::remove_file(&source_name) {
            eprintln!("Could not delete '{}': {}", source_name, e);
        } else {
            DELETED_COUNT.fetch_add(1, Ordering::SeqCst);
            if verbosity_level >= 1 {
                println!("Deleted '{}'", &source_name);
            }
        }
    }
}

static MOVED_COUNT: AtomicUsize = AtomicUsize::new(0);
static DELETED_COUNT: AtomicUsize = AtomicUsize::new(0);
static RENAMED_COUNT: AtomicUsize = AtomicUsize::new(0);

lazy_static! {
    static ref FILE_NAME_REGEX: Regex =
        Regex::new(r"\(\d{4}_\d{2}_\d{2} \d{2}_\d{2}_\d{2} UTC\)\..*")
            .expect("failed to compile regex");
    static ref DATE_PART_REGEX: Regex =
        Regex::new(r"\d{4}_\d{2}_\d{2} \d{2}_\d{2}_\d{2}").expect("failed to compile regex");
    static ref FILE_END_REGEX: Regex = Regex::new(r"\(\d{4}_\d{2}_\d{2} \d{2}_\d{2}_\d{2} UTC\)\.")
        .expect("failed to compile regex");
}

fn main() {
    let now = SystemTime::now();

    let mut opt = Opt::from_args();

    if let Some(target_dir) = opt.target_folder.take() {
        let mut dir_str = target_dir
            .into_os_string()
            .into_string()
            .expect("Input for target_folder arg is not valid UTF-8");
        if !dir_str.ends_with('/') {
            dir_str.push('/');
        }

        opt.target_folder = Some(PathBuf::from(dir_str));
    }

    let opt = Arc::new(opt);

    let pool = if let Some(max_size) = opt.max_threads {
        Builder::new().max_size(max_size).build()
    } else {
        ThreadPool::default()
    };

    let cloned_pool = pool.clone();
    pool.execute(|| handle_dir(None, opt, cloned_pool));
    pool.join();

    println!("__________________________________________________________");

    let moved_count = MOVED_COUNT.load(Ordering::SeqCst);
    let deleted_count = DELETED_COUNT.load(Ordering::SeqCst);
    let renamed_count = RENAMED_COUNT.load(Ordering::SeqCst);

    if moved_count > 0 {
        println!("Moved {} files to the to_delete folder", moved_count);
    }
    if deleted_count > 0 {
        println!("Deleted {} files", deleted_count);
    }
    if renamed_count > 0 {
        println!("Renamed {} kept files to remove timestamp", renamed_count);
    }
    if moved_count == 0 && deleted_count == 0 && renamed_count == 0 {
        println!("No files affected");
    }

    if let Ok(elapsed) = now.elapsed() {
        println!("Done after {}ms", elapsed.as_millis());
    } else {
        println!("Done");
    }
}

fn handle_dir(path: Option<String>, opt: Arc<Opt>, pool: ThreadPool) {
    let current_path = path.clone().unwrap_or_else(|| String::from("./"));
    match fs::read_dir(&current_path) {
        Ok(dir_elems) => {
            let mut fh_files_map: HashMap<String, Vec<FHFile>> = HashMap::new();
            let verbosity_level = opt.verbose;
            if verbosity_level >= 2 {
                println!("stepping into dir: {}", &current_path);
            }

            for dir_elem in dir_elems {
                match dir_elem {
                    Ok(dir_elem) => {
                        handle_dir_elem(
                            dir_elem,
                            &opt,
                            &pool,
                            &current_path,
                            &mut fh_files_map,
                            &path,
                        );
                    }
                    Err(e) => eprintln!("could not read dir element: {}", e),
                }
            }

            handle_results(fh_files_map, current_path, verbosity_level, opt);
        }
        Err(e) => eprintln!("could not open dir '{}': {}", &current_path, e),
    }
}

#[inline]
fn handle_dir_elem(
    dir_elem: DirEntry,
    opt: &Arc<Opt>,
    pool: &ThreadPool,
    current_path: &String,
    mut fh_files_map: &mut HashMap<String, Vec<FHFile>>,
    path: &Option<String>,
) {
    let file_type = dir_elem.file_type();
    match file_type {
        Ok(file_type) => {
            if opt.incl_subdir && file_type.is_dir() {
                let cloned_pool = pool.clone();
                let cloned_opt = opt.clone();
                let current_path = current_path.clone();
                pool.execute(move || {
                    handle_dir(
                        Some(
                            current_path
                                + dir_elem.file_name().to_str().unwrap_or_else(|| {
                                    panic!("Invalid UTF-8 file name: '{:?}'", dir_elem.file_name())
                                })
                                + "/",
                        ),
                        cloned_opt,
                        cloned_pool,
                    )
                });
            } else if file_type.is_file() {
                handle_file(dir_elem, &mut fh_files_map, path);
            }
        }
        Err(e) => eprintln!(
            "Could not determine file type of {:?}: {}",
            dir_elem.path(),
            e
        ),
    }
}

#[inline]
fn handle_file(
    dir_elem: DirEntry,
    fh_files_map: &mut HashMap<String, Vec<FHFile>>,
    path: &Option<String>,
) {
    match dir_elem.file_name().to_str() {
        Some(file_name) if FILE_NAME_REGEX.is_match(file_name) => {
            let date_str = FILE_NAME_REGEX
                .find_iter(file_name)
                .last()
                .expect("no last item found for regex despite is_match returning true")
                .as_str();
            let date = DATE_PART_REGEX
                .find(date_str)
                .unwrap_or_else(|| panic!("could not extract date from {}", date_str))
                .as_str();
            let parsed_date = NaiveDateTime::parse_from_str(date, "%Y_%m_%d %H_%M_%S")
                .unwrap_or_else(|_| panic!("could not parse date: '{}'", date));

            let mut parts = FILE_END_REGEX.split(file_name).collect::<Vec<&str>>();
            let extension = parts.pop().expect("file parts empty");
            let mut trimmed_name = parts
                .into_iter()
                .map(|part| part.trim())
                .fold(String::new(), |a, b| a + b);
            trimmed_name.push('.');
            trimmed_name.push_str(extension);

            let fh_file = FHFile {
                date: parsed_date,
                dir_path: path.clone(),
                full_name: String::from(file_name),
            };
            put_multi_map(fh_files_map, trimmed_name, fh_file);
        }
        None => eprintln!("Invalid UTF-8 file name: '{:?}'", dir_elem.file_name()),
        // irrelevant file name
        Some(_) => {}
    }
}

#[inline]
fn handle_results(
    fh_files_map: HashMap<String, Vec<FHFile>>,
    current_path: String,
    verbosity_level: u8,
    opt: Arc<Opt>,
) {
    if fh_files_map.is_empty() {
        if verbosity_level >= 2 {
            println!("No relevant files found in dir '{}'", &current_path);
        }
    } else {
        if verbosity_level >= 2 {
            let file_count: usize = fh_files_map.iter().map(|entry| entry.1.len()).sum();
            println!(
                "Found {} relevant files in dir '{}'",
                file_count, &current_path
            );
        }

        for file_entry in fh_files_map.into_iter() {
            let trimmed_name = file_entry.0;
            let mut file_duplicates = file_entry.1;

            let trimmed_path = current_path.clone() + &trimmed_name;
            let should_rename = if Path::new(&trimmed_path).exists() {
                if verbosity_level >= 1 {
                    println!("File without timestamp already exists, treating all other files as duplicates: {}", &trimmed_path);
                }

                false
            } else {
                true
            };

            file_duplicates.sort_by_key(|f| f.date);
            let file_count = file_duplicates.len();

            if verbosity_level >= 2 {
                println!(
                    "Found {} matching files for '{}'",
                    file_count, &trimmed_name
                );
            }

            for (i, file) in file_duplicates.into_iter().enumerate() {
                // rename last file
                if should_rename && !opt.keep_names && i == file_count - 1 {
                    file.rename(&trimmed_name, verbosity_level);
                } else if !should_rename || i < file_count - 1 {
                    if opt.purge {
                        file.delete(verbosity_level);
                    } else {
                        let temp_dir = if let Some(ref dir) = opt.target_folder {
                            String::from(
                                dir.to_str()
                                    .unwrap_or_else(|| panic!("Invalid UTF-8 path: '{:?}'", dir)),
                            )
                        } else {
                            String::from("./fhcleanup_to_del/")
                        };

                        file.mov(temp_dir, verbosity_level);
                    }
                }
            }
        }
    }
}

#[inline]
fn put_multi_map(map: &mut HashMap<String, Vec<FHFile>>, key: String, elem: FHFile) {
    if let Some(vec) = map.get_mut(&key) {
        vec.push(elem);
    } else {
        map.insert(key, vec![elem]);
    }
}
