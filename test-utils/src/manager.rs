use std::{
    fs::{self, File},
    io::Read,
    os::unix::prelude::{AsRawFd, FromRawFd},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub struct Manager {
    pub process: Option<Child>,
    pub storage_dir: String,
    api: String,
    instance_name: String,
}

impl Manager {
    pub fn new(output_dir: &str, name: &str, node_index: u16, api: String) -> Self {
        let instance_name = format!("{}_{}", name, node_index);
        let storage_dir = format!("{}/{}", output_dir, instance_name);
        fs::remove_dir_all(&storage_dir).unwrap_or_default();
        fs::create_dir_all(&storage_dir).unwrap();

        Manager {
            process: None,
            storage_dir,
            api,
            instance_name,
        }
    }

    pub async fn start(&mut self, command: &str, args: &[&str]) {
        if self.process.is_none() {
            let path = format!("{}/test.log", self.storage_dir);
            let log_file = File::create(&path).unwrap();
            let fd = log_file.as_raw_fd();
            let out = unsafe { Stdio::from_raw_fd(fd) };
            let err = unsafe { Stdio::from_raw_fd(fd) };
            println!("Starting: {}", self.instance_name);
            let child = Command::new(command)
                .stdout(out)
                .stderr(err)
                .args(args)
                .spawn()
                .unwrap();

            self.process = Some(child);

            let i = Instant::now();
            let started = loop {
                if self.has_started().await {
                    break true;
                };
                if i.elapsed() >= Duration::from_secs(30) {
                    break false;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            };
            if !started {
                let mut file = File::open(&path).unwrap();
                let mut buf = String::new();
                file.read_to_string(&mut buf).unwrap();
                println!("Timed out waiting to start: {}", self.instance_name);
                println!("Begin log file: {}", path);
                println!("{}", buf);
                println!("End of log file.");
                panic!("Failed to start {}", self.instance_name);
            } else {
                println!("Successfully started: {}", self.instance_name);
            }
        }
    }

    async fn has_started(&self) -> bool {
        reqwest::get(self.api.clone()).await.is_ok()
    }

    pub fn kill(&mut self) {
        if let Some(mut process) = self.process.take() {
            println!("Killing: {}", self.instance_name);
            process.kill().unwrap_or_default();
            process.wait().unwrap();
            self.process = None
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.kill()
    }
}
