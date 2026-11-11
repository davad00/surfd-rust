use std::{
    fmt,
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpStream},
    path::Path,
    sync::Arc,
};

use rustls::{
    ClientConfig, ClientConnection, Connection, RootCertStore, ServerConfig, ServerConnection,
    crypto::ring::default_provider,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
};

const ROOM_PROTOCOL: &[u8] = b"surfd-room/1";
const TLS_PREFIX: &str = "tls://";

pub fn normalize_address(address: &str) -> Result<String, String> {
    let address = RoomAddress::parse(address)?;
    let scheme = if address.encrypted { TLS_PREFIX } else { "" };
    Ok(format!("{scheme}{}", address.authority))
}

pub fn room_address_uses_tls(address: &str) -> bool {
    address.starts_with(TLS_PREFIX)
}

pub fn room_address_authority(address: &str) -> &str {
    address.strip_prefix(TLS_PREFIX).unwrap_or(address)
}

pub fn room_address_is_loopback(address: &str) -> bool {
    RoomAddress::parse(address).is_ok_and(|address| {
        address.host == "localhost"
            || address
                .host
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

pub(crate) fn lan_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        IpAddr::V6(address) => address.to_ipv4_mapped().map_or_else(
            || {
                address.is_loopback()
                    || address.is_unique_local()
                    || address.is_unicast_link_local()
            },
            |address| lan_address(IpAddr::V4(address)),
        ),
    }
}

pub(super) struct RoomAddress {
    pub(super) authority: String,
    pub(super) host: String,
    pub(super) encrypted: bool,
}

impl RoomAddress {
    pub(super) fn parse(address: &str) -> Result<Self, String> {
        let address = address.trim().to_ascii_lowercase();
        let encrypted = room_address_uses_tls(&address);
        let authority = room_address_authority(&address);
        let invalid = || "Enter a room host, IP address or tls://host:port.".to_owned();
        if authority.is_empty() || authority.len() > 255 {
            return Err(invalid());
        }
        let socket = authority.parse::<SocketAddr>().ok().or_else(|| {
            let bare = authority
                .strip_prefix('[')
                .and_then(|ip| ip.strip_suffix(']'))
                .unwrap_or(authority);
            bare.parse::<IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, super::DEFAULT_PORT))
        });
        if let Some(socket) = socket {
            if socket.port() == 0 {
                return Err(invalid());
            }
            return Ok(Self {
                authority: socket.to_string(),
                host: socket.ip().to_string(),
                encrypted,
            });
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().map_err(|_| invalid())?),
            None => (authority, super::DEFAULT_PORT),
        };
        let host = host.strip_suffix('.').unwrap_or(host);
        if port == 0
            || host.is_empty()
            || host.len() > 253
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.as_bytes()[0].is_ascii_alphanumeric()
                    && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return Err(invalid());
        }
        Ok(Self {
            authority: format!("{host}:{port}"),
            host: host.to_owned(),
            encrypted,
        })
    }
}

#[derive(Clone)]
pub struct RoomTlsConfig(Arc<ServerConfig>);

impl fmt::Debug for RoomTlsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RoomTlsConfig")
    }
}

impl RoomTlsConfig {
    pub fn from_pem(certificate: &Path, key: &Path) -> io::Result<Self> {
        let chain = certificates(certificate)?;
        let key = PrivateKeyDer::from_pem_file(key).map_err(io::Error::other)?;
        Self::from_der(chain, key)
    }

    pub(super) fn from_der(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> io::Result<Self> {
        let mut config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(io::Error::other)?;
        config.alpn_protocols = vec![ROOM_PROTOCOL.to_vec()];
        Ok(Self(Arc::new(config)))
    }
}

fn certificates(path: &Path) -> io::Result<Vec<CertificateDer<'static>>> {
    let certificates = CertificateDer::pem_file_iter(path)
        .map_err(io::Error::other)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    if certificates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the certificate PEM is empty",
        ));
    }
    Ok(certificates)
}

pub(super) struct Transport {
    stream: TcpStream,
    tls: Option<Box<Connection>>,
    written_bytes: u64,
    write_closed: bool,
}

#[derive(Clone)]
pub(super) struct RoomClientTls {
    config: Arc<ClientConfig>,
    name: ServerName<'static>,
}

impl RoomClientTls {
    pub(super) fn for_hostname(host: String) -> io::Result<Self> {
        let name = ServerName::try_from(host).map_err(io::Error::other)?;
        let mut roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        if let Some(path) = std::env::var_os("SURF_ROOM_CA_CERT") {
            for cert in certificates(Path::new(&path))? {
                roots.add(cert).map_err(io::Error::other)?;
            }
        }
        Self::with_roots(name, roots)
    }

    pub(super) fn with_roots(name: ServerName<'static>, roots: RootCertStore) -> io::Result<Self> {
        let mut config = ClientConfig::builder_with_provider(Arc::new(default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![ROOM_PROTOCOL.to_vec()];
        Ok(Self {
            config: Arc::new(config),
            name,
        })
    }

    pub(super) fn connect(&self, stream: TcpStream) -> io::Result<Transport> {
        let connection = ClientConnection::new(Arc::clone(&self.config), self.name.clone())
            .map_err(io::Error::other)?;
        Transport::new(stream, Some(Connection::Client(connection)))
    }
}

impl Transport {
    pub(super) fn plain(stream: TcpStream) -> io::Result<Self> {
        Self::new(stream, None)
    }

    pub(super) fn encrypted_server(stream: TcpStream, config: &RoomTlsConfig) -> io::Result<Self> {
        let connection = ServerConnection::new(config.0.clone()).map_err(io::Error::other)?;
        Self::new(stream, Some(Connection::Server(connection)))
    }

    fn new(stream: TcpStream, tls: Option<Connection>) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let tls = tls.map(|mut tls| {
            tls.set_buffer_limit(Some(super::MAX_PENDING_BYTES));
            Box::new(tls)
        });
        Ok(Self {
            stream,
            tls,
            written_bytes: 0,
            write_closed: false,
        })
    }

    pub(super) fn drained(&self) -> bool {
        self.tls.as_ref().is_none_or(|tls| !tls.wants_write())
    }

    pub(super) fn handshake_ready(&mut self) -> io::Result<bool> {
        match self.flush() {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
        let Some(tls) = &self.tls else {
            return Ok(true);
        };
        if tls.is_handshaking() || tls.wants_write() {
            return Ok(false);
        }
        check_protocol(tls)?;
        Ok(true)
    }

    pub(super) fn written_bytes(&self) -> u64 {
        self.written_bytes
    }

    pub(super) fn close_notify(&mut self) {
        if let Some(tls) = &mut self.tls {
            tls.send_close_notify();
        }
    }

    pub(super) fn drain_shutdown(&mut self) -> io::Result<bool> {
        if !self.write_closed {
            self.stream.shutdown(Shutdown::Write)?;
            self.write_closed = true;
        }
        // Leave the read half open so pending client bytes cannot reset the
        // socket and discard the last room frame or TLS close notification.
        let mut scratch = [0; 4096];
        for _ in 0..8 {
            match self.stream.read(&mut scratch) {
                Ok(0) => return Ok(true),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(false)
    }
}

impl Read for Transport {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let Some(tls) = &mut self.tls else {
            return self.stream.read(buffer);
        };
        let first = tls.reader().read(buffer);
        if first
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
        {
            let (_, written) = tls.complete_io(&mut self.stream)?;
            self.written_bytes = self.written_bytes.wrapping_add(written as u64);
            if tls.is_handshaking() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            check_protocol(tls)?;
            return tls.reader().read(buffer);
        }
        if first.is_ok() && !tls.is_handshaking() {
            check_protocol(tls)?;
        }
        first
    }
}

impl Write for Transport {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(tls) = &mut self.tls else {
            let written = self.stream.write(buffer)?;
            self.written_bytes = self.written_bytes.wrapping_add(written as u64);
            return Ok(written);
        };
        if tls.is_handshaking() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        check_protocol(tls)?;
        let written = tls.writer().write(buffer)?;
        if written == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(tls) = &mut self.tls {
            if tls.is_handshaking() || tls.wants_write() {
                let (_, written) = tls.complete_io(&mut self.stream)?;
                self.written_bytes = self.written_bytes.wrapping_add(written as u64);
            }
            return Ok(());
        }
        self.stream.flush()
    }
}

fn check_protocol(connection: &Connection) -> io::Result<()> {
    if connection.alpn_protocol() == Some(ROOM_PROTOCOL) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "the TLS endpoint is not a Surf room",
    ))
}
