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
        // OpenSSH scp checks the remote SSH process status after SFTP completes.
        session.exit_status_request(channel, 0)?;
        session.close(channel)?;
        self.take_channel(channel).await;
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.take_channel(channel).await;
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
    file: fs::File,
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
                Component::ParentDir => {
                    // SFTP paths use a virtual root; /.. stays at that root.
                    normalized.pop();
                }
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            attrs.permissions = Some(metadata.permissions().mode());
        }
        // WASIp2 exposes access capabilities, not POSIX permission bits.
        #[cfg(not(unix))]
        {
            // WASI's readonly flag is not a POSIX mode; using it here can
            // cause scp -p to make downloaded files unexpectedly read-only.
            attrs.permissions = Some(if metadata.is_dir() { 0o755 } else { 0o644 });
        }
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
        let options: fs::OpenOptions = pflags.into();
        let file = options.open(&path).map_err(map_io_error)?;
        if file.metadata().map_err(map_io_error)?.is_dir() {
            return Err(StatusCode::Failure);
        }

        let handle = self.alloc_handle("file");
        self.handles.insert(
            handle.clone(),
            HandleEntry::File(FileHandle {
                file,
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
        let entry = self
            .handles
            .get_mut(&handle)
            .ok_or(StatusCode::NoSuchFile)?;
        let file = match entry {
            HandleEntry::File(file) => file,
            HandleEntry::Dir(_) => return Err(StatusCode::BadMessage),
        };

        if !file.can_read {
            return Err(StatusCode::PermissionDenied);
        }

        file.file
            .seek(SeekFrom::Start(offset))
            .map_err(map_io_error)?;

        let mut buffer = vec![0; len as usize];
        let bytes_read = file.file.read(&mut buffer).map_err(map_io_error)?;
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
        let entry = self
            .handles
            .get_mut(&handle)
            .ok_or(StatusCode::NoSuchFile)?;
        let file = match entry {
            HandleEntry::File(file) => file,
            HandleEntry::Dir(_) => return Err(StatusCode::BadMessage),
        };

        if !file.can_write {
            return Err(StatusCode::PermissionDenied);
        }

        file.file
            .seek(SeekFrom::Start(offset))
            .map_err(map_io_error)?;
        file.file.write_all(&data).map_err(map_io_error)?;
        file.file.flush().map_err(map_io_error)?;

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
        let metadata = match self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)? {
            HandleEntry::File(file) => file.file.metadata(),
            HandleEntry::Dir(dir) => fs::metadata(&dir.path),
        }
        .map_err(map_io_error)?;
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
        match self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)? {
            HandleEntry::File(file) => {
                if attrs.size.is_some() && !file.can_write {
                    return Err(StatusCode::PermissionDenied);
                }
                apply_file_attrs(&file.file, &attrs)?;
            }
            HandleEntry::Dir(dir) => apply_attrs(&dir.path, &attrs)?,
        }
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
        fs::rename(oldpath, newpath).map_err(map_io_error)?;
        Ok(self.status(id, StatusCode::Ok))
    }
}

fn apply_attrs(path: &Path, attrs: &FileAttributes) -> Result<(), StatusCode> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(attrs.size.is_some())
        .open(path)
        .map_err(map_io_error)?;
    apply_file_attrs(&file, attrs)
}

fn apply_file_attrs(file: &fs::File, attrs: &FileAttributes) -> Result<(), StatusCode> {
    if let Some(size) = attrs.size {
        file.set_len(size).map_err(map_io_error)?;
    }
    if attrs.atime.is_some() || attrs.mtime.is_some() {
        let mut times = fs::FileTimes::new();
        if let Some(atime) = attrs.atime {
            times = times.set_accessed(UNIX_EPOCH + Duration::from_secs(atime.into()));
        }
        if let Some(mtime) = attrs.mtime {
            times = times.set_modified(UNIX_EPOCH + Duration::from_secs(mtime.into()));
        }
        file.set_times(times).map_err(map_io_error)?;
    }
    #[cfg(unix)]
    if let Some(mode) = attrs.permissions {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))
            .map_err(map_io_error)?;
    }
    // In this experiment WASIp2 mode/ownership changes are accepted as no-ops.
    // File data and timestamps are persisted; POSIX mode preservation is not promised.
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
        keys: vec![if let Ok(path) = std::env::var("SFTP_HOST_KEY") {
            load_or_create_host_key(Path::new(&path))?
        } else {
            russh::keys::PrivateKey::random(
                &mut rand::rng(),
                russh::keys::ssh_key::Algorithm::Ed25519,
            )?
        }],
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

fn load_or_create_host_key(path: &Path) -> anyhow::Result<russh::keys::PrivateKey> {
    match fs::metadata(path) {
        Ok(_) => {
            return russh::keys::load_secret_key(path, None)
                .with_context(|| format!("failed to load SFTP_HOST_KEY at {}", path.display()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let key = russh::keys::PrivateKey::random(
        &mut rand::rng(),
        russh::keys::ssh_key::Algorithm::Ed25519,
    )?;
    let encoded = key.to_openssh(Default::default())?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create SFTP_HOST_KEY at {}", path.display()))?;
    file.write_all(encoded.as_bytes())?;
    file.sync_all()?;
    Ok(key)
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

#[cfg(test)]
mod tests {
    use super::*;
    use russh_sftp::server::Handler;
    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "sftp-review-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn host_key_is_persistent_and_invalid_key_is_not_replaced() {
        let root = TempRoot::new("host-key");
        let path = root.0.join("host_ed25519");
        let first = load_or_create_host_key(&path).unwrap();
        let bytes = fs::read(&path).unwrap();
        let second = load_or_create_host_key(&path).unwrap();
        assert_eq!(first.public_key(), second.public_key());
        assert_eq!(bytes, fs::read(&path).unwrap());
        fs::write(&path, b"invalid key").unwrap();
        assert!(load_or_create_host_key(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"invalid key");
    }

    #[tokio::test]
    async fn append_ignores_write_offset() {
        let root = TempRoot::new("append");
        fs::write(root.0.join("file"), b"original").unwrap();
        let mut session = FsSftpSession::new(root.0.clone());
        let h = session
            .open(
                1,
                "file".into(),
                OpenFlags::WRITE | OpenFlags::APPEND,
                FileAttributes::empty(),
            )
            .await
            .unwrap();
        session
            .write(2, h.handle, 0, b"NEW".to_vec())
            .await
            .unwrap();
        assert_eq!(fs::read(root.0.join("file")).unwrap(), b"originalNEW");
    }

    #[tokio::test]
    async fn open_handle_survives_rename() {
        let root = TempRoot::new("rename");
        fs::write(root.0.join("file"), b"original").unwrap();
        let mut session = FsSftpSession::new(root.0.clone());
        let h = session
            .open(1, "file".into(), OpenFlags::READ, FileAttributes::empty())
            .await
            .unwrap();
        session
            .rename(2, "file".into(), "moved".into())
            .await
            .unwrap();
        fs::write(root.0.join("file"), b"replacement").unwrap();
        let actual = session.read(3, h.handle, 0, 100).await.unwrap();
        assert_eq!(actual.data, b"original");
    }

    #[tokio::test]
    async fn failed_read_open_does_not_create_directories() {
        let root = TempRoot::new("read");
        let mut session = FsSftpSession::new(root.0.clone());
        assert!(
            session
                .open(
                    1,
                    "new/sub/missing".into(),
                    OpenFlags::READ,
                    FileAttributes::empty()
                )
                .await
                .is_err()
        );
        assert!(!root.0.join("new/sub").exists());
    }

    #[tokio::test]
    async fn read_only_handle_cannot_truncate() {
        let root = TempRoot::new("fsetstat");
        fs::write(root.0.join("file"), b"original").unwrap();
        let mut session = FsSftpSession::new(root.0.clone());
        let h = session
            .open(1, "file".into(), OpenFlags::READ, FileAttributes::empty())
            .await
            .unwrap();
        let mut attrs = FileAttributes::empty();
        attrs.size = Some(0);
        assert!(session.fsetstat(2, h.handle, attrs).await.is_err());
        assert_eq!(fs::read(root.0.join("file")).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_setstat_updates_mode() {
        use std::os::unix::fs::PermissionsExt;
        let root = TempRoot::new("chmod");
        let path = root.0.join("file");
        fs::write(&path, b"data").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let mut session = FsSftpSession::new(root.0.clone());
        let mut attrs = FileAttributes::empty();
        attrs.permissions = Some(0o600);
        let response = session.setstat(1, "file".into(), attrs).await.unwrap();
        assert_eq!(response.status_code, StatusCode::Ok);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
