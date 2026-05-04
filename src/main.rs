use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use log::{LevelFilter, error, info};
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::sync::Mutex;

#[derive(Clone)]
struct AppServer {
    root: PathBuf,
    username: String,
    password: String,
}

impl russh::server::Server for AppServer {
    type Handler = SshSession;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
        SshSession::new(
            self.root.clone(),
            self.username.clone(),
            self.password.clone(),
        )
    }
}

struct SshSession {
    clients: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    root: PathBuf,
    username: String,
    password: String,
}

impl SshSession {
    fn new(root: PathBuf, username: String, password: String) -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            root,
            username,
            password,
        }
    }

    async fn take_channel(&mut self, channel_id: ChannelId) -> Option<Channel<Msg>> {
        let mut clients = self.clients.lock().await;
        clients.remove(&channel_id)
    }
}

impl russh::server::Handler for SshSession {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == self.username && password == self.password {
            info!("accepted password auth for {user}");
            return Ok(Auth::Accept);
        }

        info!("rejected password auth for {user}");
        Ok(Auth::Reject {
            proceed_with_methods: None,
            partial_success: false,
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        _public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        info!("rejected public key auth for {user}");
        Ok(Auth::Reject {
            proceed_with_methods: None,
            partial_success: false,
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let mut clients = self.clients.lock().await;
        clients.insert(channel.id(), channel);
        Ok(true)
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        info!("subsystem request: {name}");

        if name != "sftp" {
            session.channel_failure(channel_id)?;
            return Ok(());
        }

        let Some(channel) = self.take_channel(channel_id).await else {
            session.channel_failure(channel_id)?;
            return Ok(());
        };

        let sftp = FsSftpSession::new(self.root.clone());
        session.channel_success(channel_id)?;
        russh_sftp::server::run(channel.into_stream(), sftp).await;

        Ok(())
    }
}

struct FsSftpSession {
    root: PathBuf,
    version: Option<u32>,
    next_handle: u64,
    handles: HashMap<String, HandleEntry>,
}

enum HandleEntry {
    File(FileHandle),
    Dir(DirHandle),
}

struct FileHandle {
    path: PathBuf,
    can_read: bool,
    can_write: bool,
}

struct DirHandle {
    path: PathBuf,
    entries: Vec<File>,
    cursor: usize,
}

impl FsSftpSession {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            version: None,
            next_handle: 0,
            handles: HashMap::new(),
        }
    }

    fn alloc_handle(&mut self, prefix: &str) -> String {
        let handle = format!("{prefix}-{}", self.next_handle);
        self.next_handle += 1;
        handle
    }

    fn normalize_virtual_path(&self, path: &str) -> Result<String, StatusCode> {
        let mut normalized = Vec::new();
        let input = if path.is_empty() { "/" } else { path };

        for component in Path::new(input).components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(part) => normalized.push(part.to_string_lossy().into_owned()),
                Component::ParentDir => return Err(StatusCode::PermissionDenied),
                Component::Prefix(_) => return Err(StatusCode::PermissionDenied),
            }
        }

        if normalized.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", normalized.join("/")))
        }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf, StatusCode> {
        let normalized = self.normalize_virtual_path(path)?;
        let mut resolved = self.root.clone();
        for part in normalized.split('/').filter(|part| !part.is_empty()) {
            resolved.push(part);
        }
        Ok(resolved)
    }

    fn attrs_from_metadata(&self, metadata: &fs::Metadata) -> FileAttributes {
        let mut attrs = FileAttributes::empty();
        attrs.size = Some(metadata.len());
        attrs.permissions = Some(if metadata.permissions().readonly() {
            0o555
        } else {
            0o777
        });
        attrs.atime = Some(system_time_to_secs(
            metadata.accessed().unwrap_or(UNIX_EPOCH),
        ));
        attrs.mtime = Some(system_time_to_secs(
            metadata.modified().unwrap_or(UNIX_EPOCH),
        ));

        if metadata.file_type().is_dir() {
            attrs.set_dir(true);
        } else if metadata.file_type().is_symlink() {
            attrs.set_symlink(true);
        } else {
            attrs.set_regular(true);
        }

        attrs
    }

    fn status(&self, id: u32, code: StatusCode) -> Status {
        Status {
            id,
            status_code: code,
            error_message: code.to_string(),
            language_tag: "en-US".to_string(),
        }
    }
}

impl russh_sftp::server::Handler for FsSftpSession {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        version: u32,
        extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        if self.version.replace(version).is_some() {
            error!("duplicate SFTP init packet");
            return Err(StatusCode::ConnectionLost);
        }

        info!("sftp version={version} extensions={extensions:?}");
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = self.resolve_path(&filename)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(map_io_error)?;
        }

        let options: fs::OpenOptions = pflags.into();
        options.open(&path).map_err(map_io_error)?;

        let handle = self.alloc_handle("file");
        self.handles.insert(
            handle.clone(),
            HandleEntry::File(FileHandle {
                path,
                can_read: pflags.contains(OpenFlags::READ),
                can_write: pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND),
            }),
        );

        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle).ok_or(StatusCode::NoSuchFile)?;
        Ok(self.status(id, StatusCode::Ok))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let entry = self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let file = match entry {
            HandleEntry::File(file) => file,
            HandleEntry::Dir(_) => return Err(StatusCode::BadMessage),
        };

        if !file.can_read {
            return Err(StatusCode::PermissionDenied);
        }

        let mut opened = fs::OpenOptions::new()
            .read(true)
            .open(&file.path)
            .map_err(map_io_error)?;
        opened.seek(SeekFrom::Start(offset)).map_err(map_io_error)?;

        let mut buffer = vec![0; len as usize];
        let bytes_read = opened.read(&mut buffer).map_err(map_io_error)?;
        if bytes_read == 0 {
            return Err(StatusCode::Eof);
        }
        buffer.truncate(bytes_read);

        Ok(Data { id, data: buffer })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let entry = self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let file = match entry {
            HandleEntry::File(file) => file,
            HandleEntry::Dir(_) => return Err(StatusCode::BadMessage),
        };

        if !file.can_write {
            return Err(StatusCode::PermissionDenied);
        }

        let mut opened = fs::OpenOptions::new()
            .write(true)
            .open(&file.path)
            .map_err(map_io_error)?;
        opened.seek(SeekFrom::Start(offset)).map_err(map_io_error)?;
        opened.write_all(&data).map_err(map_io_error)?;
        opened.flush().map_err(map_io_error)?;

        Ok(self.status(id, StatusCode::Ok))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let path = self.resolve_path(&path)?;
        let metadata = fs::symlink_metadata(path).map_err(map_io_error)?;
        Ok(Attrs {
            id,
            attrs: self.attrs_from_metadata(&metadata),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let path = match self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)? {
            HandleEntry::File(file) => file.path.clone(),
            HandleEntry::Dir(dir) => dir.path.clone(),
        };
        let metadata = fs::symlink_metadata(path).map_err(map_io_error)?;
        Ok(Attrs {
            id,
            attrs: self.attrs_from_metadata(&metadata),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let path = self.resolve_path(&path)?;
        apply_attrs(&path, &attrs)?;
        Ok(self.status(id, StatusCode::Ok))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let path = match self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)? {
            HandleEntry::File(file) => file.path.clone(),
            HandleEntry::Dir(dir) => dir.path.clone(),
        };
        apply_attrs(&path, &attrs)?;
        Ok(self.status(id, StatusCode::Ok))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let resolved = self.resolve_path(&path)?;
        let read_dir = fs::read_dir(&resolved).map_err(map_io_error)?;
        let mut entries = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(map_io_error)?;
            let metadata = entry.metadata().map_err(map_io_error)?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            entries.push(File::new(file_name, self.attrs_from_metadata(&metadata)));
        }

        let handle = self.alloc_handle("dir");
        self.handles.insert(
            handle.clone(),
            HandleEntry::Dir(DirHandle {
                path: resolved,
                entries,
                cursor: 0,
            }),
        );
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let entry = self
            .handles
            .get_mut(&handle)
            .ok_or(StatusCode::NoSuchFile)?;
        let dir = match entry {
            HandleEntry::Dir(dir) => dir,
            HandleEntry::File(_) => return Err(StatusCode::BadMessage),
        };

        if dir.cursor >= dir.entries.len() {
            return Err(StatusCode::Eof);
        }

        let next = (dir.cursor + 64).min(dir.entries.len());
        let files = dir.entries[dir.cursor..next].to_vec();
        dir.cursor = next;
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let path = self.resolve_path(&filename)?;
        fs::remove_file(path).map_err(map_io_error)?;
        Ok(self.status(id, StatusCode::Ok))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let path = self.resolve_path(&path)?;
        fs::create_dir(path).map_err(map_io_error)?;
        Ok(self.status(id, StatusCode::Ok))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let path = self.resolve_path(&path)?;
        fs::remove_dir(path).map_err(map_io_error)?;
        Ok(self.status(id, StatusCode::Ok))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let normalized = self.normalize_virtual_path(&path)?;
        Ok(Name {
            id,
            files: vec![File::dummy(normalized)],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let path = self.resolve_path(&path)?;
        let metadata = fs::metadata(path).map_err(map_io_error)?;
        Ok(Attrs {
            id,
            attrs: self.attrs_from_metadata(&metadata),
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let oldpath = self.resolve_path(&oldpath)?;
        let newpath = self.resolve_path(&newpath)?;
        if let Some(parent) = newpath.parent() {
            fs::create_dir_all(parent).map_err(map_io_error)?;
        }
        fs::rename(oldpath, newpath).map_err(map_io_error)?;
        Ok(self.status(id, StatusCode::Ok))
    }
}

fn apply_attrs(path: &Path, attrs: &FileAttributes) -> Result<(), StatusCode> {
    if let Some(size) = attrs.size {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|file| file.set_len(size))
            .map_err(map_io_error)?;
    }

    Ok(())
}

fn system_time_to_secs(time: SystemTime) -> u32 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(u32::MAX as u64) as u32
}

fn map_io_error(error: std::io::Error) -> StatusCode {
    use std::io::ErrorKind;

    match error.kind() {
        ErrorKind::NotFound => StatusCode::NoSuchFile,
        ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        ErrorKind::AlreadyExists => StatusCode::Failure,
        ErrorKind::UnexpectedEof => StatusCode::Eof,
        _ => StatusCode::Failure,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    env_logger::builder().filter_level(LevelFilter::Info).init();

    let host = std::env::var("SFTP_BIND_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("SFTP_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2222);
    let username = std::env::var("SFTP_USERNAME").unwrap_or_else(|_| "demo".to_string());
    let password = std::env::var("SFTP_PASSWORD").unwrap_or_else(|_| "demo".to_string());
    let root = detect_root()?;

    let config = russh::server::Config {
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::from_secs(0)),
        keys: vec![russh::keys::PrivateKey::random(
            &mut rand::rng(),
            russh::keys::ssh_key::Algorithm::Ed25519,
        )?],
        ..Default::default()
    };

    info!(
        "starting experimental SFTP server on {host}:{port}, root={}, username={username}",
        root.display()
    );

    let mut server = AppServer {
        root,
        username,
        password,
    };

    server
        .run_on_address(Arc::new(config), (host.as_str(), port))
        .await
        .with_context(|| format!("failed to bind SFTP server on {host}:{port}"))?;

    Ok(())
}

fn detect_root() -> anyhow::Result<PathBuf> {
    if let Ok(root) = std::env::var("SFTP_FS_ROOT") {
        let trimmed = root.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            fs::create_dir_all(&path)
                .with_context(|| format!("failed to create SFTP_FS_ROOT at {}", path.display()))?;
            return Ok(path);
        }
    }

    let path = PathBuf::from("data");
    fs::create_dir_all(&path)
        .with_context(|| format!("failed to create default data dir at {}", path.display()))?;
    Ok(path)
}
