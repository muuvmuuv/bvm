use async_trait::async_trait;
use dprint_cli_core::types::ErrBox;
use dprint_cli_core::{download_url, log_action_with_progress, ProgressBars};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use super::Environment;

#[derive(Clone)]
pub struct RealEnvironment {
    output_lock: Arc<Mutex<u8>>,
    progress_bars: Arc<ProgressBars>,
    is_verbose: bool,
    ignore_shell_commands: Arc<Mutex<bool>>,
}

impl RealEnvironment {
    pub fn new(is_verbose: bool) -> Result<RealEnvironment, ErrBox> {
        let environment = RealEnvironment {
            output_lock: Arc::new(Mutex::new(0)),
            progress_bars: Arc::new(ProgressBars::new()),
            is_verbose,
            ignore_shell_commands: Arc::new(Mutex::new(false)),
        };

        environment.create_dir_all(&environment.get_local_user_data_dir())?;
        environment.create_dir_all(&environment.get_user_data_dir())?;

        Ok(environment)
    }
}

#[async_trait]
impl Environment for RealEnvironment {
    fn is_real(&self) -> bool {
        true
    }

    fn ignore_shell_commands(&self) {
        let mut ignore_shell_commands = self.ignore_shell_commands.lock().unwrap();
        *ignore_shell_commands = true;
    }

    fn read_file_text(&self, file_path: &Path) -> Result<String, ErrBox> {
        let file_bytes = self.read_file(file_path)?;
        Ok(String::from_utf8(file_bytes)?)
    }

    fn read_file(&self, file_path: &Path) -> Result<Vec<u8>, ErrBox> {
        log_verbose!(self, "Reading file: {}", file_path.display());
        Ok(fs::read(file_path)?)
    }

    fn write_file_text(&self, file_path: &Path, file_text: &str) -> Result<(), ErrBox> {
        self.write_file(file_path, file_text.as_bytes())
    }

    fn write_file(&self, file_path: &Path, bytes: &[u8]) -> Result<(), ErrBox> {
        log_verbose!(self, "Writing file: {}", file_path.display());
        match fs::write(file_path, bytes) {
            Ok(_) => Ok(()),
            Err(err) => err!("Error writing file {}: {}", file_path.display(), err.to_string()),
        }
    }

    fn remove_file(&self, file_path: &Path) -> Result<(), ErrBox> {
        log_verbose!(self, "Deleting file: {}", file_path.display());
        fs::remove_file(file_path)?;
        Ok(())
    }

    fn remove_dir_all(&self, dir_path: &Path) -> Result<(), ErrBox> {
        log_verbose!(self, "Deleting directory: {}", dir_path.display());
        fs::remove_dir_all(dir_path)?;
        Ok(())
    }

    async fn download_file(&self, url: &str) -> Result<Vec<u8>, ErrBox> {
        log_verbose!(self, "Downloading url: {}", url);
        download_url(url, &self.progress_bars).await
    }

    fn path_exists(&self, file_path: &Path) -> bool {
        log_verbose!(self, "Checking path exists: {}", file_path.display());
        file_path.exists()
    }

    fn is_dir_empty(&self, dir_path: &Path) -> Result<bool, ErrBox> {
        Ok(std::fs::read_dir(dir_path)?.next().is_none())
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), ErrBox> {
        fs::create_dir_all(path)?;
        Ok(())
    }

    fn cwd(&self) -> Result<PathBuf, ErrBox> {
        Ok(env::current_dir()?)
    }

    fn log(&self, text: &str) {
        let _g = self.output_lock.lock().unwrap();
        println!("{}", text);
    }

    fn log_error(&self, text: &str) {
        let _g = self.output_lock.lock().unwrap();
        eprintln!("{}", text);
    }

    async fn log_action_with_progress<
        TResult: std::marker::Send + std::marker::Sync,
        TCreate: FnOnce(Box<dyn Fn(usize)>) -> TResult + std::marker::Send + std::marker::Sync,
    >(
        &self,
        message: &str,
        action: TCreate,
        total_size: usize,
    ) -> Result<TResult, ErrBox> {
        log_action_with_progress(&self.progress_bars, message, action, total_size).await
    }

    fn get_local_user_data_dir(&self) -> PathBuf {
        log_verbose!(self, "Getting local user data directory.");
        if cfg!(target_os = "windows") {
            // %LOCALAPPDATA% is used because we don't want to sync this data across a domain.
            let dir = dirs::data_local_dir().expect("Could not get local data dir");
            dir.join("bvm")
        } else {
            get_home_dir()
        }
    }

    fn get_user_data_dir(&self) -> PathBuf {
        log_verbose!(self, "Getting user data directory.");
        if cfg!(target_os = "windows") {
            let dir = dirs::data_dir().expect("Could not get data dir");
            dir.join("bvm")
        } else {
            get_home_dir()
        }
    }

    fn get_system_path_dirs(&self) -> Vec<PathBuf> {
        log_verbose!(self, "Getting system path directories.");
        super::common::get_system_path_dirs()
    }

    #[cfg(windows)]
    fn ensure_system_path(&self, directory_path: &str) -> Result<(), ErrBox> {
        use winreg::{enums::*, RegKey};
        log_verbose!(self, "Ensuring '{}' is on the path.", directory_path);

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (env, _) = hkcu.create_subkey("Environment")?;
        let mut path: String = env.get_value("Path")?;

        // add to the path if it doesn't have this entry
        if !path.split(";").any(|p| p == directory_path) {
            if !path.is_empty() && !path.ends_with(';') {
                path.push_str(";")
            }
            path.push_str(&directory_path);
            env.set_value("Path", &path)?;
        }
        Ok(())
    }

    #[cfg(windows)]
    fn ensure_system_path_pre(&self, directory_path: &str) -> Result<(), ErrBox> {
        use winreg::{enums::*, RegKey};

        // always puts the provided directory at the start of the path
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (env, _) = hkcu.create_subkey("Environment")?;
        let path: String = env.get_value("Path")?;
        let mut paths = path.split(";").collect::<Vec<_>>();
        paths.retain(|p| p != &directory_path && !p.is_empty());
        paths.insert(0, directory_path);
        env.set_value("Path", &paths.join(";"))?;

        Ok(())
    }

    #[cfg(windows)]
    fn remove_system_path(&self, directory_path: &str) -> Result<(), ErrBox> {
        use winreg::{enums::*, RegKey};
        log_verbose!(self, "Ensuring '{}' is on the path.", directory_path);

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (env, _) = hkcu.create_subkey("Environment")?;
        let path: String = env.get_value("Path")?;
        let mut paths = path.split(";").collect::<Vec<_>>();
        let original_len = paths.len();

        paths.retain(|p| p != &directory_path);

        let was_removed = original_len != paths.len();
        if was_removed {
            env.set_value("Path", &paths.join(";"))?;
        }
        Ok(())
    }

    fn get_time_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn run_shell_command(&self, cwd: &Path, command: &str) -> Result<(), ErrBox> {
        if *self.ignore_shell_commands.lock().unwrap() {
            return Ok(());
        }

        #[cfg(unix)]
        return finalize_and_run_command(cwd, Command::new("/bin/sh").arg("-c").arg(command));

        #[cfg(target_os = "windows")]
        return finalize_and_run_command(cwd, Command::new("cmd").arg("/C").arg(command));

        fn finalize_and_run_command(cwd: &Path, command: &mut Command) -> Result<(), ErrBox> {
            let status = command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .current_dir(cwd)
                .status()?;
            handle_exit_status(status)
        }

        fn handle_exit_status(exit_status: ExitStatus) -> Result<(), ErrBox> {
            match exit_status.code() {
                Some(code) => {
                    if code != 0 {
                        return err!("Received non zero exit code from shell command: {}", code);
                    } else {
                        Ok(())
                    }
                }
                None => err!("Process terminated by signal."),
            }
        }
    }

    fn exit(&self, code: i32) -> Result<(), ErrBox> {
        std::process::exit(code)
    }

    fn is_verbose(&self) -> bool {
        self.is_verbose
    }
}

fn get_home_dir() -> PathBuf {
    let dir = dirs::home_dir().expect("Could not get home data dir");
    dir.join(".bvm")
}
