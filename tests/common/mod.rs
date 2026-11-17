use std::{
    path::PathBuf,
    process::{Child, Command, Output},
    time::{Duration, Instant},
};

pub struct Harness {
    pub dir: tempfile::TempDir,
    pub port: u16,
    child: Option<Child>,
    pub socket: Option<std::net::TcpListener>,
}
impl Harness {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        std::fs::create_dir_all(dir.path().join("config/shrinkgate")).unwrap();
        std::fs::write(
            dir.path().join("config/shrinkgate/config.toml"),
            format!("port = {port}\n"),
        )
        .unwrap();
        Self {
            dir,
            port,
            child: None,
            socket: Some(socket),
        }
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    pub fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_shrinkgate"));
        c.env("USERPROFILE", self.dir.path())
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("CLAUDE_CONFIG_DIR", self.path("claude"))
            .env("CODEX_HOME", self.path("codex"));
        c
    }
    pub fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    pub fn ok(&self, args: &[&str]) -> Output {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&o.stderr)
        );
        o
    }
    pub fn start(&mut self) {
        self.start_with(&[]);
    }
    pub fn start_with(&mut self, args: &[&str]) {
        drop(self.socket.take());
        self.child = Some(
            self.command()
                .args(["serve", "--quiet"])
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            assert!(Instant::now() < deadline, "engine did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
